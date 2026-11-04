/// Persistent settings: custom hotkeys + language
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HotkeyConfig {
    pub modifiers: String,   // e.g. "Ctrl+Alt"
    pub key: String,         // e.g. "G"
}

impl HotkeyConfig {
    pub fn new(modifiers: &str, key: &str) -> Self {
        Self { modifiers: modifiers.to_string(), key: key.to_string() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub move_together: bool,
    #[serde(default = "default_auto_detect")]
    pub auto_detect: bool,
    #[serde(default)]
    pub alt: Option<crate::snap_config::FileConfig>,
    pub lang: String,  // "en" or "zh" or "auto"
    pub hotkey_bind: HotkeyConfig,
    pub hotkey_unbind_cursor: HotkeyConfig,
    pub hotkey_unbind_all: HotkeyConfig,
    #[serde(default = "default_hotkey_sync_move")]
    pub hotkey_sync_move: HotkeyConfig,
    pub sync_move: bool,
    pub sync_minimize: bool,
    pub auto_start: bool,
}

fn default_auto_detect() -> bool { true }

fn default_hotkey_sync_move() -> HotkeyConfig {
    HotkeyConfig::new("Ctrl+Alt", "M")
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            move_together: false,
            auto_detect: true,
            alt: None,
            lang: "auto".to_string(),
            hotkey_bind: HotkeyConfig::new("Ctrl+Alt", "G"),
            hotkey_unbind_cursor: HotkeyConfig::new("Ctrl+Alt", "D"),
            hotkey_unbind_all: HotkeyConfig::new("Ctrl+Alt", "U"),
            hotkey_sync_move: default_hotkey_sync_move(),
            sync_move: false,
            sync_minimize: true,
            auto_start: false,
        }
    }
}

pub fn settings_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AlwaysBindWindow");
    let _ = fs::create_dir_all(&dir);
    dir.join("settings.json")
}

pub fn load() -> Settings {
    let path = settings_path();
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(s) = serde_json::from_str::<Settings>(&content) {
            return s;
        }
        log::error!("Invalid settings file; preserving existing file");
        return Settings::default();
    }
    // Return default and save it
    let s = Settings::default();
    let _ = save(&s);
    s
}

pub fn load_alt() -> Result<crate::snap_config::FileConfig, Box<dyn std::error::Error>> {
    load_alt_from(&settings_path())
}

fn load_alt_from(path: &std::path::Path) -> Result<crate::snap_config::FileConfig, Box<dyn std::error::Error>> {
    let mut settings: Settings = serde_json::from_str(&fs::read_to_string(&path)?)?;
    if let Some(alt) = settings.alt { return Ok(alt); }
    let alt = crate::snap_config::FileConfig::default();
    alt.to_runtime_config()?;
    settings.alt = Some(alt.clone());
    save_to(path, &settings)?;
    Ok(alt)
}

pub fn save(settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    save_to(&settings_path(), settings)
}

