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
    /// Never show a Dock icon, even while a window (e.g. Settings) is open.
    /// The app is still reachable via the tray icon.
    #[serde(default)]
    pub hide_dock_icon: bool,
    /// Periodically re-transcribe audio while recording so the HUD shows a
    /// gradually-updating preview instead of only the final text. Costs
    /// extra inference passes, so it's opt-in.
    #[serde(default)]
    pub live_preview: bool,
    /// After transcribing, show the finished transcript text in the overlay
    /// for a moment before it hides. Off by default — the text is inserted
    /// into the focused app anyway, so most people want the overlay to just
    /// disappear (WisprFlow-style) rather than echo it back.
    #[serde(default)]
    pub show_transcript_in_overlay: bool,
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
            model: "whisper-tiny.en".into(),
            // paste = clipboard + ⌘V (most reliable on macOS)
            output_mode: "paste".into(),
            hotkey: "Alt+Space".into(),
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
            hide_dock_icon: false,
            live_preview: false,
            show_transcript_in_overlay: false,
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
    /// Sender to the single recording-lifecycle coordinator thread. The
    /// shortcut handler pushes Press/Release here; the coordinator owns the
    /// start→stop→transcribe→deliver→hide sequence. Set during `setup`.
    pub coord_tx: Mutex<Option<std::sync::mpsc::Sender<CoordCmd>>>,
    /// Bumped on every recording start. A delayed overlay-hide captures the
    /// value at schedule time and only hides if it's unchanged — so a new
    /// recording started during the previous result's linger never gets its
    /// overlay yanked away.
    pub hud_generation: std::sync::atomic::AtomicU64,
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
            coord_tx: Mutex::new(None),
            hud_generation: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

// ─── Recording lifecycle coordinator ────────────────────────────────────────
//
// The entire record → stop → transcribe → deliver → hide sequence is owned by
// ONE dedicated thread, driven directly by the global shortcut's press/release.
// This replaces the previous frontend-driven state machine, which decided
// start/stop in React from async events and guarded on a mutable ref — a design
// that could miss a fast release (releasing before the async start finished set
// the "recording" flag), leaving the overlay stuck open forever. Matching
// Handy, the source of truth now lives in Rust where press and release are
// handled synchronously and in order. The frontend HUD is a pure display of the
// `hud-state` events emitted here.

/// A press/release of the global shortcut, forwarded to the coordinator.
pub enum CoordCmd {
    Press,
    Release,
}

/// Overlay state pushed to the HUD webview.
#[derive(Clone, Serialize)]
struct HudPayload {
    phase: String, // "recording" | "transcribing" | "result" | "error" | "idle"
    text: String,
    show_text: bool,
}

/// Always-on diagnostic log at app-data/openvoice.log, independent of RUST_LOG
/// (which is unset when the app is launched from Finder, so env_logger output
/// is lost). Records the recording lifecycle so a "didn't work" report leaves
/// real evidence — e.g. a "Pressed" with no matching "Released" would prove the
/// OS never delivered the key release.
fn hud_log(msg: &str) {
    use std::io::Write;
    log::info!("{msg}");
    let path = model_manager::app_data_dir().join("openvoice.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{ts} {msg}");
    }
}

fn emit_hud(app: &AppHandle, phase: &str, text: &str, show_text: bool) {
    let _ = app.emit(
        "hud-state",
        HudPayload {
            phase: phase.into(),
            text: text.into(),
            show_text,
        },
    );
}

/// Hide the overlay after `delay_ms`, unless a newer recording session has
/// started in the meantime (generation guard).
fn schedule_hide(app: &AppHandle, delay_ms: u64) {
    let app = app.clone();
    let generation = app
        .state::<AppState>()
        .hud_generation
        .load(std::sync::atomic::Ordering::SeqCst);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        let state = app.state::<AppState>();
        let unchanged =
            state.hud_generation.load(std::sync::atomic::Ordering::SeqCst) == generation;
        if unchanged && !*state.is_recording.lock() {
            hide_hud_window(&app);
            emit_hud(&app, "idle", "", false);
        }
    });
}

/// Start recording. Returns true if recording actually began.
fn coordinator_start(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().clone();

    if !model_manager::is_model_downloaded(&settings.model) {
        show_hud_window(app);
        emit_hud(
            app,
            "error",
            &format!("No model ready — open Settings and download \"{}\".", settings.model),
            false,
        );
        schedule_hide(app, 3500);
        return false;
    }

    if let Err(e) = state
        .recorder
        .lock()
        .start(state.temp_wav_path.clone(), &settings.microphone)
    {
        show_hud_window(app);
        emit_hud(app, "error", &e, false);
        schedule_hide(app, 3500);
        return false;
    }

    *state.is_recording.lock() = true;
    state
        .hud_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    show_hud_window(app);
    emit_hud(app, "recording", "", false);
    hud_log("[coord] recording started");
    true
}

