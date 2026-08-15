// lib.rs — OpenVoice Tauri backend entry point.

mod audio;
mod llm;
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
    /// What was actually pasted (the enhanced text when enhancement is on).
    pub text: String,
    /// The original raw transcript, kept only when enhancement changed it, so
    /// the UI can offer a "view original". `None` when it equals `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
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

    // ── Transcript enhancement (V2): a small local LLM polishes the raw ASR
    //    text before it's pasted. All off by default (opt-in). See llm.rs.
    /// Master switch for LLM post-processing.
    #[serde(default)]
    pub enhance_enabled: bool,
    /// Slug of the downloaded enhancement model (empty = none selected).
    #[serde(default)]
    pub enhance_model: String,
    /// "auto" | "clean" | "message" | "email" | "notes" | "custom"
    #[serde(default = "default_enhance_mode")]
    pub enhance_mode: String,
    /// "light" | "balanced" | "strong"
    #[serde(default = "default_enhance_intensity")]
    pub enhance_intensity: String,
    /// Custom instruction used when enhance_mode == "custom".
    #[serde(default)]
    pub enhance_custom_prompt: String,
    /// Obey spoken instructions in the transcript ("make this more formal",
    /// "summarize this") instead of transcribing them literally. Off by default.
    #[serde(default)]
    pub enhance_voice_commands: bool,
    /// Write each enhancement (raw transcript, full prompt, model response) to
    /// enhancement.log for debugging. Off by default.
    #[serde(default)]
    pub enhance_debug_log: bool,
    /// Max output tokens for enhancement. `0` = auto (scale to the input, the
    /// default); `-1` = unconstrained (bounded only by the generation
    /// wall-clock guard); a positive value = a hard cap the user chose.
    #[serde(default)]
    pub enhance_max_tokens: i32,
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
fn default_enhance_mode() -> String {
    "clean".into()
}
fn default_enhance_intensity() -> String {
    // Light (level 1) by default: safe for short dictations. Medium/Heavy add
    // the aggressive "must restructure" levels, which help long rambling text
    // but over-edit a short one-liner.
    "light".into()
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
            enhance_enabled: false,
            enhance_model: String::new(),
            enhance_mode: default_enhance_mode(),
            enhance_intensity: default_enhance_intensity(),
            enhance_custom_prompt: String::new(),
            enhance_voice_commands: false,
            enhance_debug_log: false,
            enhance_max_tokens: 0,
        }
    }
}

fn load_settings() -> AppSettings {
    let path = model_manager::settings_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => sanitize_settings(serde_json::from_str(&raw).unwrap_or_default()),
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
    /// Name of the app that was frontmost when the current recording started,
    /// captured on Press (on the main thread). Given to the model as context in
    /// "Automatic (per app)" enhancement.
    pub frontmost_app: Mutex<Option<String>>,
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
            frontmost_app: Mutex::new(None),
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
    phase: String, // "recording" | "transcribing" | "enhancing" | "result" | "error" | "idle"
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
    // Cap the log so it can never grow unbounded: past ~512 KB, keep only the
    // newest half. Cheap enough to check on every write.
    const MAX_LOG_BYTES: u64 = 512 * 1024;
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                let keep_from = raw.len() / 2;
                // Cut at a line boundary so the file stays parseable.
                let start = raw[keep_from..]
                    .find('\n')
                    .map(|i| keep_from + i + 1)
                    .unwrap_or(keep_from);
                let _ = std::fs::write(&path, &raw[start..]);
            }
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{ts} {msg}");
    }
}

/// Absolute path of the opt-in enhancement debug log.
fn enhance_log_path() -> PathBuf {
    model_manager::app_data_dir().join("enhancement.log")
}

