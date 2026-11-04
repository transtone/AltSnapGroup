//! Thin, stateless Win32 wrappers used by hook.rs and the tray.

#![cfg(target_os = "windows")]

use std::mem::size_of;

use crate::engine::geometry::{Point as GPoint, Rect as GRect};
use crate::engine::rules::{WindowInfo, WindowTraits};
use crate::engine::state::WindowId;

use windows::core::PWSTR;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetAncestor, GetClassNameW, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsZoomed, SetForegroundWindow,
    SetWindowPos, ShowWindow, WindowFromPoint, GA_ROOT, GWL_EXSTYLE, HWND_TOP, SWP_NOACTIVATE,
    SWP_NOSENDCHANGING, SWP_NOZORDER, SW_MAXIMIZE, SW_RESTORE, WINDOW_EX_STYLE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
};

pub fn to_win_point(p: GPoint) -> POINT {
    POINT { x: p.x, y: p.y }
}
pub fn from_win_rect(r: RECT) -> GRect {
    GRect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

pub unsafe fn window_under_cursor(cursor: GPoint) -> Option<WindowInfo> {
    let mut hwnd = WindowFromPoint(to_win_point(cursor));
    if hwnd.0.is_null() {
        return None;
    }
    hwnd = GetAncestor(hwnd, GA_ROOT);
    if hwnd.0.is_null() {
        return None;
    }
    collect_window_info(hwnd)
}

pub unsafe fn collect_window_info(hwnd: HWND) -> Option<WindowInfo> {
    let class = read_class_name(hwnd)?;
    if SKIP_CLASSES.iter().any(|c| *c == class) {
        return None;
    }
    let title = read_window_text(hwnd);
    let process = read_process_basename(hwnd)?;
    let traits = read_window_traits(hwnd);
    Some(WindowInfo {
        process_basename: process.to_lowercase(),
        class_name: class,
        title,
        traits,
    })
}

const SKIP_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Button", // desktop's "Show Desktop" button
];

unsafe fn read_class_name(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    let n = GetClassNameW(hwnd, &mut buf);
    if n <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

unsafe fn read_window_text(hwnd: HWND) -> String {
    let n = GetWindowTextLengthW(hwnd);
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (n + 1) as usize];
    let got = GetWindowTextW(hwnd, &mut buf);
    String::from_utf16_lossy(&buf[..got as usize])
}

unsafe fn read_process_basename(hwnd: HWND) -> Option<String> {
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return None;
    }
    let proc_handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut size: u32 = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(
        proc_handle,
        PROCESS_NAME_FORMAT(0),
        PWSTR(buf.as_mut_ptr()),
        &mut size,
    )
    .is_ok();
    let _ = windows::Win32::Foundation::CloseHandle(proc_handle);
    if !ok {
        return None;
    }
    let full = String::from_utf16_lossy(&buf[..size as usize]);
    Some(
        std::path::Path::new(&full)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(full),
    )
}

unsafe fn read_window_traits(hwnd: HWND) -> WindowTraits {
    let ex_style = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
    let cloaked: u32 = {
        let mut v: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut v as *mut _ as *mut _,
            size_of::<u32>() as u32,
        );
        v
    };
    WindowTraits {
        is_topmost: (ex_style.0 & WS_EX_TOPMOST.0) != 0,
        is_cloaked: cloaked != 0,
        is_tool: (ex_style.0 & WS_EX_TOOLWINDOW.0) != 0,
        is_owned: false, // populate later if needed
    }
}

pub unsafe fn hwnd_rect(hwnd: HWND) -> Option<GRect> {
    let mut r = RECT::default();
    GetWindowRect(hwnd, &mut r).ok()?;
    Some(from_win_rect(r))
}

pub unsafe fn set_window_rect(hwnd: HWND, rect: GRect) -> bool {
    SetWindowPos(
        hwnd,
        HWND_TOP,
        rect.left,
        rect.top,
        rect.width(),
        rect.height(),
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSENDCHANGING,
    )
    .is_ok()
}