fn save_to(path: &std::path::Path, settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    let mut updated = settings.clone();
    if path.exists() {
        let current: Settings = serde_json::from_str(&fs::read_to_string(&path)?)?;
        updated.alt = current.alt.or(updated.alt);
    }
    let content = serde_json::to_string_pretty(&updated)?;
    let temporary = path.with_file_name(format!("settings-{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, content)?;
    #[cfg(windows)]
    let result = {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { MoveFileExW(windows::core::PCWSTR(from.as_ptr()), windows::core::PCWSTR(to.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    };
    #[cfg(not(windows))]
    let result = fs::rename(&temporary, &path).map_err(|e| Box::new(e) as Box<dyn std::error::Error>);
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result?;
    Ok(())
}

/// Parse modifier string like "Ctrl+Alt" into global_hotkey Modifiers
pub fn parse_modifiers(s: &str) -> global_hotkey::hotkey::Modifiers {
    use global_hotkey::hotkey::Modifiers;
    let mut m = Modifiers::empty();
    let upper = s.to_uppercase();
    if upper.contains("CTRL") || upper.contains("CONTROL") { m |= Modifiers::CONTROL; }
    if upper.contains("ALT") { m |= Modifiers::ALT; }
    if upper.contains("SHIFT") { m |= Modifiers::SHIFT; }
    if upper.contains("SUPER") || upper.contains("WIN") || upper.contains("META") { m |= Modifiers::SUPER; }
    m
}

/// Parse key string like "G" into global_hotkey Code
pub fn parse_key(s: &str) -> Option<global_hotkey::hotkey::Code> {
    use global_hotkey::hotkey::Code;
    match s.to_uppercase().as_str() {
        "A" => Some(Code::KeyA), "B" => Some(Code::KeyB), "C" => Some(Code::KeyC),
        "D" => Some(Code::KeyD), "E" => Some(Code::KeyE), "F" => Some(Code::KeyF),
        "G" => Some(Code::KeyG), "H" => Some(Code::KeyH), "I" => Some(Code::KeyI),
        "J" => Some(Code::KeyJ), "K" => Some(Code::KeyK), "L" => Some(Code::KeyL),
        "M" => Some(Code::KeyM), "N" => Some(Code::KeyN), "O" => Some(Code::KeyO),
        "P" => Some(Code::KeyP), "Q" => Some(Code::KeyQ), "R" => Some(Code::KeyR),
        "S" => Some(Code::KeyS), "T" => Some(Code::KeyT), "U" => Some(Code::KeyU),
        "V" => Some(Code::KeyV), "W" => Some(Code::KeyW), "X" => Some(Code::KeyX),
        "Y" => Some(Code::KeyY), "Z" => Some(Code::KeyZ),
        "0" | ")" => Some(Code::Digit0), "1" | "!" => Some(Code::Digit1),
        "2" | "@" => Some(Code::Digit2), "3" | "#" => Some(Code::Digit3),
        "4" | "$" => Some(Code::Digit4), "5" | "%" => Some(Code::Digit5),
        "6" | "^" => Some(Code::Digit6), "7" | "&" => Some(Code::Digit7),
        "8" | "*" => Some(Code::Digit8), "9" | "(" => Some(Code::Digit9),
        "F1" => Some(Code::F1), "F2" => Some(Code::F2), "F3" => Some(Code::F3),
        "F4" => Some(Code::F4), "F5" => Some(Code::F5), "F6" => Some(Code::F6),
        "F7" => Some(Code::F7), "F8" => Some(Code::F8), "F9" => Some(Code::F9),
        "F10" => Some(Code::F10), "F11" => Some(Code::F11), "F12" => Some(Code::F12),
        _ => None,
    }
}

/// Build a HotKey from config
pub fn build_hotkey(cfg: &HotkeyConfig) -> Option<global_hotkey::hotkey::HotKey> {
    use global_hotkey::hotkey::HotKey;
    let mods = parse_modifiers(&cfg.modifiers);
    let key = parse_key(&cfg.key)?;
    Some(HotKey::new(Some(mods), key))
}

/// Reject unsupported keys and duplicate combinations before saving the dialog.
pub fn valid_hotkeys(configs: &[&HotkeyConfig]) -> bool {
    let mut ids = std::collections::HashSet::new();
    configs.iter().all(|cfg| build_hotkey(cfg).map(|hk| ids.insert(hk.id())).unwrap_or(false))
}

/// Format hotkey for display: "Ctrl+Alt+G"
pub fn format_hotkey(cfg: &HotkeyConfig) -> String {
    format!("{}+{}", cfg.modifiers, cfg.key)
}

// ── Auto-start ────────────────────────────────────────────────

const APP_NAME: &str = "AlwaysBindWindow";

/// Set or remove auto-start on login
pub fn set_auto_start(enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    {
        set_auto_start_windows(enable)
    }
    #[cfg(target_os = "macos")]
    {
        set_auto_start_macos(enable)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err("Auto-start not supported on this platform".into())
    }
}

#[cfg(target_os = "windows")]
fn set_auto_start_windows(enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    let exe_path = std::env::current_exe()?.to_string_lossy().to_string();

    if enable {
        // Add to HKCU\Software\Microsoft\Windows\CurrentVersion\Run
        Command::new("reg")
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v", APP_NAME,
                "/t", "REG_SZ",
                "/d", &exe_path,
                "/f"])
            .output()?;
    } else {
        // Remove from registry
        let _ = Command::new("reg")
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v", APP_NAME,
                "/f"])
            .output();
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_auto_start_macos(enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    let plist_dir = dirs::home_dir()
        .ok_or("No home dir")?
        .join("Library/LaunchAgents");
    let plist_path = plist_dir.join("com.alwaysbindwindow.plist");

    if enable {
        let exe_path = std::env::current_exe()?.to_string_lossy().to_string();
        let plist = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.alwaysbindwindow</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>"#, exe_path);
        let _ = fs::create_dir_all(&plist_dir);
        fs::write(&plist_path, plist)?;
    } else {
        let _ = fs::remove_file(&plist_path);
    }
    Ok(())
}

/// Check if auto-start is currently enabled
#[allow(dead_code)]
pub fn is_auto_start_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        std::process::Command::new("reg")
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v", APP_NAME])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .map(|h| h.join("Library/LaunchAgents/com.alwaysbindwindow.plist").exists())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    { false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_defaults_and_preserves_existing_settings() {
        let dir = std::env::temp_dir().join(format!("abw-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut original = Settings::default();
        original.lang = "zh".into();
        fs::write(&path, serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(load_alt_from(&path).unwrap().activation.modifier, "alt");
        let mut edited: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        edited.alt.as_mut().unwrap().activation.modifier = "ctrl".into();
        fs::write(&path, serde_json::to_string(&edited).unwrap()).unwrap();
        assert_eq!(load_alt_from(&path).unwrap().activation.modifier, "ctrl");
        original.lang = "en".into();
        save_to(&path, &original).unwrap();
        let updated: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated.lang, "en");
        assert_eq!(updated.alt.unwrap().activation.modifier, "ctrl");
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn invalid_settings_are_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("abw-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("settings.json");
        let original = "invalid json";
        fs::write(&path, original).unwrap();
        assert!(load_alt_from(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn old_settings_gain_movement_hotkey_without_losing_preferences() {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old.as_object_mut().unwrap().remove("hotkey_sync_move");
        old["lang"] = serde_json::json!("zh");
        old["sync_move"] = serde_json::json!(false);
        old["auto_start"] = serde_json::json!(true);
        old["hotkey_bind"] = serde_json::json!({"modifiers": "Ctrl+Shift", "key": "F8"});
        let loaded: Settings = serde_json::from_value(old).unwrap();
        assert_eq!(format_hotkey(&loaded.hotkey_sync_move), "Ctrl+Alt+M");
        assert_eq!(format_hotkey(&loaded.hotkey_bind), "Ctrl+Shift+F8");
        assert_eq!(loaded.lang, "zh");
        assert!(!loaded.sync_move);
        assert!(loaded.auto_start);
    }

    #[test]
    fn duplicate_or_unsupported_hotkeys_are_rejected() {
        let normal = HotkeyConfig::new("Ctrl+Alt", "G");
        let duplicate = HotkeyConfig::new("Alt+Ctrl", "g");
        let movement = default_hotkey_sync_move();
        let invalid = HotkeyConfig::new("Ctrl", "Unknown");
        assert!(valid_hotkeys(&[&normal, &movement]));
        assert!(!valid_hotkeys(&[&normal, &duplicate]));
        assert!(!valid_hotkeys(&[&normal, &invalid]));
    }

    #[test]
    fn custom_movement_hotkey_survives_serialization() {
        let mut settings = Settings::default();
        settings.hotkey_sync_move = HotkeyConfig::new("Ctrl+Shift", "F9");
        let loaded: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(build_hotkey(&loaded.hotkey_sync_move), build_hotkey(&settings.hotkey_sync_move));
        assert_eq!(format_hotkey(&loaded.hotkey_sync_move), "Ctrl+Shift+F9");
    }
}