/// Appends a detailed enhancement record (raw transcript, full prompt, model
/// response) to a separate opt-in log file. Only called when the user enables
/// `enhance_debug_log`. Capped like the main log so it can't grow unbounded.
fn enhance_log(block: &str) {
    use std::io::Write;
    let path = enhance_log_path();
    const MAX_LOG_BYTES: u64 = 1024 * 1024; // 1 MB (entries are large)
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                let keep_from = raw.len() / 2;
                let start = raw[keep_from..]
                    .find('\n')
                    .map(|i| keep_from + i + 1)
                    .unwrap_or(keep_from);
                let _ = std::fs::write(&path, &raw[start..]);
            }
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{block}");
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

    // Capture the frontmost app NOW, while the user's target app is still
    // focused (our overlay never steals focus), for "Automatic (per app)"
    // enhancement. Runs on the main thread (AppKit requirement); the result
    // lands well before Release since the user speaks for a moment first. Only
    // needed when auto mode is active.
    if settings.enhance_enabled && settings.enhance_mode == "auto" {
        // Clear first so a fast tap (before the main-thread read lands) falls
        // back cleanly instead of reusing the previous recording's app.
        *state.frontmost_app.lock() = None;
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            let name = output::frontmost_app_name();
            *app2.state::<AppState>().frontmost_app.lock() = name;
        });
    }

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
    if settings.audio_feedback {
        output::play_feedback_sound("start");
    }
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

    if state.settings.lock().audio_feedback {
        output::play_feedback_sound("stop");
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
            let raw = text.trim().to_string();

            // Optional LLM enhancement. Best-effort: on ANY failure (model not
            // downloaded, load/generation error, timeout) we keep `raw`, so the
            // user always gets their transcript. `enhanced_differs` tracks
            // whether to preserve the original in history.
            let mut text = raw.clone();
            if settings.enhance_enabled
                && !settings.enhance_model.is_empty()
                && model_manager::is_model_downloaded(&settings.enhance_model)
            {
                emit_hud(app, "enhancing", "", false);
                hud_log("[coord] enhancing");
                let dir = model_manager::resolved_model_dir(&settings.enhance_model);

                // "Automatic (per app)": pass the frontmost app name and let the
                // model adapt — no hardcoded per-app formatting. Other modes use
                // the user's chosen mode with no app context.
                let app_name: Option<String> = if settings.enhance_mode == "auto" {
                    let a = state.frontmost_app.lock().clone();
                    if let Some(name) = &a {
                        hud_log(&format!("[coord] auto mode, app: {name}"));
                    }
                    a
                } else {
                    None
                };

                let sys = llm::system_prompt(
                    &settings.enhance_mode,
                    &settings.enhance_intensity,
                    &settings.enhance_custom_prompt,
                    app_name.as_deref(),
                    settings.enhance_voice_commands,
                );
                let temperature = llm::temperature_for(&settings.enhance_intensity);
                let params = llm::GenParams {
                    temperature,
                    max_tokens: settings.enhance_max_tokens,
                };
                let t0 = std::time::Instant::now();
                // Wait a hair longer than the in-worker time budget (which scales
                // with the user's token setting) so the worker's own guard trips
                // first and returns raw cleanly, rather than the caller timing out
                // while the worker keeps running. Normal cleanups finish in well
                // under a second; a raised/unconstrained budget waits longer
                // before falling back to raw.
                let call_timeout = llm::gen_time_budget(settings.enhance_max_tokens)
                    + std::time::Duration::from_secs(2);
                let outcome = llm::enhance(&dir, &sys, &raw, params, call_timeout);
                let elapsed_ms = t0.elapsed().as_millis();

                let outcome_str = match &outcome {
                    Ok(enhanced) if !enhanced.trim().is_empty() => {
                        text = enhanced.trim().to_string();
                        hud_log(&format!("[coord] enhanced ({} chars)", text.len()));
                        format!("OUTPUT (pasted):\n{}", text)
                    }
                    Ok(_) => {
                        hud_log("[coord] enhancement returned empty — using raw");
                        "OUTPUT: <empty> — fell back to raw transcript".to_string()
                    }
                    Err(e) => {
                        log::warn!("[llm] enhancement failed: {e}");
                        hud_log(&format!("[coord] enhancement failed ({e}) — using raw"));
                        format!("ERROR: {e} — fell back to raw transcript")
                    }
                };

                // Opt-in detailed log: raw transcript, resolved settings, the
                // exact prompt sent, and the model's response.
                if settings.enhance_debug_log {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    enhance_log(&format!(
                        "===== {ts} =====\n\
                         mode={} app={} intensity={} temp={temperature} voice_cmds={} max_tokens={} {elapsed_ms}ms\n\
                         ----- RAW TRANSCRIPT -----\n{raw}\n\
                         ----- SYSTEM PROMPT -----\n{sys}\n\
                         ----- {outcome_str}\n",
                        settings.enhance_mode,
                        app_name.as_deref().unwrap_or("-"),
                        settings.enhance_intensity,
                        settings.enhance_voice_commands,
                        settings.enhance_max_tokens,
                    ));
                }
            }

            // History keeps the original only when enhancement actually changed it.
            let raw_for_history = if text.trim() != raw.trim() {
                Some(raw.clone())
            } else {
                None
            };

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
                    raw: raw_for_history,
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
    let enhance_changed = old.enhance_model != settings.enhance_model
        || old.enhance_enabled != settings.enhance_enabled;

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

    if enhance_changed {
        llm::unload_llm();
        // Warm the enhancement model in the background if enabled + downloaded.
        if settings.enhance_enabled
            && !settings.enhance_model.is_empty()
            && model_manager::is_model_downloaded(&settings.enhance_model)
        {
            let dir = model_manager::resolved_model_dir(&settings.enhance_model);
            std::thread::spawn(move || {
                if let Err(e) = llm::preload_llm(&dir) {
                    log::warn!("[llm] preload failed: {e}");
                }
            });
        }
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

    let warm_name = model_name.clone();

    if model_manager::model_kind(&warm_name) == "llm" {
        // Enhancement model: warm it only if it's the selected one and
        // enhancement is enabled.
        let s = state.settings.lock();
        if s.enhance_enabled && s.enhance_model == warm_name {
            let dir = model_manager::resolved_model_dir(&warm_name);
            std::thread::spawn(move || {
                if let Err(e) = llm::preload_llm(&dir) {
                    log::warn!("[llm] preload after download failed: {e}");
                }
            });
        }
        return Ok(());
    }

    let active = state.settings.lock().model.clone();
    // Warm if the downloaded name (or mapped ggml) is active
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

/// Register a user-supplied enhancement model. It appears in the Enhancement UI
/// immediately (as not-downloaded); the frontend then triggers a normal
/// download. Returns the final slug.
#[tauri::command]
fn add_enhancement_model(model: model_manager::UserModel) -> Result<String, String> {
    model_manager::add_user_model(model)
}

/// Remove a user-added enhancement model: delete its files and its registry
/// entry, and unload it if it's the active model.
#[tauri::command]
fn remove_enhancement_model(state: State<'_, AppState>, slug: String) -> Result<(), String> {
    let dir = model_manager::models_dir().join("catalog").join(&slug);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    if state.settings.lock().enhance_model == slug {
        llm::unload_llm();
    }
    model_manager::remove_user_model(&slug)
}

/// List the `.gguf` filenames in a HuggingFace repo, so the add-model form can
/// offer a dropdown instead of asking the user to type an exact filename.
#[tauri::command]
async fn fetch_repo_gguf_files(repo: String) -> Result<Vec<String>, String> {
    let repo = repo.trim().trim_matches('/');
    if repo.is_empty() {
        return Err("Repository is required".into());
    }
    let url = format!("https://huggingface.co/api/models/{repo}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(&url)
        .header("User-Agent", "OpenVoice")
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {repo}", resp.status()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let mut files: Vec<String> = v
        .get("siblings")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("rfilename").and_then(|n| n.as_str()))
                .filter(|n| n.to_lowercase().ends_with(".gguf"))
                .map(|n| n.to_string())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    Ok(files)
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
fn open_enhancement_log() -> Result<(), String> {
    let path = enhance_log_path();
    if !path.exists() {
        std::fs::write(&path, "# OpenVoice enhancement log — enable logging and dictate to populate.\n")
            .map_err(|e| e.to_string())?;
    }
    std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn clear_enhancement_log() -> Result<(), String> {
    let path = enhance_log_path();
    if path.exists() {
        std::fs::write(&path, "").map_err(|e| e.to_string())?;
    }
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

/// Copy arbitrary text to the clipboard via the backend (arboard) — reliable
/// in the Tauri webview where `navigator.clipboard` may silently fail.
#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    output::copy_to_clipboard(&text)
}

#[tauri::command]
fn export_settings_to(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let settings = state.settings.lock().clone();
    let raw = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(path, raw).map_err(|e| e.to_string())
}

/// Clamp/normalize settings coming from an untrusted source (an imported
/// JSON file). Typed deserialization already rejects wrong shapes; this
/// closes the remaining gap of syntactically-valid but nonsensical values
/// (a 10 GB history limit, an enum-ish string outside its known set, a
/// threshold that silently disables the silence gate).
fn sanitize_settings(mut s: AppSettings) -> AppSettings {
    if !["paste", "type", "clipboard"].contains(&s.output_mode.as_str()) {
        s.output_mode = "paste".into();
    }
    if !["ptt", "toggle"].contains(&s.recording_mode.as_str()) {
        s.recording_mode = "ptt".into();
    }
    if !["system", "light", "dark"].contains(&s.theme.as_str()) {
        s.theme = "system".into();
    }
    s.history_limit = s.history_limit.clamp(1, 500);
    if !s.silence_threshold.is_finite() {
        s.silence_threshold = default_silence_threshold();
    }
    s.silence_threshold = s.silence_threshold.clamp(0.0, 0.1);
    if s.hotkey.trim().is_empty() {
        s.hotkey = "Alt+Space".into();
    }
    if !["auto", "clean", "message", "email", "notes", "custom"].contains(&s.enhance_mode.as_str()) {
        s.enhance_mode = "clean".into();
    }
    if !["light", "balanced", "strong"].contains(&s.enhance_intensity.as_str()) {
        s.enhance_intensity = "balanced".into();
    }
    // Bound the custom prompt so an imported settings file can't smuggle in an
    // enormous instruction blob.
    if s.enhance_custom_prompt.chars().count() > 2000 {
        s.enhance_custom_prompt = s.enhance_custom_prompt.chars().take(2000).collect();
    }
    // Max tokens: keep the sentinels (-1 unconstrained, 0 auto); clamp any
    // positive value to a sane window so the wall-clock guard is still the real
    // backstop for very large asks.
    if s.enhance_max_tokens < -1 {
        s.enhance_max_tokens = -1;
    } else if s.enhance_max_tokens > 0 {
        s.enhance_max_tokens = s.enhance_max_tokens.clamp(32, 4096);
    }
    s
}

#[tauri::command]
fn import_settings_from(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<AppSettings, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let settings: AppSettings = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let settings = sanitize_settings(settings);
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
    reveal_settings(&app, None);
}

/// Show + focus the settings window, optionally telling the frontend to
/// navigate to a specific tab first (via a `settings-navigate` event).
///
/// The close button hides the window rather than destroying it (see setup).
/// If it was still destroyed somehow, we recreate it so tray → Settings
/// always works without quitting the app.
fn reveal_settings(app: &AppHandle, tab: Option<&str>) {
    if let Some(tab) = tab {
        let _ = app.emit("settings-navigate", tab);
    }

    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        activate_app(app);
        return;
    }

    // Window was destroyed (e.g. old builds that didn't intercept close).
    // Rebuild so Settings remains reachable from the tray.
    log::warn!("[window] settings window missing — recreating");
    if let Err(e) = recreate_settings_window(app) {
        log::error!("[window] failed to recreate settings: {e}");
        return;
    }
    if let Some(tab) = tab {
        // Frontend may need a beat to mount listeners after recreate
        let app2 = app.clone();
        let tab = tab.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = app2.emit("settings-navigate", tab);
        });
    }
}

/// Brings the app itself to the front.
///
/// With `hide_dock_icon` on we run as `ActivationPolicy::Accessory`, and an
/// accessory app is never made frontmost by simply showing a window: the
/// window can order in behind whatever the user is working in, or come up
/// without keyboard focus. `set_focus()` alone is not enough — the process
/// has to explicitly activate. No-op cost when the Dock icon is visible.
fn activate_app(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(|| unsafe {
            use objc2::runtime::{AnyObject, Bool};
            use objc2::{class, msg_send};
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if !ns_app.is_null() {
                let _: () = msg_send![ns_app, activateIgnoringOtherApps: Bool::YES];
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Rebuilds the settings window. This MUST stay in sync with the `settings`
/// entry in `tauri.conf.json` — a recreated window gets none of that config,
/// so without these calls it would come back opaque, with a standard title
/// bar, no vibrancy and mispositioned traffic lights.
fn recreate_settings_window(app: &AppHandle) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let builder = WebviewWindowBuilder::new(
        app,
        "settings",
        WebviewUrl::App("index.html".into()),
    )
    .title("OpenVoice")
    .inner_size(740.0, 560.0)
    .min_inner_size(660.0, 480.0)
    .resizable(true)
    .visible(true)
    .center()
    .transparent(true)
    .effects(tauri::utils::config::WindowEffectsConfig {
        effects: vec![tauri::window::Effect::Sidebar],
        state: Some(tauri::window::EffectState::FollowsWindowActiveState),
        radius: None,
        color: None,
    });

    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(18.0, 20.0));

    let win = builder.build().map_err(|e| e.to_string())?;
    install_settings_close_handler(&win);
    let _ = win.set_focus();
    Ok(())
}

/// Close (✕) must hide, not destroy — otherwise tray → Settings does nothing.
fn install_settings_close_handler(win: &tauri::WebviewWindow) {
    let w = win.clone();
    win.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = w.hide();
        }
    });
}

