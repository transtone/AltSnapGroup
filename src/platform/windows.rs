use crate::group::{GroupManager, TrackedWindow};
use log::info;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use windows::Win32::System::Threading::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MonitorFromWindow, GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetDpiForMonitor, GetDpiForWindow,
    GetSystemMetricsForDpi, GetWindowDpiAwarenessContext, SetThreadDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, DPI_AWARENESS_CONTEXT_UNAWARE,
    DPI_AWARENESS_PER_MONITOR_AWARE, DPI_AWARENESS_SYSTEM_AWARE, MDT_EFFECTIVE_DPI,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

static AUTO_DETECT: AtomicBool = AtomicBool::new(true);
static DETECT_TX: OnceLock<std::sync::mpsc::Sender<()>> = OnceLock::new();
static GROUPS_CHANGED: AtomicBool = AtomicBool::new(false);
static GROUPS_WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

pub fn set_groups_wake(wake: impl Fn() + Send + Sync + 'static) {
    let _ = GROUPS_WAKE.set(Box::new(wake));
}

pub fn take_groups_changed() -> bool { GROUPS_CHANGED.swap(false, Ordering::SeqCst) }

fn notify_groups_changed() {
    GROUPS_CHANGED.store(true, Ordering::SeqCst);
    if let Some(wake) = GROUPS_WAKE.get() { wake(); }
}

pub fn window_label(handle: isize) -> String {
    let hwnd = HWND(handle as *mut _);
    let title = get_window_title(hwnd);
    if title.is_empty() { format!("{} ({handle})", get_process_name(hwnd)) } else { title }
}

pub fn set_auto_detect(enabled: bool) {
    AUTO_DETECT.store(enabled, Ordering::SeqCst);
    if enabled { request_detection(); }
}

pub fn request_detection() {
    if let Some(tx) = DETECT_TX.get() { let _ = tx.send(()); }
}

fn detect_windows(gm: &Arc<Mutex<GroupManager>>, desktop: &IVirtualDesktopManager) {
    let mut handles = Vec::<isize>::new();
    unsafe extern "system" fn collect(hwnd: HWND, lp: LPARAM) -> BOOL {
        if IsWindowVisible(hwnd).as_bool() && IsWindowArranged(hwnd).as_bool() {
            (*(lp.0 as *mut Vec<isize>)).push(hwnd.0 as isize);
        }
        TRUE
    }
    unsafe { let _ = EnumWindows(Some(collect), LPARAM(&mut handles as *mut _ as isize)); }
    let mut by_desktop = HashMap::<String, Vec<(isize, String, String)>>::new();
    for handle in handles {
        let hwnd = HWND(handle as *mut _);
        if let Ok(id) = unsafe { desktop.GetWindowDesktopId(hwnd) } {
            by_desktop.entry(format!("{id:?}")).or_default().push((handle, String::new(), String::new()));
        }
    }
    if let Ok(mut manager) = gm.lock() {
        for (&handle, group_id) in &manager.active_bindings {
            let hwnd = HWND(handle as *mut _);
            if unsafe { IsWindow(hwnd).as_bool() && IsIconic(hwnd).as_bool() } {
                if let (Some(group), Ok(id)) = (manager.groups.iter().find(|g| &g.id == group_id), unsafe { desktop.GetWindowDesktopId(hwnd) }) {
                    let key = format!("{id:?}");
                    if group.name == key {
                        let members = by_desktop.entry(key).or_default();
                        if !members.iter().any(|item| item.0 == handle) { members.push((handle, String::new(), String::new())); }
                    }
                }
            }
        }
        if replace_detected_groups(&mut manager, by_desktop) { notify_groups_changed(); }
    }
}

fn replace_detected_groups(manager: &mut GroupManager, by_desktop: HashMap<String, Vec<(isize, String, String)>>) -> bool {
        let incoming: HashMap<_, _> = by_desktop.iter().filter(|(_, windows)| windows.len() >= 2)
            .map(|(desktop, windows)| (desktop.clone(), windows.iter().map(|window| window.0).collect::<std::collections::HashSet<_>>())).collect();
        let existing: HashMap<_, _> = manager.groups.iter().map(|group| (group.name.clone(), manager.active_bindings.iter()
            .filter_map(|(&hwnd, id)| (id == &group.id).then_some(hwnd)).collect::<std::collections::HashSet<_>>())).collect();
        if incoming == existing { return false; }
        manager.groups.clear();
        manager.active_bindings.clear();
        for (id, windows) in by_desktop {
            if windows.len() >= 2 {
                manager.create_group_from_hwnds(&id, windows, crate::group::MovementMode::Inherit, true);
            }
        }
        true
}

