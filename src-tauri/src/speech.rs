// speech.rs — Local speech recognition via transcribe-cpp (GGUF) + Parakeet (ONNX).
// Model loads once and stays warm until the user switches or deletes it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::OnceLock;

static ORT_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// GGUF/GGML model inference (transcribe-cpp) — the single engine behind
/// every architecture in the model catalog (Whisper, Parakeet, Canary,
/// Moonshine, SenseVoice, GigaAM, Cohere, Voxtral, Qwen3-ASR,
/// Granite-Speech, FunASR, MedASR) auto-detected from the GGUF file header.
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
///
/// All GGUF/Metal work runs on one dedicated, persistent OS thread (below),
/// not wherever `tokio::task::spawn_blocking` or an ad-hoc `std::thread::spawn`
/// happens to land. transcribe-cpp's Metal backend keeps a residency-set
/// object alive across calls (`ggml_metal_rsets_init: ... keep_alive`); the
/// crate never documents it as safe to touch from a different OS thread than
/// the one that created it, and this app previously called into the cached
/// session from cargo's tokio blocking pool (a different thread most calls),
/// a fresh `std::thread::spawn` for background preloads, and Tauri's own
/// sync-command dispatch thread — a plausible cross-thread Metal/ggml
/// violation, and it matches the observed crash: `ggml-metal-device.m: GGML_ASSERT(
/// [rsets->data count] == 0) failed`. Giving the cache exclusive ownership to
/// one thread removes the cross-thread access entirely instead of guessing
/// at which specific call was unsafe.
enum GgufJob {
    EnsureLoaded {
        model_path: PathBuf,
        respond: std_mpsc::Sender<Result<(), String>>,
    },
    Unload,
    Run {
        samples: Vec<f32>,
        model_path: PathBuf,
        language: String,
        respond: std_mpsc::Sender<Result<String, String>>,
    },
}

fn gguf_worker() -> &'static std_mpsc::Sender<GgufJob> {
    static TX: OnceLock<std_mpsc::Sender<GgufJob>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = std_mpsc::channel::<GgufJob>();
        std::thread::Builder::new()
            .name("speech-gguf-worker".into())
            .spawn(move || {
                transcribe_cpp::init_logging();
                if let Err(e) = transcribe_cpp::init_backends_default() {
                    log::warn!("[speech] transcribe-cpp backend init failed: {e}");
                }

                // Owned exclusively by this thread — no Mutex needed, and no
                // other thread ever touches transcribe-cpp state.
                let mut cache: Option<(PathBuf, transcribe_cpp::Session)> = None;

                for job in rx {
                    match job {
                        GgufJob::EnsureLoaded {
                            model_path,
                            respond,
                        } => {
                            let result = ensure_loaded(&mut cache, &model_path);
                            let _ = respond.send(result);
                        }
                        GgufJob::Unload => {
                            cache = None;
                            log::info!("[speech] Unloaded cached model");
                        }
                        GgufJob::Run {
                            samples,
                            model_path,
                            language,
                            respond,
                        } => {
                            let result = ensure_loaded(&mut cache, &model_path).and_then(|()| {
                                let (_, session) = cache.as_mut().expect("just ensured loaded");
                                run_gguf_session(session, &samples, &language)
                            });
                            let _ = respond.send(result);
                        }
                    }
                }
            })
            .expect("failed to spawn speech-gguf-worker thread");
        tx
    })
}

fn ensure_loaded(
    cache: &mut Option<(PathBuf, transcribe_cpp::Session)>,
    model_path: &Path,
) -> Result<(), String> {
    if let Some((cached_path, _)) = cache.as_ref() {
        if cached_path == model_path {
            return Ok(());
        }
    }

    log::info!("[speech] Loading GGUF model from {}", model_path.display());
    let model = transcribe_cpp::Model::load(model_path)
        .map_err(|e| format!("Failed to load GGUF model: {e}"))?;
    let session = model
        .session()
        .map_err(|e| format!("Failed to create transcribe-cpp session: {e}"))?;

    *cache = Some((model_path.to_path_buf(), session));
    log::info!("[speech] GGUF model ready");
    Ok(())
}

fn run_gguf_session(
    session: &mut transcribe_cpp::Session,
    samples: &[f32],
    language: &str,
) -> Result<String, String> {
    use transcribe_cpp::{RunOptions, Task};

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

/// One-time transcribe-cpp init. Actually runs lazily on the dedicated GGUF
/// worker thread the first time it's touched — this just forces that thread
/// to spawn now, at app startup, instead of on the first real transcription.
pub fn init_transcribe_cpp() {
    let _ = gguf_worker();
}

/// Drop any cached model (call when the active model is deleted or switched).
pub fn unload_model() {
    let _ = gguf_worker().send(GgufJob::Unload);
}

/// Preload a model into memory so the first real transcription is fast.
pub fn preload_model(model_path: &Path) -> Result<(), String> {
    if model_path.is_dir() {
        // Parakeet is loaded per-call for now (ONNX session management is messier).
        return Ok(());
    }
    let (tx, rx) = std_mpsc::channel();
    gguf_worker()
        .send(GgufJob::EnsureLoaded {
            model_path: model_path.to_path_buf(),
            respond: tx,
        })
        .map_err(|_| "speech worker unavailable".to_string())?;
    rx.recv().map_err(|_| "speech worker gone".to_string())?
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
        let (tx, rx) = std_mpsc::channel();
        gguf_worker()
            .send(GgufJob::Run {
                samples: samples.to_vec(),
                model_path: model_path.to_path_buf(),
                language: language.to_string(),
                respond: tx,
            })
            .map_err(|_| "speech worker unavailable".to_string())?;
        rx.recv().map_err(|_| "speech worker gone".to_string())?
    }
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

/// Peak absolute sample value in a recorded WAV. Used to tell "no
/// microphone signal at all" (peak near true zero — almost always a denied
/// Microphone permission or wrong input device, since even a silent room has
/// some noise floor) apart from "quiet speech" (peak present, just under the
/// RMS silence threshold).
pub fn wav_peak_level(wav_path: &Path) -> Result<f32, String> {
    let samples = read_wav_samples(wav_path)?;
    Ok(samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs())))
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
