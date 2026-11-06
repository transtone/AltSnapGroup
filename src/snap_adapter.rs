#![cfg(target_os = "windows")]

use crate::snap_hook::ENGINE;
use crate::win_api;
use crate::engine::geometry::{Point, Rect};
use crate::engine::rules::{evaluate, RuleAction};
use crate::engine::state::{Action, DragAbortReason, DragMode, DragTarget, Event, WindowId};
use crate::swallow_latch::SwallowLatch;
use crate::win_api_trait::WinApi;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static LAST_BALLOON_EPOCH_SECS: AtomicU64 = AtomicU64::new(0);
static DRAG_STATE: AtomicBool = AtomicBool::new(false);


static MOVE_HWND: AtomicU64 = AtomicU64::new(0);

static SWALLOW_LATCH: std::sync::OnceLock<SwallowLatch> = std::sync::OnceLock::new();

pub fn swallow_latch() -> &'static SwallowLatch {
    SWALLOW_LATCH.get_or_init(SwallowLatch::new)
}

/// Returns true if a window drag is currently in progress.
pub fn drag_active() -> bool {
    DRAG_STATE.load(Ordering::Relaxed)
}

fn maybe_balloon_uipi() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST_BALLOON_EPOCH_SECS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < 60 {
        return;
    }
    LAST_BALLOON_EPOCH_SECS.store(now, Ordering::Relaxed);
    log::warn!("Can't move this window while running unelevated");
}

pub unsafe fn resolve_target(cursor: Point) -> Option<DragTarget> {
    // A finishing resize must settle before reading the next gesture's initial rectangle.
    if crate::resize_worker::finishing() { return None; }
    let info = win_api::window_under_cursor(cursor)?;
    let hwnd_raw = win_api::window_under_cursor_hwnd(cursor)?;

    let exclude = ENGINE.with(|e| {
        let eng = e.borrow();
        matches!(
            evaluate(&eng.config().rules, &info),
            Some(RuleAction::Exclude)
        )
    });

    let initial_rect = win_api::hwnd_rect(hwnd_raw)?;
    let is_maximized = win_api::is_zoomed(hwnd_raw);

    Some(DragTarget {
        hwnd: win_api::hwnd_to_id(hwnd_raw),
        initial_rect,
        is_maximized,
        exclude,
        monitor_snapshot: Some(crate::monitors::current()),
    })
}

pub unsafe fn resolve_drag_target(cursor: Point) -> Option<DragTarget> {
    let target = resolve_target(cursor)?;
    let hwnd = win_api::id_to_hwnd(target.hwnd);
    let arranged = windows::Win32::UI::WindowsAndMessaging::IsWindowArranged(hwnd).as_bool();
    filter_drag_target(target, arranged)
}

fn filter_drag_target(target: DragTarget, arranged: bool) -> Option<DragTarget> {
    (!target.is_maximized && !arranged).then_some(target)
}

#[cfg(test)]
mod drag_target_tests {
    use super::*;

    #[test]
    fn maximized_and_arranged_windows_are_not_drag_targets() {
        let target = DragTarget {
            hwnd: WindowId(1), initial_rect: Rect { left: 0, top: 0, right: 100, bottom: 100 },
            is_maximized: false, exclude: false, monitor_snapshot: None,
        };
        assert!(filter_drag_target(target.clone(), false).is_some());
        assert!(filter_drag_target(target.clone(), true).is_none());
        let mut maximized = target;
        maximized.is_maximized = true;
        assert!(filter_drag_target(maximized.clone(), false).is_none());
        assert!(filter_drag_target(maximized, true).is_none());
    }
}

/// Outcome returned by `apply_actions`.
///
/// `swallow` is fed back to the Windows low-level hook as the "suppress this
/// event" flag.  `re_entry` holds any `Event`s that the adapter wants to inject
/// back into the engine (e.g. `DragAborted` after a hard `ApplySnapRect`
/// failure); callers must drive those through `Engine::handle` themselves.
pub struct AdapterOutcome {
    pub swallow: bool,
    pub re_entry: Vec<Event>,
}