#[cfg(test)]
mod detection_tests {
    use super::*;
    #[test]
    fn windows_are_grouped_only_within_each_desktop() {
        let window = |h| (h, String::new(), String::new());
        let mut manager = GroupManager::new();
        replace_detected_groups(&mut manager, HashMap::from([
            ("a".into(), vec![window(1), window(2)]),
            ("b".into(), vec![window(3), window(4)]),
            ("c".into(), vec![window(5)]),
        ]));
        assert_eq!(manager.get_sibling_hwnds(1), vec![2]);
        assert_eq!(manager.get_sibling_hwnds(3), vec![4]);
        assert!(manager.get_sibling_hwnds(5).is_empty());
        replace_detected_groups(&mut manager, HashMap::new());
        assert!(manager.active_bindings.is_empty());
    }
}

static GROUP_MANAGER: OnceLock<Arc<Mutex<GroupManager>>> = OnceLock::new();
static SUPPRESSED: AtomicBool = AtomicBool::new(false);
static LAST_FG_SYNC_MS: AtomicIsize = AtomicIsize::new(0);
static START_INSTANT: OnceLock<Instant> = OnceLock::new();
static MOVE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static MOVE_CACHE_DIRTY: AtomicBool = AtomicBool::new(false);
static ALT_TARGET: AtomicIsize = AtomicIsize::new(0);
static ALT_PENDING: AtomicBool = AtomicBool::new(false);
static ALT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static ALT_SESSION: AtomicU64 = AtomicU64::new(0);
static ALT_ENDED: AtomicU64 = AtomicU64::new(0);
static ALT_COMPLETED: AtomicU64 = AtomicU64::new(0);
static ALT_SNAPSHOT: Mutex<Option<(u64, Option<DragSnapshot>)>> = Mutex::new(None);

/// Reserve a session without waiting or calling window APIs from the mouse hook.
/// Zero rejects a gesture until the previous final translation has been flushed.
pub fn alt_move_begin(target: isize) -> u64 {
    if target == 0 || ALT_PENDING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() { return 0; }
    let id = ALT_SEQUENCE.fetch_add(1, Ordering::SeqCst) + 1;
    ALT_TARGET.store(target, Ordering::SeqCst);
    ALT_SESSION.store(id, Ordering::SeqCst);
    id
}

/// Worker-only: capture the reference BEFORE its first geometry operation.
/// The hook must never call this function; it queries foreign windows.
pub fn alt_move_start(target: isize, session: u64) {
    if ALT_SESSION.load(Ordering::SeqCst) != session || ALT_TARGET.load(Ordering::SeqCst) != target { return; }
    let hwnd = HWND(target as *mut _);
    let _awareness = ThreadAwareness::for_window(hwnd);
    let members = GROUP_MANAGER.get().and_then(|gm| movement_members(gm, target));
    let snapshot = members.and_then(|members| DragSnapshot::capture(hwnd, members, DragGesture::Move));
    *ALT_SNAPSHOT.lock().expect("ALT snapshot poisoned") = Some((session, snapshot));
}

pub fn alt_move_update(_: isize) {}

/// Latch input release; worker completion may arrive before or after this call.
pub fn alt_move_end(target: isize, session: u64) {
    if ALT_SESSION.load(Ordering::SeqCst) == session && ALT_TARGET.load(Ordering::SeqCst) == target {
        ALT_ENDED.fetch_max(session, Ordering::SeqCst);
    }
}

/// Worker-only: signal after its last geometry operation, including failure/cancel.
/// A failed worker begin calls end and complete without start: no rect was applied.
pub fn alt_move_complete(target: isize, session: u64) {
    if ALT_SESSION.load(Ordering::SeqCst) == session && ALT_TARGET.load(Ordering::SeqCst) == target {
        ALT_COMPLETED.fetch_max(session, Ordering::SeqCst);
    }
}

/// Includes worker completion through final flush; safe to call from the hook.
pub fn alt_move_pending() -> bool { ALT_PENDING.load(Ordering::SeqCst) }

// Restore awareness on every exit, including failed window reads and group changes.
struct ThreadAwareness(Option<DPI_AWARENESS_CONTEXT>);
impl ThreadAwareness {
    fn for_window(hwnd: HWND) -> Self {
        Self(virtualized_awareness(hwnd).and_then(|context| {
            // SAFETY: the context comes from Windows and is used only on this thread.
            let previous = unsafe { SetThreadDpiAwarenessContext(context) };
            (!previous.0.is_null()).then_some(previous)
        }))
    }
}
impl Drop for ThreadAwareness {
    fn drop(&mut self) { restore_thread_awareness(&mut self.0); }
}

