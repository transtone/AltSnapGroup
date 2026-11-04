use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn wait_for_previous() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--wait-for-instance") { return true; }
    let Some(pid) = args.next().and_then(|s| s.parse::<u32>().ok()) else { return false; };
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(process) => {
                let result = WaitForSingleObject(process, 30000);
                let _ = CloseHandle(process);
                result == WAIT_OBJECT_0
            }
            Err(_) => true,
        }
    }
}

pub fn restart_elevated() -> windows::core::Result<()> {
    let executable = std::env::current_exe().map_err(|_| windows::core::Error::from_win32())?;
    let exe: Vec<u16> = executable.as_os_str().encode_wide().chain(Some(0)).collect();
    let args: Vec<u16> = format!("--wait-for-instance {}", std::process::id()).encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(args.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        ShellExecuteExW(&mut info)?;
        if !info.hProcess.is_invalid() { let _ = CloseHandle(info.hProcess); }
    }
    Ok(())
}

use std::os::windows::ffi::OsStrExt;