pub fn apply_actions(actions: &[Action]) -> AdapterOutcome {
    // Spec §3.5: clear the latch defensively before any BeginDrag in this batch.
    if actions
        .iter()
        .any(|a| matches!(a, Action::BeginDrag { .. }))
    {
        swallow_latch().on_begin_drag();
    }

    let mut outcome = AdapterOutcome {
        swallow: false,
        re_entry: Vec::new(),
    };
    for a in actions {
        match a {
            Action::BeginDrag { hwnd, mode, initial_rect, .. } => unsafe {
                let resizing = matches!(mode, DragMode::Resize { .. });
                if !crate::resize_worker::begin(*hwnd, *initial_rect, resizing) {
                    outcome.re_entry.push(Event::DragAborted { reason: DragAbortReason::ApplyGeometryFailed });
                    break;
                }
                DRAG_STATE.store(true, Ordering::Relaxed);

                if *mode == DragMode::Move {
                    MOVE_HWND.store(hwnd.0, Ordering::Relaxed);
                    crate::platform::windows::alt_move_begin(hwnd.0 as isize);
                }
                win_api::capture_mouse(crate::snap_tool_window::hwnd());
            },
            Action::UpdateDrag { hwnd, new_rect } => {
                let ok = crate::resize_worker::update(*new_rect);

                if ok && MOVE_HWND.load(Ordering::Relaxed) == hwnd.0 {
                    crate::platform::windows::alt_move_update(hwnd.0 as isize);
                }
                if !ok {
                    maybe_balloon_uipi();
                    outcome.re_entry.push(Event::DragAborted {
                        reason: DragAbortReason::ApplyGeometryFailed,
                    });
                    // Release/capture cleanup may follow this update in the same batch.
                    continue;
                }
            },
            Action::EndDrag { hwnd } => unsafe {
                crate::resize_worker::end();
                if MOVE_HWND.swap(0, Ordering::Relaxed) == hwnd.0 {
                    crate::platform::windows::alt_move_end(hwnd.0 as isize);
                }
                DRAG_STATE.store(false, Ordering::Relaxed);

                crate::monitors::on_drag_ended();
                win_api::release_mouse();
            },
            Action::RestoreIfMaximized { hwnd, .. } => unsafe {
                win_api::restore(win_api::id_to_hwnd(*hwnd));
            },
            Action::RaiseWindow { hwnd } => unsafe {
                win_api::raise(win_api::id_to_hwnd(*hwnd));
            },
            Action::CancelMenuActivation => unsafe {
                win_api::cancel_menu_activation();
            },
            Action::SwallowEvent => {
                outcome.swallow = true;
            }
            Action::UpdateTrayIcon { .. } => {}
            Action::ToggleMaximize { hwnd } => unsafe {
                let h = win_api::id_to_hwnd(*hwnd);
                if win_api::is_zoomed(h) {
                    win_api::restore(h);
                } else {
                    win_api::maximize(h);
                }
                swallow_latch().set(now_ms());
            },
            Action::ShowSnapPreview { rect } => {
                // Lazy-fetch the module handle inline — same pattern as tray.rs.
                unsafe {
                    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
                    if let Ok(hmod) = GetModuleHandleW(None) {
                        let hinstance: windows::Win32::Foundation::HINSTANCE = hmod.into();
                        crate::snap_overlay::show(hinstance, *rect);
                    }
                }
            }
            Action::HideSnapPreview => {
                crate::snap_overlay::hide();
            }
            Action::ApplySnapRect { hwnd, rect } => {
                if crate::resize_worker::update(*rect) {
                    if MOVE_HWND.load(Ordering::Relaxed) == hwnd.0 {
                        crate::platform::windows::alt_move_update(hwnd.0 as isize);
                    }
                } else {
                    // EndDrag in this batch must still release capture and the group session.
                    maybe_balloon_uipi();
                }
            }
        }
    }
    outcome
}

fn now_ms() -> u64 {
    // GetTickCount64 is monotonic, unaffected by system clock adjustment.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

// ---- Win32 real WinApi impl ----

/// Concrete `WinApi` implementation that calls the real Win32 wrappers and the
/// overlay.  Lives here (binary crate) rather than in `win_api_trait` because
/// `win_api` and `overlay` are declared in `main.rs`'s module tree.
#[allow(dead_code)]
pub struct Win32WinApi {
    pub hinstance: windows::Win32::Foundation::HINSTANCE,
}

impl WinApi for Win32WinApi {
    #[allow(clippy::result_unit_err)]
    fn set_window_rect(&mut self, hwnd: WindowId, rect: Rect) -> Result<(), ()> {
        let h = win_api::id_to_hwnd(hwnd);
        if unsafe { win_api::set_window_rect(h, rect) } {
            Ok(())
        } else {
            Err(())
        }
    }
    fn is_zoomed(&mut self, hwnd: WindowId) -> bool {
        let h = win_api::id_to_hwnd(hwnd);
        unsafe { win_api::is_zoomed(h) }
    }
    fn show_maximize(&mut self, hwnd: WindowId) {
        let h = win_api::id_to_hwnd(hwnd);
        unsafe { win_api::maximize(h) };
    }
    fn show_restore(&mut self, hwnd: WindowId) {
        let h = win_api::id_to_hwnd(hwnd);
        unsafe { win_api::restore(h) };
    }
    fn capture_mouse(&mut self, hwnd: WindowId) {
        let h = win_api::id_to_hwnd(hwnd);
        unsafe { win_api::capture_mouse(h) };
    }
    fn release_mouse(&mut self) {
        unsafe { win_api::release_mouse() };
    }
    fn overlay_show(&mut self, rect: Rect) {
        crate::snap_overlay::show(self.hinstance, rect);
    }
    fn overlay_hide(&mut self) {
        crate::snap_overlay::hide();
    }
}