// Copy membership while locked, then release it before any foreign window call.
fn movement_members(gm: &Mutex<GroupManager>, target: isize) -> Option<Vec<isize>> {
    let manager = gm.lock().ok()?;
    let group = manager.find_group_for_hwnd(target)?;
    manager.movement_enabled(group).then(|| manager.get_sibling_hwnds(target))
}

struct DragSnapshot {
    target: isize,
    members: Vec<isize>,
    gesture: DragGesture,
    start: (i32, i32),
    positions: HashMap<isize, (i32, i32)>,
    last: RECT,
    dpi: u32,
}
impl DragSnapshot {
    fn capture(hwnd: HWND, mut members: Vec<isize>, gesture: DragGesture) -> Option<Self> {
        let rect = get_window_rect(hwnd)?;
        members.sort_unstable();
        Some(Self {
            target: hwnd.0 as isize,
            positions: capture_sibling_positions(&members),
            members,
            gesture,
            start: (rect.left, rect.top),
            last: rect,
            dpi: monitor_dpi(hwnd),
        })
    }

    fn update(&mut self, gm: &Mutex<GroupManager>) {
        let hwnd = HWND(self.target as *mut _);
        // SAFETY: window validity is checked before querying or moving it.
        if unsafe { !IsWindow(hwnd).as_bool() || IsIconic(hwnd).as_bool() } { return; }
        let Some(mut members) = movement_members(gm, self.target) else {
            self.positions.clear();
            return;
        };
        let Some(rect) = get_window_rect(hwnd) else { return; };
        members.sort_unstable();
        if members != self.members {
            // A new member must not inherit movement from before it joined.
            self.members = members;
            self.start = (rect.left, rect.top);
            self.positions = capture_sibling_positions(&self.members);
            self.last = rect;
            self.dpi = monitor_dpi(hwnd);
            return;
        }
        let dpi = monitor_dpi(hwnd);
        let layout_jump = is_layout_jump(&self.last, &rect, self.dpi, dpi);
        self.last = rect;
        self.dpi = dpi;
        if self.gesture == DragGesture::Resize { return; }
        if layout_jump {
            self.start = (rect.left, rect.top);
            self.positions = capture_sibling_positions(&self.members);
            return;
        }
        translate_siblings(gm, self.target, &self.positions, self.start, (rect.left, rect.top));
    }
}

fn is_layout_jump(before: &RECT, after: &RECT, old_dpi: u32, dpi: u32) -> bool {
    rect_size(before) != rect_size(after)
        && old_dpi == dpi
        && !looks_like_dpi_scale(rect_size(before), rect_size(after))
}

fn session_ready(session: u64, ended: u64, completed: u64) -> bool {
    session != 0 && ended == session && completed == session
}

fn move_sync_loop(gm: Arc<Mutex<GroupManager>>) {
    let mut native: Option<DragSnapshot> = None;
    let mut alt: Option<(u64, Option<DragSnapshot>)> = None;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(8));
        if alt_move_pending() {
            native = None;
            let session = ALT_SESSION.load(Ordering::SeqCst);
            if session == 0 { continue; }
            // Read completion BEFORE taking the reference and querying geometry.
            // Completion publishes the worker's final rect and initial snapshot.
            let ready = session_ready(session, ALT_ENDED.load(Ordering::SeqCst), ALT_COMPLETED.load(Ordering::SeqCst));
            if alt.as_ref().is_none_or(|(id, _)| *id != session) {
                alt = ALT_SNAPSHOT.lock().expect("ALT snapshot poisoned").take();
            }
            if let Some((id, snapshot)) = &mut alt {
                if *id != session { continue; }
                let _awareness = ThreadAwareness::for_window(HWND(ALT_TARGET.load(Ordering::SeqCst) as *mut _));
                if let Some(snapshot) = snapshot { snapshot.update(&gm); }
            } else if !ready {
                continue;
            }
            // No snapshot plus both signals is a cancelled worker begin: no
            // geometry was applied, so there is no final movement to flush.
            if ready {
                alt = None;
                ALT_SESSION.store(0, Ordering::SeqCst);
                ALT_TARGET.store(0, Ordering::SeqCst);
                ALT_PENDING.store(false, Ordering::SeqCst);
            }
            continue;
        }
        if MOVE_CACHE_DIRTY.swap(false, Ordering::SeqCst) { native = None; }
        if SUPPRESSED.load(Ordering::SeqCst) { continue; }
        if !lbutton_down() {
            if let Some(mut snapshot) = native.take() {
                let _awareness = ThreadAwareness::for_window(HWND(snapshot.target as *mut _));
                snapshot.update(&gm);
            }
            continue;
        }
        // SAFETY: this reads the foreground handle without dereferencing it.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() { native = None; continue; }
        let _awareness = ThreadAwareness::for_window(hwnd);
        let target = hwnd.0 as isize;
        let Some(members) = movement_members(&gm, target) else { native = None; continue; };
        if native.as_ref().is_none_or(|snapshot| snapshot.target != target) {
            native = get_window_rect(hwnd).and_then(|rect| {
                DragSnapshot::capture(hwnd, members, drag_gesture_at(hwnd, rect))
            });
            continue;
        }
        if let Some(snapshot) = &mut native { snapshot.update(&gm); }
    }
}

