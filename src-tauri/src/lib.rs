// lib.rs — OpenVoice Tauri backend entry point.

mod audio;
mod model_manager;
mod output;
mod speech;

use audio::AudioRecorder;
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

// ─── App State ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub id: u64,
    pub text: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub model: String,
    /// "paste" | "type" | "clipboard"
    pub output_mode: String,
    pub hotkey: String,
    /// "ptt" | "toggle"
    pub recording_mode: String,
    #[serde(default)]
    pub onboarding_complete: bool,
    /// Append a trailing space after each transcript
    #[serde(default = "default_true")]
    pub append_trailing_space: bool,
    /// Start with settings window hidden
    #[serde(default)]
    pub start_hidden: bool,
    /// Show floating HUD while recording
    #[serde(default = "default_true")]
    pub show_overlay: bool,
    /// Keep last N history items
    #[serde(default = "default_history_limit")]
    pub history_limit: u32,
    /// Preferred microphone device name (empty = system default)
    #[serde(default)]
    pub microphone: String,
    /// Play a short system sound when recording starts/stops
    #[serde(default)]
    pub audio_feedback: bool,
    /// Transcription language: "auto" lets Whisper detect it, or an ISO 639-1
    /// code (e.g. "en", "es") to force decoding in that language.
    #[serde(default = "default_language")]
    pub language: String,
    /// RMS level below which audio is treated as silence and skipped.
    #[serde(default = "default_silence_threshold")]
    pub silence_threshold: f32,
    /// "system" | "light" | "dark"
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_true() -> bool {
    true
}
fn default_history_limit() -> u32 {
    50
}
fn default_language() -> String {
    "auto".into()
}
fn default_silence_threshold() -> f32 {
    0.005
}
fn default_theme() -> String {
    "system".into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            model: "ggml-tiny.en.bin".into(),
            // paste = clipboard + ⌘V (most reliable on macOS)
            output_mode: "paste".into(),
            hotkey: "CommandOrControl+Shift+Space".into(),
            recording_mode: "ptt".into(),
            onboarding_complete: false,
            append_trailing_space: true,
            start_hidden: false,
            show_overlay: true,
            history_limit: 50,
            microphone: String::new(),
            audio_feedback: false,
            language: default_language(),
            silence_threshold: default_silence_threshold(),
            theme: default_theme(),
        }
    }
}

fn load_settings() -> AppSettings {
    let path = model_manager::settings_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => AppSettings::default(),
    }
}

fn persist_settings(settings: &AppSettings) -> Result<(), String> {
    model_manager::ensure_app_data_dir().map_err(|e| e.to_string())?;
    let path = model_manager::settings_path();
    let raw = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, raw).map_err(|e| e.to_string())
}

pub struct AppState {
    pub recorder: Mutex<AudioRecorder>,
    pub is_recording: Mutex<bool>,
    pub transcript_history: Mutex<VecDeque<TranscriptEntry>>,
    pub next_id: Mutex<u64>,
    pub settings: Mutex<AppSettings>,
    pub temp_wav_path: PathBuf,
    pub download_cancel: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}

impl AppState {
    fn new() -> Self {
        let settings = load_settings();
        let temp_wav_path = std::env::temp_dir().join("openvoice_recording.wav");
        Self {
            recorder: Mutex::new(AudioRecorder::new()),
            is_recording: Mutex::new(false),
            transcript_history: Mutex::new(VecDeque::with_capacity(100)),
            next_id: Mutex::new(1),
            settings: Mutex::new(settings),
            temp_wav_path,
            download_cancel: Mutex::new(None),
        }
    }
}

// ─── Tauri Commands ──────────────────────────────────────────────────────────

#[tauri::command]
fn start_recording(state: State<'_, AppState>) -> Result<(), String> {
    let mut is_recording = state.is_recording.lock();
    if *is_recording {
        return Err("Already recording".into());
    }

    let settings = state.settings.lock().clone();
    if !model_manager::is_model_downloaded(&settings.model) {
        return Err(format!(
            "No model ready. Open Settings and download “{}” first.",
            settings.model
        ));
    }
    // Do not hard-block recording when AX reports false — macOS often lags until
    // a full relaunch, and we fall back to clipboard after transcription.

    state
        .recorder
        .lock()
        .start(state.temp_wav_path.clone(), &settings.microphone)?;
    *is_recording = true;
    Ok(())
}

