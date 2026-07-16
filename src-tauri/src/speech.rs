// speech.rs — Local speech recognition with cached Whisper contexts.
// Model loads once and stays warm until the user switches or deletes it.

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static ORT_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// In-memory Whisper model cache. Loading a 75–466 MB model from disk every
/// transcription is the main reason OpenVoice felt broken/slow.
struct WhisperCache {
    path: Option<PathBuf>,
    // WhisperContext is Send+Sync in whisper-rs 0.13
    ctx: Option<whisper_rs::WhisperContext>,
}

static WHISPER_CACHE: OnceLock<Mutex<WhisperCache>> = OnceLock::new();

fn whisper_cache() -> &'static Mutex<WhisperCache> {
    WHISPER_CACHE.get_or_init(|| {
        Mutex::new(WhisperCache {
            path: None,
            ctx: None,
        })
    })
}

/// Drop any cached model (call when the active model is deleted or switched).
pub fn unload_model() {
    let mut cache = whisper_cache().lock();
    cache.path = None;
    cache.ctx = None;
    log::info!("[speech] Unloaded cached Whisper model");
}

/// Preload a Whisper model into memory so the first real transcription is fast.
pub fn preload_model(model_path: &Path) -> Result<(), String> {
    if model_path.is_dir() {
        // Parakeet is loaded per-call for now (ONNX session management is messier).
        return Ok(());
    }
    ensure_whisper_loaded(model_path)?;
    Ok(())
}

fn ensure_whisper_loaded(
    model_path: &Path,
) -> Result<(), String> {
    use whisper_rs::{WhisperContext, WhisperContextParameters};

    let mut cache = whisper_cache().lock();
    if cache.path.as_ref().map(|p| p.as_path()) == Some(model_path) && cache.ctx.is_some() {
        return Ok(());
    }

    log::info!("[speech] Loading Whisper model from {}", model_path.display());
    let ctx = WhisperContext::new_with_params(
        model_path
            .to_str()
            .ok_or_else(|| "Invalid model path".to_string())?,
        WhisperContextParameters::default(),
    )
    .map_err(|e| format!("Failed to load Whisper model: {e}"))?;

    cache.path = Some(model_path.to_path_buf());
    cache.ctx = Some(ctx);
    log::info!("[speech] Whisper model ready");
    Ok(())
}

fn get_ort_dylib_path() -> Result<PathBuf, String> {
    if let Ok(env_path) = std::env::var("ORT_DYLIB_PATH") {
        let path = PathBuf::from(env_path);
        if path.exists() {
            return Ok(path);
        }
    }

    let paths = [
        "/opt/homebrew/opt/onnxruntime/lib/libonnxruntime.dylib",
        "/usr/local/opt/onnxruntime/lib/libonnxruntime.dylib",
    ];

    for path in paths {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    Err(
        "ONNX Runtime not found. Parakeet needs it — install with: brew install onnxruntime"
            .into(),
    )
}

fn init_ort() -> Result<(), String> {
    if !ORT_INITIALIZED.load(Ordering::SeqCst) {
        let dylib_path = get_ort_dylib_path()?;
        ort::init_from(dylib_path)
            .map_err(|e| format!("Failed to initialize ONNX Runtime: {e}"))?;
        ORT_INITIALIZED.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Transcribes a 16 kHz mono f32 WAV using Whisper (cached) or Parakeet.
pub fn transcribe(
    wav_path: &Path,
    model_path: &Path,
    language: &str,
    silence_threshold: f32,
) -> Result<String, String> {
    let samples = read_wav_samples(wav_path)?;
    if samples.is_empty() {
        return Ok(String::new());
    }

    // Skip near-silence so we don't waste inference and spam empty results.
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    if rms < silence_threshold {
        log::info!("[speech] Audio too quiet (rms={rms:.5}), skipping");
        return Ok(String::new());
    }

    if model_path.is_dir() {
        transcribe_parakeet(&samples, model_path)
    } else {
        transcribe_whisper(&samples, model_path, language)
    }
}

fn transcribe_whisper(samples: &[f32], model_path: &Path, language: &str) -> Result<String, String> {
    use whisper_rs::{FullParams, SamplingStrategy};

    ensure_whisper_loaded(model_path)?;

    let cache = whisper_cache().lock();
    let ctx = cache
        .ctx
        .as_ref()
        .ok_or_else(|| "Whisper model not loaded".to_string())?;

    let mut state = ctx.create_state().map_err(|e| e.to_string())?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // "auto" lets Whisper detect the spoken language; otherwise force decoding
    // in the requested language (English-only model files ignore this anyway).
    params.set_language(if language == "auto" { None } else { Some(language) });
    // Suppress common Whisper hallucination on short clips
    params.set_suppress_blank(true);
    params.set_no_speech_thold(0.6);

    state
        .full(params, samples)
        .map_err(|e| format!("Whisper inference error: {e}"))?;

    let num_segments = state.full_n_segments().map_err(|e| e.to_string())?;
    let mut result = String::new();
    for i in 0..num_segments {
        if let Ok(text) = state.full_get_segment_text(i) {
            result.push_str(&text);
        }
    }

    // Clean common whisper artifacts
    let cleaned = result
        .trim()
        .trim_matches(|c| c == '[' || c == ']' || c == '(' || c == ')')
        .trim();

    // Drop pure noise tokens Whisper sometimes emits
    let lower = cleaned.to_lowercase();
    if lower.is_empty()
        || lower == "you"
        || lower == "thank you"
        || lower == "thanks for watching"
        || lower.starts_with("[blank")
        || lower.starts_with("(blank")
    {
        // Keep short real utterances; only drop known hallucinations when very short audio
        if samples.len() < 16_000 * 2 && (lower == "you" || lower.contains("blank")) {
            return Ok(String::new());
        }
    }

    Ok(cleaned.to_string())
}

fn transcribe_parakeet(samples: &[f32], model_dir: &Path) -> Result<String, String> {
    use parakeet_rs::{ParakeetTDT, Transcriber};

    init_ort()?;

    let mut parakeet = ParakeetTDT::from_pretrained(
        model_dir
            .to_str()
            .ok_or_else(|| "Invalid Parakeet model path".to_string())?,
        None,
    )
    .map_err(|e| format!("Failed to load Parakeet model: {e}"))?;

    let result = parakeet
        .transcribe_samples(samples.to_vec(), 16000, 1, None)
        .map_err(|e| format!("Parakeet inference error: {e}"))?;

    Ok(result.text.trim().to_string())
}

fn read_wav_samples(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| {
                s.map(|v| v as f32 / i16::MAX as f32)
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
    };

    Ok(samples)
}