/// Which part of the window frame the pointer grabbed when the drag began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragGesture {
    /// Title-bar or client drag: positional deltas translate the group.
    Move,
    /// Edge or corner resize: position changes are layout, not group translation.
    Resize,
}

fn get_window_rect(hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok().map(|_| rect)
    }
}

fn lbutton_down() -> bool {
    unsafe { (GetAsyncKeyState(0x01 /* VK_LBUTTON */) as u16 & 0x8000) != 0 }
}

fn rect_size(rect: &RECT) -> (i32, i32) {
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// Classifies the pointer against the frame metrics of the window. The caption
/// strip always counts as a move: crossing a monitor with a different DPI resizes
/// the dragged window mid-drag, and that size change must not stop the follow.
fn gesture_for_pointer(pointer: POINT, rect: RECT, frame: i32, caption: i32) -> DragGesture {
    let frame = frame.max(1);
    let caption = caption.max(1);
    let on_caption_x = pointer.x >= rect.left + frame && pointer.x < rect.right - frame;
    let on_caption_y = pointer.y >= rect.top + frame && pointer.y < rect.top + frame + caption;
    if on_caption_x && on_caption_y {
        return DragGesture::Move;
    }
    let grip = frame + 3;
    let near_window = pointer.x >= rect.left - 2
        && pointer.x <= rect.right + 2
        && pointer.y >= rect.top - 2
        && pointer.y <= rect.bottom + 2;
    let on_edge = pointer.x <= rect.left + grip
        || pointer.x >= rect.right - grip
        || pointer.y <= rect.top + grip
        || pointer.y >= rect.bottom - grip;
    if near_window && on_edge {
        DragGesture::Resize
    } else {
        DragGesture::Move
    }
}

fn frame_metrics(hwnd: HWND) -> (i32, i32) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let frame = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
            + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        let caption = GetSystemMetricsForDpi(SM_CYCAPTION, dpi);
        (frame.max(1), caption.max(1))
    }
}

fn drag_gesture_at(hwnd: HWND, rect: RECT) -> DragGesture {
    let mut pointer = POINT::default();
    if unsafe { GetCursorPos(&mut pointer) }.is_err() {
        return DragGesture::Move;
    }
    let (frame, caption) = frame_metrics(hwnd);
    gesture_for_pointer(pointer, rect, frame, caption)
}

/// DPI of the monitor under the window, independent of the window's own DPI
/// awareness. System-aware windows keep reporting their system DPI from
/// GetDpiForWindow, but the monitor DPI still changes when they cross monitors.
fn monitor_dpi(hwnd: HWND) -> u32 {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() && dpi_x > 0 {
            return dpi_x;
        }
        GetDpiForWindow(hwnd).max(96)
    }
}

/// A size change that matches a common monitor DPI ratio is a DPI rescale, not a
/// resize/restore jump. Windows rescales system-aware and unaware windows itself
/// while they cross a monitor with a different DPI.
fn looks_like_dpi_scale(old: (i32, i32), new: (i32, i32)) -> bool {
    if old.0 <= 0 || old.1 <= 0 || new.0 <= 0 || new.1 <= 0 {
        return false;
    }
    let rx = new.0 as f32 / old.0 as f32;
    let ry = new.1 as f32 / old.1 as f32;
    if (rx - ry).abs() > 0.04 {
        return false;
    }
    const RATIOS: [f32; 14] = [
        1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0, 0.8, 0.6667, 0.5714, 0.5, 0.4444, 0.4, 0.3333,
    ];
    RATIOS.iter().any(|ratio| (rx - ratio).abs() <= 0.045)
}