/// Stop recording, transcribe, deliver the text, and drive the overlay through
/// its transcribing/result/error states. Runs entirely on the coordinator
/// thread, so nothing here races with a subsequent press (those queue behind it).
fn coordinator_stop(app: &AppHandle) {
    let state = app.state::<AppState>();

    {
        let mut is_recording = state.is_recording.lock();
        if !*is_recording {
            return;
        }
        let _ = state.recorder.lock().stop();
        *is_recording = false;
    }

    emit_hud(app, "transcribing", "", false);
    hud_log("[coord] transcribing");
    // Brief settle so the WAV finalizes cleanly.
    std::thread::sleep(std::time::Duration::from_millis(80));

    let settings = state.settings.lock().clone();
    let model_path = model_manager::resolved_model_path(&settings.model);
    let wav_path = state.temp_wav_path.clone();

    let result = speech::transcribe(
        &wav_path,
        &model_path,
        &settings.language,
        settings.silence_threshold,
    );

    match result {
        Ok(text) if !text.trim().is_empty() => {
            let mut text = text.trim().to_string();
            if settings.append_trailing_space && !text.ends_with(' ') {
                text.push(' ');
            }

            if let Err(e) = output::deliver_text(&text, &settings.output_mode) {
                log::warn!("[output] deliver failed: {e}");
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
            hud_log(&format!("[coord] result ({} chars)", text.trim().len()));

            if settings.show_transcript_in_overlay {
                // Opt-in: briefly echo the text before hiding.
                emit_hud(app, "result", text.trim(), true);
                schedule_hide(app, 2200);
            } else {
                // Default: no "Done" confirmation screen — the text is already
                // inserted, so the overlay just disappears.
                schedule_hide(app, 0);
            }
        }
        Ok(_) => {
            // Empty transcript: only surface the "no mic signal at all" case
            // (denied permission / wrong device), since that's actionable.
            // A plain "nothing was said" just hides with no screen.
            let peak = speech::wav_peak_level(&wav_path).unwrap_or(1.0);
            if peak < 0.001 {
                emit_hud(
                    app,
                    "error",
                    "No mic input — check System Settings → Privacy & Security → Microphone.",
                    false,
                );
                hud_log("[coord] empty result, peak≈0 (mic permission?)");
                schedule_hide(app, 3500);
            } else {
                hud_log("[coord] empty result, signal present");
                schedule_hide(app, 0);
            }
        }
        Err(e) => {
            emit_hud(app, "error", &e, false);
            hud_log(&format!("[coord] transcribe error: {e}"));
            schedule_hide(app, 3500);
        }
    }
}

/// Spawn the coordinator thread and return its command sender.
fn spawn_coordinator(app: AppHandle) -> std::sync::mpsc::Sender<CoordCmd> {
    let (tx, rx) = std::sync::mpsc::channel::<CoordCmd>();
    std::thread::Builder::new()
        .name("recording-coordinator".into())
        .spawn(move || {
            let mut recording = false;
            for cmd in rx {
                let ptt = {
                    let state = app.state::<AppState>();
                    let s = state.settings.lock();
                    s.recording_mode != "toggle"
                };
                match cmd {
                    CoordCmd::Press => {
                        if ptt {
                            if !recording {
                                recording = coordinator_start(&app);
                            }
                        } else if recording {
                            coordinator_stop(&app);
                            recording = false;
                        } else {
                            recording = coordinator_start(&app);
                        }
                    }
                    CoordCmd::Release => {
                        if ptt && recording {
                            coordinator_stop(&app);
                            recording = false;
                        }
                    }
                }
            }
        })
        .expect("failed to spawn recording-coordinator thread");
    tx
}

#[tauri::command]
fn get_audio_level(state: State<'_, AppState>) -> f32 {
    state.recorder.lock().get_level().min(1.0)
}

/// Re-transcribes the audio captured so far in the current recording, for a
/// "live preview" that updates gradually instead of only showing text once
/// recording stops. Runs the same model/engine as the final transcript, just
/// against a growing in-memory buffer instead of the finalized WAV file.
#[tauri::command]
fn get_partial_transcript(state: State<'_, AppState>) -> Result<String, String> {
    if !*state.is_recording.lock() {
        return Ok(String::new());
    }
    let samples = state.recorder.lock().snapshot_samples();
    if samples.len() < 16_000 {
        // Less than 1s of audio — not worth a pass.
        return Ok(String::new());
    }
    let settings = state.settings.lock().clone();
    let model_path = model_manager::resolved_model_path(&settings.model);
    speech::transcribe_samples(&samples, &model_path, &settings.language, settings.silence_threshold)
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
    let dock_changed = old.hide_dock_icon != settings.hide_dock_icon;

    *state.settings.lock() = settings.clone();
    persist_settings(&settings)?;

    #[cfg(target_os = "macos")]
    if dock_changed {
        let policy = if settings.hide_dock_icon {
            tauri::ActivationPolicy::Accessory
        } else {
            tauri::ActivationPolicy::Regular
        };
        let _ = app.set_activation_policy(policy);
    }

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

/// Shows + positions the HUD overlay directly from Rust, driven by the
/// global-shortcut handler itself rather than a frontend event round-trip.
/// The frontend still owns hiding it (state transitions need a delay to show
/// "done"/"error" before disappearing) — this only guarantees the overlay
/// reliably *appears* the instant the shortcut fires.
// The window is deliberately larger than the visible pill: the pill is
// centered inside it with a wide TRANSPARENT margin so its soft drop shadow
// fades to nothing well before the window's rectangular edge. (Too small a
// margin clips the shadow at the edge and it reads as a translucent
// rectangle.) Only the rounded pill is ever painted.
const HUD_W: f64 = 236.0;
const HUD_H: f64 = 96.0;
/// Taller variant used only when live preview is on, to fit the rolling text.
const HUD_H_PREVIEW: f64 = 168.0;

/// The monitor the HUD should appear on: whichever one currently has the
/// mouse cursor, so the overlay follows the screen the user is actually
/// looking at instead of always pinning to the primary display.
fn active_monitor(win: &tauri::WebviewWindow) -> Option<tauri::Monitor> {
    use enigo::{Enigo, Mouse, Settings};
    let cursor = Enigo::new(&Settings::default()).ok()?.location().ok()?;
    let (cx, cy) = cursor;
    win.available_monitors().ok()?.into_iter().find(|m| {
        let pos = m.position();
        let size = m.size();
        (cx as i64) >= pos.x as i64
            && (cx as i64) < pos.x as i64 + size.width as i64
            && (cy as i64) >= pos.y as i64
            && (cy as i64) < pos.y as i64 + size.height as i64
    })
}

fn show_hud_window(app: &AppHandle) {
    let show_overlay = app
        .try_state::<AppState>()
        .map(|s| s.settings.lock().show_overlay)
        .unwrap_or(true);
    if !show_overlay {
        return;
    }
    let height = if app
        .try_state::<AppState>()
        .map(|s| s.settings.lock().live_preview)
        .unwrap_or(false)
    {
        HUD_H_PREVIEW
    } else {
        HUD_H
    };
    // Window geometry/visibility must be touched on the main thread on macOS —
    // the coordinator calls this from a background thread.
    let app_for_closure = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(win) = app_for_closure.get_webview_window("hud") else {
            return;
        };
        // Force the macOS window shadow off (the rounded pill draws its own);
        // a rectangular window shadow would show around the transparent margin.
        let _ = win.set_shadow(false);
        let _ = win.set_size(tauri::Size::Logical(tauri::LogicalSize {
            width: HUD_W,
            height,
        }));
        let monitor = active_monitor(&win).or_else(|| win.primary_monitor().ok().flatten());
        if let Some(monitor) = monitor {
            let scale = monitor.scale_factor();
            let mon_pos = monitor.position();
            let mon_size = monitor.size();
            let screen_x = mon_pos.x as f64 / scale;
            let screen_y = mon_pos.y as f64 / scale;
            let screen_w = mon_size.width as f64 / scale;
            let screen_h = mon_size.height as f64 / scale;
            let x = (screen_x + (screen_w - HUD_W) / 2.0).round();
            // Keep the pill ~110px above the bottom regardless of window height
            // (the pill is vertically centered within the window).
            let y = (screen_y + screen_h - 110.0 - height / 2.0).round();
            let _ = win.set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
        }
        let _ = win.show();
    });
}