#[tauri::command]
async fn stop_recording_and_transcribe(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    {
        let mut is_recording = state.is_recording.lock();
        if !*is_recording {
            return Err("Not currently recording".into());
        }
        state.recorder.lock().stop()?;
        *is_recording = false;
    }

    // Brief settle so the WAV finalizes cleanly
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    let settings = state.settings.lock().clone();
    let model_path = model_manager::resolved_model_path(&settings.model);

    if !model_manager::is_model_downloaded(&settings.model) {
        return Err(format!(
            "Model '{}' not found. Open Settings → Models to download one.",
            settings.model
        ));
    }

    let wav_path = state.temp_wav_path.clone();
    let language = settings.language.clone();
    let silence_threshold = settings.silence_threshold;
    let text = tokio::task::spawn_blocking(move || {
        speech::transcribe(&wav_path, &model_path, &language, silence_threshold)
    })
    .await
    .map_err(|e| e.to_string())??;

    if text.is_empty() {
        return Ok(String::new());
    }

    let mut text = text;
    if settings.append_trailing_space && !text.ends_with(' ') {
        text.push(' ');
    }

    // paste / type / clipboard — paste is default (most reliable)
    if let Err(e) = output::deliver_text(&text, &settings.output_mode) {
        log::warn!("[output] deliver failed: {e}");
        // Last resort: still keep text in clipboard so user can ⌘V
        let _ = output::copy_to_clipboard(text.trim());
    }

    {
        let mut history = state.transcript_history.lock();
        let mut id_counter = state.next_id.lock();
        let entry = TranscriptEntry {
            id: *id_counter,
            text: text.trim().to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        *id_counter += 1;
        history.push_front(entry);
        let limit = settings.history_limit.max(1) as usize;
        while history.len() > limit {
            history.pop_back();
        }
    }

    let _ = app.emit("transcript-updated", ());
    Ok(text)
}

#[tauri::command]
fn get_audio_level(state: State<'_, AppState>) -> f32 {
    state.recorder.lock().get_level().min(1.0)
}

#[tauri::command]
fn get_history(state: State<'_, AppState>) -> Vec<TranscriptEntry> {
    state.transcript_history.lock().iter().cloned().collect()
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings.lock().clone()
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<(), String> {
    let old = state.settings.lock().clone();
    let model_changed = old.model != settings.model;
    let hotkey_changed = old.hotkey != settings.hotkey;

    *state.settings.lock() = settings.clone();
    persist_settings(&settings)?;

    if model_changed {
        speech::unload_model();
        // Warm the new model in the background if it's already downloaded
        if model_manager::is_model_downloaded(&settings.model) {
            let path = model_manager::resolved_model_path(&settings.model);
            std::thread::spawn(move || {
                if let Err(e) = speech::preload_model(&path) {
                    log::warn!("[speech] preload failed: {e}");
                }
            });
        }
    }

    if hotkey_changed {
        let _ = app.global_shortcut().unregister(old.hotkey.as_str());
        app.global_shortcut()
            .register(settings.hotkey.as_str())
            .map_err(|e| format!("Could not register shortcut: {e}"))?;
    }

    Ok(())
}

#[tauri::command]
fn complete_onboarding(state: State<'_, AppState>) -> Result<(), String> {
    let mut s = state.settings.lock().clone();
    s.onboarding_complete = true;
    *state.settings.lock() = s.clone();
    persist_settings(&s)
}

#[derive(Serialize)]
struct PermissionsStatus {
    accessibility: bool,
    microphone: bool,
    model_ready: bool,
    onboarding_complete: bool,
    active_model: String,
    /// What to look for in the Accessibility list (binary name).
    process_name: String,
    /// Full path of the running binary (helpful in dev).
    executable_path: String,
    /// True when running `tauri dev` / cargo binary (not a .app bundle).
    is_dev: bool,
}

#[tauri::command]
fn get_permissions(state: State<'_, AppState>) -> PermissionsStatus {
    let settings = state.settings.lock().clone();
    PermissionsStatus {
        accessibility: output::is_accessibility_trusted(),
        microphone: output::has_input_device(),
        model_ready: model_manager::is_model_downloaded(&settings.model),
        onboarding_complete: settings.onboarding_complete,
        active_model: settings.model,
        process_name: output::process_display_name(),
        executable_path: output::executable_path(),
        is_dev: output::is_dev_binary(),
    }
}

/// Show the system Accessibility dialog + open the privacy pane.
#[tauri::command]
fn request_accessibility() -> bool {
    output::request_accessibility_permission()
}

#[tauri::command]
fn open_accessibility_settings() -> Result<(), String> {
    output::open_accessibility_settings()
}

#[tauri::command]
fn open_microphone_settings() -> Result<(), String> {
    output::open_microphone_settings()
}

/// Reveal the running binary in Finder (for Accessibility “+” button).
#[tauri::command]
fn reveal_executable() -> Result<(), String> {
    output::reveal_executable_in_finder()
}

/// Copy the running binary path to the clipboard.
#[tauri::command]
fn copy_executable_path() -> Result<(), String> {
    output::copy_to_clipboard(&output::executable_path())
}

#[tauri::command]
fn get_models() -> Vec<model_manager::AppModel> {
    model_manager::list_models_for_ui()
}

#[tauri::command]
async fn download_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_name: String,
) -> Result<(), String> {
    let (dest_dir, files) = model_manager::resolve_download(&model_name)?;
    if files.is_empty() {
        return Err(format!("Unknown model: {model_name}"));
    }

    model_manager::ensure_models_dir().map_err(|e| e.to_string())?;

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *state.download_cancel.lock() = Some(cancel.clone());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()
        .map_err(|e| e.to_string())?;

    let file_count = files.len();
    // Actual download key for progress events (may map whisper catalog → ggml)
    let progress_key = model_name.clone();

    for (file_index, (filename, url)) in files.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = app.emit(
                "download-progress",
                serde_json::json!({ "model": &progress_key, "cancelled": true }),
            );
            return Err("Download cancelled".into());
        }

        let dest_path = dest_dir.join(filename);
        if dest_path.exists() {
            let _ = app.emit(
                "download-progress",
                serde_json::json!({
                    "model": &progress_key,
                    "file": filename,
                    "fileIndex": file_index,
                    "fileCount": file_count,
                    "bytesReceived": 0u64,
                    "totalBytes": 0u64,
                    "fileDone": true,
                    "done": file_index + 1 == file_count,
                }),
            );
            continue;
        }

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Network error: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {} downloading {filename}", resp.status()));
        }

        let total_bytes = resp.content_length().unwrap_or(0);
        let tmp_path = dest_path.with_extension("tmp");
        let mut out = tokio::fs::File::create(&tmp_path)
            .await
            .map_err(|e| e.to_string())?;

        let mut bytes_received: u64 = 0;
        let mut stream = resp.bytes_stream();
        let mut last_emit = std::time::Instant::now();

        while let Some(chunk) = stream.next().await {
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                drop(out);
                let _ = tokio::fs::remove_file(&tmp_path).await;
                let _ = app.emit(
                    "download-progress",
                    serde_json::json!({ "model": &progress_key, "cancelled": true }),
                );
                return Err("Download cancelled".into());
            }

            let chunk = chunk.map_err(|e| format!("Stream error: {e}"))?;
            use tokio::io::AsyncWriteExt;
            out.write_all(&chunk).await.map_err(|e| e.to_string())?;
            bytes_received += chunk.len() as u64;

            if last_emit.elapsed().as_millis() >= 200 {
                let _ = app.emit(
                    "download-progress",
                    serde_json::json!({
                        "model": &progress_key,
                        "file": filename,
                        "fileIndex": file_index,
                        "fileCount": file_count,
                        "bytesReceived": bytes_received,
                        "totalBytes": total_bytes,
                        "fileDone": false,
                        "done": false,
                    }),
                );
                last_emit = std::time::Instant::now();
            }
        }

        drop(out);
        tokio::fs::rename(&tmp_path, &dest_path)
            .await
            .map_err(|e| e.to_string())?;

        let is_done = file_index + 1 == file_count;
        let _ = app.emit(
            "download-progress",
            serde_json::json!({
                "model": &progress_key,
                "file": filename,
                "fileIndex": file_index,
                "fileCount": file_count,
                "bytesReceived": bytes_received,
                "totalBytes": total_bytes,
                "fileDone": true,
                "done": is_done,
            }),
        );
    }

    *state.download_cancel.lock() = None;

    let active = state.settings.lock().model.clone();
    // Warm if the downloaded name (or mapped ggml) is active
    let warm_name = model_name.clone();
    if active == warm_name || model_manager::model_path(&warm_name).exists() {
        let path = if model_manager::model_path(&warm_name).exists() {
            model_manager::model_path(&warm_name)
        } else {
            dest_dir.join(files[0].0.clone())
        };
        if path.exists() && !path.is_dir() || path.is_dir() {
            std::thread::spawn(move || {
                if let Err(e) = speech::preload_model(&path) {
                    log::warn!("[speech] preload after download failed: {e}");
                }
            });
        }
    }

    Ok(())
}

