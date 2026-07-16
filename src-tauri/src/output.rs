use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Deliver text to the focused app.
/// Prefer paste (clipboard + ⌘V) — more reliable than character typing on macOS.
pub fn deliver_text(text: &str, mode: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    match mode {
        "clipboard" => copy_to_clipboard(text),
        "type" => type_text(text).or_else(|e| {
            log::warn!("[output] type failed ({e}), trying paste");
            paste_text(text)
        }),
        // default: paste (Handy-style)
        _ => paste_text(text).or_else(|e| {
            log::warn!("[output] paste failed ({e}), trying type");
            type_text(text).or_else(|_| {
                copy_to_clipboard(text)?;
                Err(format!(
                    "Could not inject text (Accessibility). Copied to clipboard instead. Enable “{}” in System Settings → Privacy & Security → Accessibility, then fully quit and relaunch.",
                    process_display_name()
                ))
            })
        }),
    }
}

/// Types text character-by-character via Accessibility APIs.
pub fn type_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| format!("Keyboard init failed: {e}"))?;
    enigo
        .text(text)
        .map_err(|e| format!("Type failed: {e}"))
}

/// Copies to clipboard then simulates ⌘V (most reliable injection method).
pub fn paste_text(text: &str) -> Result<(), String> {
    copy_to_clipboard(text)?;
    // Brief delay so the pasteboard is ready before keystroke
    thread::sleep(Duration::from_millis(40));

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| format!("Keyboard init failed: {e}"))?;

    // ⌘V
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|e| format!("Meta press failed: {e}"))?;
    enigo
        .key(Key::Unicode('v'), Direction::Click)
        .map_err(|e| format!("V key failed: {e}"))?;
    enigo
        .key(Key::Meta, Direction::Release)
        .map_err(|e| format!("Meta release failed: {e}"))?;

    Ok(())
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut ctx = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    ctx.set_text(text)
        .map_err(|e| format!("Clipboard failed: {e}"))
}

fn resolved_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("openvoice"));
    std::fs::canonicalize(&exe).unwrap_or(exe)
}

pub fn executable_path() -> String {
    resolved_exe().display().to_string()
}

pub fn process_display_name() -> String {
    resolved_exe()
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "openvoice".into())
}

pub fn is_dev_binary() -> bool {
    let s = resolved_exe().to_string_lossy().to_string();
    s.contains("/target/debug/")
        || s.contains("/target/release/")
        || !s.contains(".app/Contents/MacOS/")
}

#[cfg(target_os = "macos")]
pub fn is_accessibility_trusted() -> bool {
    macos_accessibility_client::accessibility::application_is_trusted()
}

#[cfg(not(target_os = "macos"))]
pub fn is_accessibility_trusted() -> bool {
    true
}

/// Prompt + open Settings. Call only from main/UI thread.
#[cfg(target_os = "macos")]
pub fn request_accessibility_permission() -> bool {
    let trusted =
        macos_accessibility_client::accessibility::application_is_trusted_with_prompt();
    let _ = open_accessibility_settings();
    trusted
}

#[cfg(not(target_os = "macos"))]
pub fn request_accessibility_permission() -> bool {
    true
}

pub fn reveal_executable_in_finder() -> Result<(), String> {
    let path = resolved_exe();
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path.display().to_string()])
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Ok(())
    }
}

pub fn open_accessibility_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let urls = [
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        ];
        for url in urls {
            if std::process::Command::new("open").arg(url).status().is_ok() {
                return Ok(());
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

pub fn open_microphone_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone")
            .status();
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

pub fn has_input_device() -> bool {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host()
        .default_input_device()
        .map(|d| d.name().is_ok())
        .unwrap_or(false)
}

/// List available microphone names.
pub fn list_input_devices() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    host.input_devices()
        .map(|devs| {
            devs.filter_map(|d| d.name().ok())
                .collect()
        })
        .unwrap_or_default()
}

pub fn default_input_device_name() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
}