/// Hide the HUD window on the main thread (safe to call from any thread).
fn hide_hud_window(app: &AppHandle) {
    let app_for_closure = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app_for_closure.get_webview_window("hud") {
            let _ = win.hide();
        }
    });
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
                .with_handler(|app, _shortcut, event| {
                    let cmd = match event.state() {
                        ShortcutState::Pressed => {
                            hud_log("[shortcut] Pressed");
                            CoordCmd::Press
                        }
                        ShortcutState::Released => {
                            hud_log("[shortcut] Released");
                            CoordCmd::Release
                        }
                    };
                    if let Some(tx) = app.state::<AppState>().coord_tx.lock().as_ref() {
                        let _ = tx.send(cmd);
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
            // Spawn the recording-lifecycle coordinator and hand its sender to
            // the shared state so the shortcut handler can reach it.
            let coord_tx = spawn_coordinator(app.handle().clone());
            *app.state::<AppState>().coord_tx.lock() = Some(coord_tx);

            // Regular (not Accessory) by default: shows in Dock and makes
            // Accessibility grants attach reliably. Agent-only apps often never
            // appear as "trusted". Accessory only when the user explicitly opts
            // in via the "hide_dock_icon" setting — the tray icon stays either
            // way, so the app remains reachable.
            #[cfg(target_os = "macos")]
            {
                let hide_dock = app.state::<AppState>().settings.lock().hide_dock_icon;
                let policy = if hide_dock {
                    tauri::ActivationPolicy::Accessory
                } else {
                    tauri::ActivationPolicy::Regular
                };
                app.set_activation_policy(policy);
            }

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
            get_audio_level,
            get_partial_transcript,
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