/// System-aware and unaware windows are DPI-virtualized by Windows: their
/// coordinates are reported in a virtual space that changes when they cross a
/// monitor with a different DPI. Switch the sync thread to the window's own
/// awareness for the duration of a drag so GetWindowRect and SetWindowPos stay
/// in one consistent coordinate space for the whole group.
fn virtualized_awareness(hwnd: HWND) -> Option<DPI_AWARENESS_CONTEXT> {
    unsafe {
        let context = GetWindowDpiAwarenessContext(hwnd);
        match GetAwarenessFromDpiAwarenessContext(context) {
            DPI_AWARENESS_PER_MONITOR_AWARE => None,
            DPI_AWARENESS_SYSTEM_AWARE => Some(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE),
            _ => Some(DPI_AWARENESS_CONTEXT_UNAWARE),
        }
    }
}

fn restore_thread_awareness(context: &mut Option<DPI_AWARENESS_CONTEXT>) {
    if let Some(previous) = context.take() {
        unsafe {
            SetThreadDpiAwarenessContext(previous);
        }
    }
}

fn capture_sibling_positions(siblings: &[isize]) -> HashMap<isize, (i32, i32)> {
    let mut positions = HashMap::new();
    for &sh in siblings {
        let swh = HWND(sh as *mut _);
        if unsafe { !IsWindow(swh).as_bool() || IsIconic(swh).as_bool() } {
            continue;
        }
        if let Some(rect) = get_window_rect(swh) {
            positions.insert(sh, (rect.left, rect.top));
        }
    }
    positions
}

/// Targets are always derived from the drag reference snapshot plus the
/// foreground window's total displacement. Tests pin this: a missed poll must
/// never accumulate permanent drift.
fn translated_targets(
    start_positions: &HashMap<isize, (i32, i32)>,
    fg_start: (i32, i32),
    fg_now: (i32, i32),
) -> Vec<(isize, i32, i32)> {
    let dx = fg_now.0 - fg_start.0;
    let dy = fg_now.1 - fg_start.1;
    start_positions
        .iter()
        .map(|(&hwnd, &(x, y))| (hwnd, x + dx, y + dy))
        .collect()
}

fn translate_siblings(
    gm: &Mutex<GroupManager>,
    target: isize,
    start_positions: &HashMap<isize, (i32, i32)>,
    fg_start: (i32, i32),
    fg_now: (i32, i32),
) {
    // Apply even when the total translation is zero: the foreground window may have
    // returned to its starting point while a follower is still at a stale target from
    // the previous poll. Re-applying the drag-reference targets is what makes the
    // group converge exactly.
    MOVE_IN_PROGRESS.store(true, Ordering::SeqCst);
    unsafe {
        for (sh, target_x, target_y) in translated_targets(start_positions, fg_start, fg_now) {
            // Membership may change while another application handles SetWindowPos.
            // Never hold the manager lock across a foreign window API call.
            if !movement_members(gm, target).is_some_and(|members| members.contains(&sh)) { continue; }
            let swh = HWND(sh as *mut _);
            if !IsWindow(swh).as_bool() || IsIconic(swh).as_bool() {
                continue;
            }
            // Synchronized movement is rigid: preserve the group's relative layout
            // even when a follower would end up partly off-screen. The existing
            // "recover off-screen windows" command remains available.
            let (tx, ty) = (target_x, target_y);
            let _ = SetWindowPos(
                swh,
                HWND(std::ptr::null_mut()),
                tx,
                ty,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    MOVE_IN_PROGRESS.store(false, Ordering::SeqCst);
}


const FG_DEBOUNCE_MS: isize = 150;

fn elapsed_ms() -> isize {
    START_INSTANT.get().map(|s| s.elapsed().as_millis() as isize).unwrap_or(0)
}

fn get_process_name(hwnd: HWND) -> String {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 { return String::new(); }
        if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut buf = [0u16; 512];
            let mut size = buf.len() as u32;
            let r = QueryFullProcessImageNameW(handle, PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr()), &mut size);
            let _ = CloseHandle(handle);
            if r.is_ok() {
                let path = String::from_utf16_lossy(&buf[..size as usize]);
                return path.rsplit('\\').next().unwrap_or("").to_string();
            }
        }
        String::new()
    }
}
fn get_window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 { return String::new(); }
        let mut buf = vec![0u16; (len + 1) as usize];
        GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string()
    }
}
fn get_class_name(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf);
        if len == 0 { return String::new(); }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}
