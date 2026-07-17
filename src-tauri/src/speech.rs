// speech.rs — Local speech recognition via transcribe-cpp (GGUF) + Parakeet (ONNX).
// Model loads once and stays warm until the user switches or deletes it.

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static ORT_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// GGUF/GGML model cache (transcribe-cpp) — the single engine behind every
/// architecture in the model catalog (Whisper, Parakeet, Canary, Moonshine,
/// SenseVoice, GigaAM, Cohere, Voxtral, Qwen3-ASR, Granite-Speech, FunASR,
/// MedASR) auto-detected from the GGUF file header.
///
/// Whisper models used to run through the separate `whisper-rs` crate, which
/// vendors its own copy of ggml. Statically linking that alongside
/// transcribe-cpp-sys's vendored ggml meant two different ggml builds shared
/// the same binary with duplicate symbol names — the linker silently keeps
/// only one definition per symbol, so whisper-rs's compiled calls could end
/// up executing against transcribe-cpp's ggml internals (different struct
/// layouts/ABI), corrupting memory. Everything now goes through this one
/// engine instead, matching Handy (which has no whisper-rs dependency at all
/// — every catalog model, including Whisper, is GGUF via transcribe-cpp).
struct GgufCache {
    path: Option<PathBuf>,
    session: Option<transcribe_cpp::Session>,
}

static GGUF_CACHE: OnceLock<Mutex<GgufCache>> = OnceLock::new();

fn gguf_cache() -> &'static Mutex<GgufCache> {
    GGUF_CACHE.get_or_init(|| {
        Mutex::new(GgufCache {
            path: None,
            session: None,
        })
    })
}

/// One-time transcribe-cpp init (logging + compute backend selection). Call
/// once at app startup, before any GGUF model is loaded.
pub fn init_transcribe_cpp() {
    transcribe_cpp::init_logging();
    if let Err(e) = transcribe_cpp::init_backends_default() {
        log::warn!("[speech] transcribe-cpp backend init failed: {e}");
    }
}

fn ensure_gguf_loaded(model_path: &Path) -> Result<(), String> {
    let mut cache = gguf_cache().lock();
    if cache.path.as_ref().map(|p| p.as_path()) == Some(model_path) && cache.session.is_some() {
        return Ok(());
    }

    log::info!("[speech] Loading GGUF model from {}", model_path.display());
    let model = transcribe_cpp::Model::load(model_path)
        .map_err(|e| format!("Failed to load GGUF model: {e}"))?;
    let session = model
        .session()
        .map_err(|e| format!("Failed to create transcribe-cpp session: {e}"))?;

    cache.path = Some(model_path.to_path_buf());
    cache.session = Some(session);
    log::info!("[speech] GGUF model ready");
    Ok(())
}

/// Drop any cached model (call when the active model is deleted or switched).
pub fn unload_model() {
    let mut gguf = gguf_cache().lock();
    gguf.path = None;
    gguf.session = None;

    log::info!("[speech] Unloaded cached model");
}

/// Preload a model into memory so the first real transcription is fast.
pub fn preload_model(model_path: &Path) -> Result<(), String> {
    if model_path.is_dir() {
        // Parakeet is loaded per-call for now (ONNX session management is messier).
        return Ok(());
    }
    ensure_gguf_loaded(model_path)
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
    transcribe_samples(&samples, model_path, language, silence_threshold)
}

/// Same as `transcribe`, but for an in-memory sample buffer — used for both
/// the final transcript and the "live preview" partial re-transcribe of
/// audio captured so far while still recording.
pub fn transcribe_samples(
    samples: &[f32],
    model_path: &Path,
    language: &str,
    silence_threshold: f32,
) -> Result<String, String> {
    if samples.is_empty() {
        return Ok(String::new());
    }

    // Skip near-silence so we don't waste inference and spam empty results.
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    if rms < silence_threshold {
        return Ok(String::new());
    }

    if model_path.is_dir() {
        transcribe_parakeet(samples, model_path)
    } else {
        transcribe_gguf(samples, model_path, language)
    }
}

fn transcribe_gguf(samples: &[f32], model_path: &Path, language: &str) -> Result<String, String> {
    use transcribe_cpp::{RunOptions, Task};

    ensure_gguf_loaded(model_path)?;

    let mut cache = gguf_cache().lock();
    let session = cache
        .session
        .as_mut()
        .ok_or_else(|| "GGUF model not loaded".to_string())?;

    let run_options = RunOptions {
        task: Task::Transcribe,
        language: if language == "auto" {
            None
        } else {
            Some(language.to_string())
        },
        ..Default::default()
    };

    let result = session
        .run(samples, &run_options)
        .map_err(|e| format!("transcribe-cpp inference error: {e}"))?;

    Ok(result.text.trim().to_string())
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