#[tauri::command]
fn cancel_download(state: State<'_, AppState>) {
    if let Some(token) = state.download_cancel.lock().as_ref() {
        token.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tauri::command]
fn delete_model(state: State<'_, AppState>, model_name: String) -> Result<(), String> {
    let path = model_manager::model_path(&model_name);
    let catalog_dir = model_manager::models_dir()
        .join("catalog")
        .join(&model_name);

    if path.is_dir() {
        std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
    } else if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    } else if catalog_dir.is_dir() {
        std::fs::remove_dir_all(&catalog_dir).map_err(|e| e.to_string())?;
    }

    let active = state.settings.lock().model.clone();
    if active == model_name {
        speech::unload_model();
    }
    Ok(())
}

#[tauri::command]
fn open_models_folder() -> Result<(), String> {
    model_manager::ensure_models_dir().map_err(|e| e.to_string())?;
    let path = model_manager::models_dir();
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn clear_history(state: State<'_, AppState>) {
    state.transcript_history.lock().clear();
}

#[tauri::command]
fn list_microphones() -> Vec<String> {
    output::list_input_devices()
}

#[tauri::command]
fn default_microphone() -> Option<String> {
    output::default_input_device_name()
}

/// Probe whether an accelerator string can be registered — used by the
/// shortcut-capture UI to flag a conflict before the user saves it.
#[tauri::command]
fn is_shortcut_available(app: AppHandle, state: State<'_, AppState>, accel: String) -> bool {
    if state.settings.lock().hotkey == accel {
        return true;
    }
    let gs = app.global_shortcut();
    match gs.register(accel.as_str()) {
        Ok(_) => {
            let _ = gs.unregister(accel.as_str());
            true
        }
        Err(_) => false,
    }
}

#[tauri::command]
fn export_settings_to(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let settings = state.settings.lock().clone();
    let raw = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(path, raw).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_settings_from(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<AppSettings, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let settings: AppSettings = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    save_settings(app, state, settings.clone())?;
    Ok(settings)
}

#[tauri::command]
fn catalog_stats() -> serde_json::Value {
    serde_json::json!({
        "catalogModels": model_manager::catalog_count(),
        "totalListed": model_manager::list_models_for_ui().len(),
    })
}

#[tauri::command]
fn show_settings_window(app: AppHandle) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Fully quit so Accessibility grants apply on next launch.
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ─── App Setup ───────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::init();
    speech::init_transcribe_cpp();
    model_manager::ensure_app_data_dir().ok();
    model_manager::ensure_models_dir().ok();

    let app_state = AppState::new();
    let default_hotkey = app_state.settings.lock().hotkey.clone();
    let preload_model = app_state.settings.lock().model.clone();

    let mut builder = tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| match event.state() {
                    ShortcutState::Pressed => {
                        let _ = app.emit("shortcut-pressed", ());
                    }
                    ShortcutState::Released => {
                        let _ = app.emit("shortcut-released", ());
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ));

    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_plugin_macos_permissions::init());
    }

    builder
        .manage(app_state)
        .setup(move |app| {
            // Regular (not Accessory): shows in Dock and makes Accessibility grants
            // attach reliably. Agent-only apps often never appear as "trusted".
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);

            app.global_shortcut()
                .register(default_hotkey.as_str())
                .map_err(|e| {
                    eprintln!("[shortcut] Failed to register '{default_hotkey}': {e}");
                    e
                })
                .ok();

            // Warm active model if present
            if model_manager::is_model_downloaded(&preload_model) {
                let path = model_manager::resolved_model_path(&preload_model);
                std::thread::spawn(move || {
                    if let Err(e) = speech::preload_model(&path) {
                        log::warn!("[speech] startup preload failed: {e}");
                    }
                });
            }

            let open_settings_i =
                MenuItemBuilder::with_id("open_settings", "Settings…").build(app)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit_i = MenuItemBuilder::with_id("quit", "Quit OpenVoice").build(app)?;

            let menu = MenuBuilder::new(app)
                .items(&[&open_settings_i, &separator, &quit_i])
                .build()?;

            let mut tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("OpenVoice")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open_settings" => {
                        if let Some(win) = app.get_webview_window("settings") {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                });
            #[cfg(target_os = "macos")]
            {
                tray = tray.icon_as_template(true);
            }
            tray.build(app)?;

            // Show settings on first launch / unless user chose start_hidden
            let show_settings = {
                let state = app.state::<AppState>();
                let s = state.settings.lock();
                if s.start_hidden && s.onboarding_complete {
                    false
                } else {
                    !s.onboarding_complete || !model_manager::is_model_downloaded(&s.model)
                }
            };
            if show_settings {
                if let Some(win) = app.get_webview_window("settings") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording_and_transcribe,
            get_audio_level,
            get_history,
            get_settings,
            save_settings,
            complete_onboarding,
            get_permissions,
            request_accessibility,
            open_accessibility_settings,
            open_microphone_settings,
            reveal_executable,
            copy_executable_path,
            quit_app,
            get_models,
            download_model,
            cancel_download,
            delete_model,
            open_models_folder,
            clear_history,
            show_settings_window,
            list_microphones,
            default_microphone,
            catalog_stats,
            is_shortcut_available,
            export_settings_to,
            import_settings_from,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OpenVoice");
}