fn is_real_window(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() { return false; }
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        if ex.contains(WS_EX_TOOLWINDOW) { return false; }
        if get_window_title(hwnd).is_empty() { return false; }
        let mut cloaked: u32 = 0;
        let r = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _, std::mem::size_of::<u32>() as u32);
        if r.is_ok() && cloaked != 0 { return false; }
        true
    }
}
/// Keep a useful portion of the title bar inside a monitor's work area.
/// Negative monitor coordinates are valid; use monitor bounds, not screen zero.
fn reachable_position(hwnd: HWND, x: i32, y: i32, width: i32) -> (i32, i32) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let title_height = 32 * dpi / 96;
        let title = RECT { left: x, top: y, right: x + width, bottom: y + title_height };
        let monitor = MonitorFromRect(&title, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() { return (x, y); }
        crate::window_geometry::reachable_position(x, y, width, 96 * dpi / 96, title_height,
            (info.rcWork.left, info.rcWork.top, info.rcWork.right, info.rcWork.bottom))
    }
}

pub fn recover_windows(hwnds: &[isize]) -> usize {
    let mut recovered = 0;
    unsafe {
        for &handle in hwnds {
            let hwnd = HWND(handle as *mut _);
            if !IsWindow(hwnd).as_bool() || IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() { continue; }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() { continue; }
            let (x, y) = reachable_position(hwnd, rect.left, rect.top, rect.right - rect.left);
            if (x, y) != (rect.left, rect.top) && SetWindowPos(hwnd, None, x, y, 0, 0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE).is_ok() {
                recovered += 1;
            }
        }
    }
    if recovered > 0 { MOVE_CACHE_DIRTY.store(true, Ordering::SeqCst); }
    recovered
}
fn get_zorder_sorted(group_hwnds: &[isize]) -> Vec<isize> {
    unsafe {
        let mut sorted = Vec::new();
        let mut cur = GetWindow(GetForegroundWindow(), GW_HWNDFIRST).unwrap_or_default();
        while !cur.0.is_null() {
            let h = cur.0 as isize;
            if group_hwnds.contains(&h) { sorted.push(h); }
            if sorted.len() == group_hwnds.len() { break; }
            cur = GetWindow(cur, GW_HWNDNEXT).unwrap_or_default();
        }
        sorted
    }
}

pub fn enumerate_windows() -> Vec<TrackedWindow> {
    let mut w = Vec::new();
    unsafe { let _ = EnumWindows(Some(enum_cb), LPARAM(&mut w as *mut _ as isize)); }
    w
}
unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    if !is_real_window(hwnd) { return TRUE; }
    let v = &mut *(lp.0 as *mut Vec<TrackedWindow>);
    v.push(TrackedWindow { hwnd: hwnd.0 as isize,
        process_name: get_process_name(hwnd), title: get_window_title(hwnd),
        class_name: get_class_name(hwnd) });
    TRUE
}