/// Formats a Tauri accelerator string (e.g. "Alt+Space") into macOS glyphs
/// (e.g. "⌥ Space") for menu display.
fn format_hotkey(accel: &str) -> String {
    accel
        .split('+')
        .map(|part| match part.trim() {
            "CommandOrControl" | "CmdOrCtrl" | "Command" | "Cmd" | "Super" | "Meta" => "⌘",
            "Control" | "Ctrl" => "⌃",
            "Alt" | "Option" => "⌥",
            "Shift" => "⇧",
            "Space" => "Space",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    // Spawn the enhancement-model worker thread now; it stays idle until a model
    // is loaded. Capture whether to warm one at startup (only if enabled + present).
    llm::init_llm();
    let preload_enhance = {
        let s = app_state.settings.lock();
        if s.enhance_enabled && !s.enhance_model.is_empty() {
            Some(s.enhance_model.clone())
        } else {
            None
        }
    };

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

            // Request Microphone access from the backend so the app reliably
            // registers in System Settings and prompts on first launch, no
            // matter which window is (or isn't) visible. Done on a short delay,
            // on the main thread, so the app is fully active (a prompt requested
            // during setup, before the run loop is up, may never display).
            #[cfg(target_os = "macos")]
            {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    let _ = h.run_on_main_thread(output::request_microphone_access);
                });
            }

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

            // Warm the enhancement model too, if enabled and downloaded.
            if let Some(enhance_model) = preload_enhance {
                if model_manager::is_model_downloaded(&enhance_model) {
                    let dir = model_manager::resolved_model_dir(&enhance_model);
                    std::thread::spawn(move || {
                        if let Err(e) = llm::preload_llm(&dir) {
                            log::warn!("[llm] startup preload failed: {e}");
                        }
                    });
                }
            }

            // Informational header: current dictation shortcut (disabled row).
            let hotkey_label = {
                let state = app.state::<AppState>();
                let hotkey = state.settings.lock().hotkey.clone();
                format!("Hold {} to dictate", format_hotkey(&hotkey))
            };
            let header_i = MenuItemBuilder::with_id("hotkey_header", hotkey_label)
                .enabled(false)
                .build(app)?;

            let open_settings_i =
                MenuItemBuilder::with_id("open_settings", "Settings…").build(app)?;
            let models_i = MenuItemBuilder::with_id("open_models", "Models…").build(app)?;
            let history_i = MenuItemBuilder::with_id("open_history", "History…").build(app)?;
            let copy_last_i =
                MenuItemBuilder::with_id("copy_last", "Copy Last Transcript").build(app)?;
            let about_i = MenuItemBuilder::with_id(
                "open_about",
                format!("About OpenVoice {}", env!("CARGO_PKG_VERSION")),
            )
            .build(app)?;
            let quit_i = MenuItemBuilder::with_id("quit", "Quit OpenVoice").build(app)?;

            let menu = MenuBuilder::new(app)
                .items(&[
                    &header_i,
                    &PredefinedMenuItem::separator(app)?,
                    &open_settings_i,
                    &models_i,
                    &history_i,
                    &PredefinedMenuItem::separator(app)?,
                    &copy_last_i,
                    &PredefinedMenuItem::separator(app)?,
                    &about_i,
                    &quit_i,
                ])
                .build()?;

            let mut tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("OpenVoice")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open_settings" => reveal_settings(app, None),
                    "open_models" => reveal_settings(app, Some("models")),
                    "open_history" => reveal_settings(app, Some("history")),
                    "open_about" => reveal_settings(app, Some("about")),
                    "copy_last" => {
                        let state = app.state::<AppState>();
                        let last = state
                            .transcript_history
                            .lock()
                            .front()
                            .map(|e| e.text.clone());
                        if let Some(text) = last {
                            let _ = output::copy_to_clipboard(text.trim());
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
            // Intercept ✕ so the window is hidden, not destroyed. Without this,
            // closing Settings once makes every later tray → Settings a no-op
            // until the whole app is quit and relaunched.
            if let Some(win) = app.get_webview_window("settings") {
                install_settings_close_handler(&win);
            }
            // Same for HUD — never destroy it on accidental close paths.
            if let Some(win) = app.get_webview_window("hud") {
                let w = win.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            if show_settings {
                reveal_settings(app.handle(), None);
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
            copy_text,
            quit_app,
            get_models,
            download_model,
            cancel_download,
            delete_model,
            open_models_folder,
            open_enhancement_log,
            clear_enhancement_log,
            add_enhancement_model,
            remove_enhancement_model,
            fetch_repo_gguf_files,
            clear_history,
            show_settings_window,
            list_microphones,
            default_microphone,
            catalog_stats,
            is_shortcut_available,
            export_settings_to,
            import_settings_from,
        ])
        .build(tauri::generate_context!())
        .expect("error while running OpenVoice")
        .run(|app, event| {
            // Clicking the Dock icon when no window is visible. Since ✕ only
            // hides the settings window, macOS sees zero visible windows and
            // would otherwise do nothing at all here.
            if let tauri::RunEvent::Reopen { .. } = event {
                reveal_settings(app, None);
            }
        });
}
