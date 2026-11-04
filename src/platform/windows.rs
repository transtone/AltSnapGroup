use crate::group::{GroupManager, TrackedWindow};
use log::info;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use windows::Win32::System::Threading::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{MonitorFromRect, GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
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
                manager.create_group_from_hwnds(&id, windows, crate::group::MovementMode::Off, false);
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
pub fn alt_move_begin(_: isize) {}
pub fn alt_move_update(_: isize) {}
pub fn alt_move_end(_: isize) {}

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
        _ => {}
    }
}

pub fn start_monitor(gm: Arc<Mutex<GroupManager>>) -> Result<(), Box<dyn std::error::Error>> {
    GROUP_MANAGER.set(gm.clone()).map_err(|_| "init")?;
    START_INSTANT.set(Instant::now()).map_err(|_| "init")?;

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
