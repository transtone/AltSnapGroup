#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod group;
#[cfg(any(windows, test))] mod right_button_latch;

mod engine;
mod snap_config;
#[cfg(windows)] mod snap_hook;
#[cfg(windows)] mod elevate;
#[cfg(windows)] mod snap_fullscreen;
#[cfg(windows)] mod snap_adapter;
#[cfg(windows)] mod resize_worker;
#[cfg(windows)] mod snap_tool_window;
#[cfg(windows)] mod snap_overlay;
#[cfg(windows)] mod monitors;
#[cfg(windows)] mod win_api;
#[cfg(windows)] #[allow(dead_code)] mod win_api_trait;
#[cfg(windows)] mod swallow_latch;
mod platform;
mod tray;
mod overlay;
mod picker;
mod i18n;
mod settings;
mod hotkey_dialog;
mod window_geometry;
mod window_selection;
#[cfg(target_os = "windows")]
mod native_ui;

use log::{info, error};
use std::sync::{Arc, Mutex};
use group::GroupManager;

#[cfg(target_os = "windows")]
struct InstanceGuard(windows::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe { let _ = windows::Win32::Foundation::CloseHandle(self.0); }
    }
}

#[cfg(target_os = "windows")]
fn acquire_instance() -> windows::core::Result<Option<InstanceGuard>> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        let handle = CreateMutexW(None, false, windows::core::w!("Local\\AlwaysBindWindow.Instance"))?;
        let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
        let guard = InstanceGuard(handle);
        if already_running { Ok(None) } else { Ok(Some(guard)) }
    }
}

fn show_startup_error(message: &str) {
    error!("{}", message);
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let _ = MessageBoxW(None, windows::core::PCWSTR(message.as_ptr()),
            windows::core::w!("AlwaysBindWindow"), MB_OK | MB_ICONERROR);
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    #[cfg(windows)]
    if !elevate::wait_for_previous() {
        show_startup_error("The previous instance did not exit.");
        return;
    }

    #[cfg(target_os = "windows")]
    let _instance = match acquire_instance() {
        Ok(Some(guard)) => guard,
        Ok(None) => return,
        Err(error) => {
            show_startup_error(&format!("Could not initialize AlwaysBindWindow: {}", error));
            return;
        }
    };

    // Load settings
    let s = settings::load();

    // Set language
    match s.lang.as_str() {
        "zh" => i18n::set_lang(i18n::Lang::Zh),
        "en" => i18n::set_lang(i18n::Lang::En),
        _ => i18n::set_lang(i18n::detect_system_lang()),
    }

    info!("AlwaysBindWindow v{} starting...", env!("CARGO_PKG_VERSION"));
    info!("Hotkeys: {} = Bind, {} = Unbind Group, {} = Unbind All",
        settings::format_hotkey(&s.hotkey_bind),
        settings::format_hotkey(&s.hotkey_unbind_cursor),
        settings::format_hotkey(&s.hotkey_unbind_all));

    #[cfg(windows)]
    let (snap_ready_tx, snap_ready_rx) = std::sync::mpsc::sync_channel(1);
    #[cfg(windows)]
    let snap_thread = std::thread::spawn(move || {
        match settings::load_alt().and_then(|c| Ok(c.to_runtime_config()?)) {
            Ok(config) => {
                snap_hook::ENGINE.with(|e| { e.borrow_mut().set_config(config.engine); });
                snap_overlay::set_opacity(config.adapter.preview_opacity);
            }
            Err(e) => log::error!("Snap configuration: {}", e),
        }
        if let Err(e) = snap_tool_window::create() {
            log::error!("Snap window: {}", e);
            return;
        }
        unsafe {
            if let Ok(module) = windows::Win32::System::LibraryLoader::GetModuleHandleW(None) {
                snap_overlay::register_class(module.into());
            }
        }
        if let Err(e) = snap_hook::install() {
            log::error!("Snap hooks: {}", e);
            return;
        }
        snap_fullscreen::install();
        let _ = snap_ready_tx.send(());
        snap_tool_window::run_pump();
        snap_hook::ENGINE.with(|engine| {
            let mut engine = engine.borrow_mut();
            if !matches!(engine.state(), crate::engine::state::State::Disabled) {
                let actions = engine.handle(crate::engine::state::Event::ToggleEnable);
                drop(engine);
                snap_adapter::apply_actions(&actions);
            }
        });
        snap_fullscreen::uninstall();
        snap_hook::uninstall();
    });
    #[cfg(windows)]
    if snap_ready_rx.recv().is_err() {
        log::error!("Alt window controls failed to start");
    }

    let group_manager = Arc::new(Mutex::new(GroupManager::new()));

    let gm_clone = Arc::clone(&group_manager);
    std::thread::spawn(move || {
        if let Err(e) = platform::start_monitor(gm_clone) {
            error!("Monitor error: {}", e);
        }
    });

    if let Err(e) = tray::run_tray(group_manager, s) {
        show_startup_error(&format!("{}\n{}", i18n::t("msg.startup_error"), e));
    }
    #[cfg(windows)]
    {
        unsafe {
            let hwnd = snap_tool_window::hwnd();
            if !hwnd.0.is_null() {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    hwnd, windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
            }
        }
        let _ = snap_thread.join();
    }
    // The monitor runs indefinitely. Joining it after a tray startup failure
    // leaves an invisible process alive, potentially still holding hotkeys.
}