pub unsafe fn resize_window_rect(hwnd: HWND, rect: GRect) -> bool {
    use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_UPDATENOW, RDW_ALLCHILDREN};
    if SetWindowPos(hwnd, HWND_TOP, rect.left, rect.top, rect.width(), rect.height(),
        SWP_NOZORDER | SWP_NOACTIVATE).is_err() {
        return false;
    }
    // Flush only existing invalid regions; invalidating the whole client stalls live resize.
    if !RedrawWindow(hwnd, None, None, RDW_UPDATENOW | RDW_ALLCHILDREN).as_bool() {
        log::warn!("Could not repaint the resized window");
    }
    true
}

#[cfg(test)]
mod resize_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
    use windows::Win32::UI::WindowsAndMessaging::*;
    static PAINTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_WINDOWPOSCHANGING {
            let pos = &mut *(lp.0 as *mut WINDOWPOS);
            if !pos.flags.contains(SWP_NOSIZE) { pos.cx = pos.cx.max(100); }
        }
        if msg == WM_PAINT {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            PAINTS.fetch_add(1, SeqCst);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
    #[test]
    fn resize_honors_target_layout_and_flushes_pending_paint() {
        unsafe {
            let name = windows::core::w!("AlwaysBindResizeTest");
            let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
            let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance.into(),
                lpszClassName: name, ..Default::default() };
            assert_ne!(RegisterClassW(&class), 0);
            let hwnd = CreateWindowExW(WS_EX_NOACTIVATE, name, name, WS_POPUP | WS_VISIBLE,
                0, 0, 120, 80, None, None, instance, None).unwrap();
            let child = CreateWindowExW(WS_EX_NOACTIVATE, name, name, WS_CHILD | WS_VISIBLE,
                0, 0, 100, 30, hwnd, None, instance, None).unwrap();
            assert!(resize_window_rect(hwnd, GRect { left: 0, top: 0, right: 90, bottom: 90 }));
            let actual = hwnd_rect(hwnd).unwrap();
            let painted = PAINTS.load(SeqCst);
            assert!(resize_window_rect(hwnd, actual));
            let idle = PAINTS.load(SeqCst);
            assert!(resize_window_rect(hwnd, actual));
            let after = PAINTS.load(SeqCst);
            let _ = child;
            DestroyWindow(hwnd).unwrap();
            UnregisterClassW(name, instance).unwrap();
            assert_eq!(actual.width(), 100, "target must receive WM_WINDOWPOSCHANGING");
            assert!(painted >= 2, "parent and child must paint before resize returns");
            assert_eq!(idle, after, "unchanged geometry must not invalidate the entire client");
        }
    }
}

pub unsafe fn is_zoomed(hwnd: HWND) -> bool {
    IsZoomed(hwnd).as_bool()
}

pub unsafe fn restore(hwnd: HWND) {
    let _ = ShowWindow(hwnd, SW_RESTORE);
}

pub unsafe fn maximize(hwnd: HWND) {
    let _ = ShowWindow(hwnd, SW_MAXIMIZE);
}

pub unsafe fn raise(hwnd: HWND) {
    let _ = BringWindowToTop(hwnd);
    let _ = SetForegroundWindow(hwnd);
}

pub unsafe fn cancel_menu_activation() {
    // Send a harmless key (VK_F18 = 0x87) up-down to swallow Alt-triggered menu focus.
    keybd_event(0x87, 0, Default::default(), 0);
    keybd_event(
        0x87,
        0,
        windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_KEYUP,
        0,
    );
}

pub fn hwnd_to_id(hwnd: HWND) -> WindowId {
    WindowId(hwnd.0 as usize as u64)
}
pub fn id_to_hwnd(id: WindowId) -> HWND {
    HWND(id.0 as usize as *mut core::ffi::c_void)
}

pub unsafe fn window_under_cursor_hwnd(cursor: GPoint) -> Option<HWND> {
    let mut hwnd = WindowFromPoint(to_win_point(cursor));
    if hwnd.0.is_null() {
        return None;
    }
    hwnd = GetAncestor(hwnd, GA_ROOT);
    if hwnd.0.is_null() {
        return None;
    }
    Some(hwnd)
}

pub unsafe fn capture_mouse(tool_hwnd: HWND) {
    let _ = SetCapture(tool_hwnd);
}
pub unsafe fn release_mouse() {
    let _ = ReleaseCapture();
}