fn bring_group_to_front(activated_hwnd: isize, all_group_hwnds: &[isize]) {
    unsafe {
        SUPPRESSED.store(true, Ordering::SeqCst);

        let mut zorder = get_zorder_sorted(all_group_hwnds);
        zorder.retain(|&h| !IsIconic(HWND(h as *mut _)).as_bool());
        zorder.retain(|&h| h != activated_hwnd);
        zorder.insert(0, activated_hwnd);

        for &h in &zorder {
            let wh = HWND(h as *mut _);
            if !IsWindow(wh).as_bool() { continue; }
            if IsIconic(wh).as_bool() { continue; }
            let _ = ShowWindow(wh, SW_SHOWNA);
        }

        for &h in zorder.iter().rev() {
            let wh = HWND(h as *mut _);
            if !IsWindow(wh).as_bool() { continue; }
            let _ = SetWindowPos(wh, HWND_TOP, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        for i in 1..zorder.len() {
            let above = HWND(zorder[i - 1] as *mut _);
            let wh = HWND(zorder[i] as *mut _);
            let _ = SetWindowPos(wh, above, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }

        let _ = SetForegroundWindow(HWND(activated_hwnd as *mut _));

        MOVE_CACHE_DIRTY.store(true, Ordering::SeqCst);
        SUPPRESSED.store(false, Ordering::SeqCst);
        LAST_FG_SYNC_MS.store(elapsed_ms(), Ordering::SeqCst);
    }
}

unsafe extern "system" fn win_event_callback(
    _hook: HWINEVENTHOOK, event: u32, hwnd: HWND,
    id_object: i32, _id_child: i32, _thread_id: u32, _timestamp: u32,
) {
    if id_object != 0 || hwnd.0.is_null() { return; }
    if SUPPRESSED.load(Ordering::SeqCst) || MOVE_IN_PROGRESS.load(Ordering::SeqCst) { return; }

    if AUTO_DETECT.load(Ordering::SeqCst) && matches!(event, 0x0003 | 0x000B | 0x0016 | 0x0017 | 0x8001 | 0x800B) {
        request_detection();
    }


    let hv = hwnd.0 as isize;
    let Some(gm_arc) = GROUP_MANAGER.get() else { return; };

    match event {
        0x0003 => {
            let now = elapsed_ms();
            if now - LAST_FG_SYNC_MS.load(Ordering::SeqCst) < FG_DEBOUNCE_MS { return; }
            let gm = match gm_arc.try_lock() { Ok(g) => g, Err(_) => return };
            // Groups hold only the exact windows the user lassoed; no auto-binding by process
            // name, so other windows of the same app stay independent.
            let sibs = gm.get_sibling_hwnds(hv);

            if sibs.is_empty() { return; }
            let Ok(desktop) = CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL) else { return; };
            let Ok(active_desktop) = desktop.GetWindowDesktopId(hwnd) else { return; };
            if !desktop.IsWindowOnCurrentVirtualDesktop(hwnd).map(|v| v.as_bool()).unwrap_or(false) { return; }
            let mut all: Vec<_> = sibs.into_iter().filter(|&h| {
                let other = HWND(h as *mut _);
                desktop.GetWindowDesktopId(other).ok() == Some(active_desktop)
                    && desktop.IsWindowOnCurrentVirtualDesktop(other).map(|v| v.as_bool()).unwrap_or(false)
            }).collect();
            if all.is_empty() { return; }
            all.push(hv);
            info!("FG sync: {} windows", all.len());
            drop(gm);
            bring_group_to_front(hv, &all);
        }

        0x8001 => {
            if let Ok(mut gm) = gm_arc.try_lock() { gm.unbind_window(hv); }
        }
        0x0016 | 0x0017 => {
            let gm = match gm_arc.try_lock() { Ok(g) => g, Err(_) => return };
            if !gm.global_sync_move { return; }
            let siblings = gm.get_sibling_hwnds(hv);
            if siblings.is_empty() { return; }
            let Ok(desktop) = CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL) else { return; };
            let Ok(id) = desktop.GetWindowDesktopId(hwnd) else { return; };
            if !desktop.IsWindowOnCurrentVirtualDesktop(hwnd).map(|v| v.as_bool()).unwrap_or(false) { return; }
            drop(gm);
            SUPPRESSED.store(true, Ordering::SeqCst);
            for h in siblings {
                let other = HWND(h as *mut _);
                if !IsWindow(other).as_bool() || desktop.GetWindowDesktopId(other).ok() != Some(id) { continue; }
                if event == 0x0016 && !IsIconic(other).as_bool() { let _ = ShowWindow(other, SW_MINIMIZE); }
                if event == 0x0017 && IsIconic(other).as_bool() { let _ = ShowWindow(other, SW_RESTORE); }
            }
            SUPPRESSED.store(false, Ordering::SeqCst);
        }
        _ => {}
    }
}

pub fn start_monitor(gm: Arc<Mutex<GroupManager>>) -> Result<(), Box<dyn std::error::Error>> {
    GROUP_MANAGER.set(gm.clone()).map_err(|_| "init")?;
    START_INSTANT.set(Instant::now()).map_err(|_| "init")?;

    let move_manager = gm.clone();
    std::thread::spawn(move || move_sync_loop(move_manager));

    let (tx, rx) = std::sync::mpsc::channel();
    let _ = DETECT_TX.set(tx);
    std::thread::spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() { log::error!("COM initialization failed"); return; }
        let desktop: windows::core::Result<IVirtualDesktopManager> = CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL);
        if let Ok(desktop) = desktop {
            while rx.recv().is_ok() {
                while rx.recv_timeout(std::time::Duration::from_millis(150)).is_ok() {}
                detect_windows(&gm, &desktop);
            }
        } else { log::error!("Virtual desktop manager unavailable: {:?}", desktop.err()); }
        CoUninitialize();
    });
    if AUTO_DETECT.load(Ordering::SeqCst) { request_detection(); }

    info!("Starting event monitor...");
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND,
            None, Some(win_event_callback), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        SetWinEventHook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND,
            None, Some(win_event_callback), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY,
            None, Some(win_event_callback), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        SetWinEventHook(EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND,
            None, Some(win_event_callback), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE,
            None, Some(win_event_callback), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);

        info!("Hooks: fg + min/max + destroy | Move: poll thread");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

#[cfg(test)]
mod drag_sync_tests {
    use super::*;
    use std::collections::HashMap;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    fn point(x: i32, y: i32) -> POINT {
        POINT { x, y }
    }

    #[test]
    fn caption_strip_is_a_move_even_next_to_the_top_resize_border() {
        let window = rect(100, 100, 900, 600);
        // Three pixels below the frame is still caption: a DPI-change resize in
        // the middle of a title-bar drag must not be classified as an edge resize.
        assert_eq!(
            gesture_for_pointer(point(500, 111), window, 8, 23),
            DragGesture::Move
        );
    }

    #[test]
    fn edges_and_corners_are_resizes() {
        let window = rect(100, 100, 900, 600);
        for edge in [
            point(101, 300),
            point(899, 300),
            point(500, 101),
            point(500, 599),
            point(101, 101),
            point(899, 599),
        ] {
            assert_eq!(
                gesture_for_pointer(edge, window, 8, 23),
                DragGesture::Resize
            );
        }
    }

    #[test]
    fn client_area_drag_counts_as_a_move() {
        let window = rect(100, 100, 900, 600);
        assert_eq!(
            gesture_for_pointer(point(500, 300), window, 8, 23),
            DragGesture::Move
        );
    }

    #[test]
    fn translation_targets_come_from_the_drag_reference_and_current_position() {
        let mut starts = HashMap::new();
        starts.insert(2_isize, (500, 300));
        starts.insert(3_isize, (900, 300));
        let reference = (100, 100);

        let mut after_first_tick = translated_targets(&starts, reference, (160, 100));
        after_first_tick.sort();
        assert_eq!(after_first_tick, vec![(2, 560, 300), (3, 960, 300)]);

        // Even if an intermediate poll is missed, the next tick derives targets
        // from the reference snapshot, so no movement is lost.
        let mut after_skipped_tick = translated_targets(&starts, reference, (220, 120));
        after_skipped_tick.sort();
        assert_eq!(after_skipped_tick, vec![(2, 620, 320), (3, 1020, 320)]);
    }

    #[test]
    fn dpi_rescale_ratios_are_recognised() {
        // 100% -> 125% virtualized window and back.
        assert!(looks_like_dpi_scale((720, 644), (576, 515)));
        assert!(looks_like_dpi_scale((576, 515), (720, 644)));
        // A restore/maximize jump is not a DPI ratio.
        assert!(!looks_like_dpi_scale((1920, 1080), (720, 644)));
        assert!(!looks_like_dpi_scale((720, 644), (1000, 500)));
    }

    #[test]
    fn returning_to_the_drag_reference_restores_the_original_targets() {
        let mut starts = HashMap::new();
        starts.insert(2_isize, (500, 300));
        let back = translated_targets(&starts, (100, 100), (100, 100));
        assert_eq!(back, vec![(2, 500, 300)]);
    }

    #[test]
    fn worker_completion_and_release_are_both_required_in_either_order() {
        assert!(!session_ready(7, 0, 0));
        assert!(!session_ready(7, 7, 0));
        assert!(!session_ready(7, 0, 7));
        assert!(session_ready(7, 7, 7));
        assert!(!session_ready(8, 7, 7));
        assert!(!session_ready(0, 0, 0));
    }

    #[test]
    fn old_completion_cannot_overwrite_a_new_session_signal() {
        let completed = AtomicU64::new(8);
        completed.fetch_max(7, Ordering::SeqCst);
        assert!(session_ready(8, 8, completed.load(Ordering::SeqCst)));
    }

    #[test]
    fn dpi_transition_does_not_rebase_but_layout_change_does() {
        let before = rect(100, 100, 820, 744);
        let scaled = rect(200, 100, 776, 615);
        assert!(!is_layout_jump(&before, &scaled, 96, 120));
        assert!(!is_layout_jump(&before, &scaled, 96, 96));
        let maximized = rect(0, 0, 1920, 1080);
        assert!(is_layout_jump(&before, &maximized, 96, 96));
    }

    #[test]
    fn rigid_translation_allows_offscreen_and_large_negative_coordinates() {
        let starts = HashMap::from([(2, (-15000, -500))]);
        assert_eq!(translated_targets(&starts, (100, 100), (-100, 50)), vec![(2, -15200, -550)]);
    }
}
