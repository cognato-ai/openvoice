// model_manager.rs — Whisper (runnable) + full Handy catalog (always embedded).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Full Handy catalog shipped in the binary so models always appear in the UI.
const EMBEDDED_CATALOG: &str = include_str!("../resources/catalog.json");

pub fn models_dir() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join("Library/Application Support/com.openvoice.app/models"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/openvoice_models"))
}

pub fn app_data_dir() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join("Library/Application Support/com.openvoice.app"))
        .unwrap_or_else(|_| PathBuf::from("/tmp/openvoice"))
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

pub fn ensure_models_dir() -> std::io::Result<()> {
    fs::create_dir_all(models_dir())
}

pub fn ensure_app_data_dir() -> std::io::Result<()> {
    fs::create_dir_all(app_data_dir())
}

pub fn model_path(name: &str) -> PathBuf {
    models_dir().join(name)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppModel {
    pub name: String,
    pub display_name: String,
    pub engine: String,
    pub architecture: String,
    pub size: String,
    pub size_bytes: u64,
    pub quality: String,
    pub speed: String,
    pub description: String,
    pub recommended: bool,
    pub advanced: bool,
    pub runnable: bool,
    pub downloaded: bool,
    pub languages: Vec<String>,
    pub family: String,
}

pub struct ModelFile {
    pub filename: &'static str,
    pub url: &'static str,
}

struct WhisperSpec {
    name: &'static str,
    display: &'static str,
    size_label: &'static str,
    size_bytes: u64,
    quality: &'static str,
    speed: &'static str,
    description: &'static str,
    recommended: bool,
    advanced: bool,
    languages: &'static [&'static str],
}

/// All Whisper GGML models we can run today via whisper-rs.
const WHISPER_MODELS: &[WhisperSpec] = &[
    WhisperSpec {
        name: "ggml-tiny.en.bin",
        display: "Whisper Tiny (English)",
        size_label: "75 MB",
        size_bytes: 75_000_000,
        quality: "Good enough",
        speed: "Instant",
        description: "Fastest English model — best first download",
        recommended: true,
        advanced: false,
        languages: &["en"],
    },
    WhisperSpec {
        name: "ggml-tiny.bin",
        display: "Whisper Tiny",
        size_label: "75 MB",
        size_bytes: 75_000_000,
        quality: "Good enough",
        speed: "Instant",
        description: "Multilingual tiny model",
        recommended: false,
        advanced: false,
        languages: &["multi"],
    },
    WhisperSpec {
        name: "ggml-base.en.bin",
        display: "Whisper Base (English)",
        size_label: "142 MB",
        size_bytes: 142_000_000,
        quality: "Great",
        speed: "Fast",
        description: "Sweet spot for everyday English typing",
        recommended: true,
        advanced: false,
        languages: &["en"],
    },
    WhisperSpec {
        name: "ggml-base.bin",
        display: "Whisper Base",
        size_label: "142 MB",
        size_bytes: 142_000_000,
        quality: "Great",
        speed: "Fast",
        description: "Multilingual base model",
        recommended: false,
        advanced: false,
        languages: &["multi"],
    },
    WhisperSpec {
        name: "ggml-small.en.bin",
        display: "Whisper Small (English)",
        size_label: "466 MB",
        size_bytes: 466_000_000,
        quality: "Excellent",
        speed: "Solid",
        description: "Higher accuracy English",
        recommended: false,
        advanced: false,
        languages: &["en"],
    },
    WhisperSpec {
        name: "ggml-small.bin",
        display: "Whisper Small",
        size_label: "466 MB",
        size_bytes: 466_000_000,
        quality: "Excellent",
        speed: "Solid",
        description: "Multilingual small model",
        recommended: false,
        advanced: false,
        languages: &["multi"],
    },
    WhisperSpec {
        name: "ggml-medium.en.bin",
        display: "Whisper Medium (English)",
        size_label: "1.5 GB",
        size_bytes: 1_500_000_000,
        quality: "Near perfect",
        speed: "Slower",
        description: "Heavy English model",
        recommended: false,
        advanced: true,
        languages: &["en"],
    },
    WhisperSpec {
        name: "ggml-medium.bin",
        display: "Whisper Medium",
        size_label: "1.5 GB",
        size_bytes: 1_500_000_000,
        quality: "Near perfect",
        speed: "Slower",
        description: "Broad multilingual accuracy",
        recommended: false,
        advanced: true,
        languages: &["multi"],
    },
    WhisperSpec {
        name: "ggml-large-v3-turbo.bin",
        display: "Whisper Large v3 Turbo",
        size_label: "1.6 GB",
        size_bytes: 1_600_000_000,
        quality: "Excellent",
        speed: "Fast for size",
        description: "Large quality with turbo speed",
        recommended: false,
        advanced: true,
        languages: &["multi"],
    },
    WhisperSpec {
        name: "ggml-large-v3.bin",
        display: "Whisper Large v3",
        size_label: "3.1 GB",
        size_bytes: 3_100_000_000,
        quality: "Best Whisper",
        speed: "Slow",
        description: "Highest Whisper accuracy",
        recommended: false,
        advanced: true,
        languages: &["multi"],
    },
    WhisperSpec {
        name: "parakeet-tdt-0.6b-v3",
        display: "Parakeet TDT 0.6B v3 (ONNX)",
        size_label: "2.6 GB",
        size_bytes: 2_600_000_000,
        quality: "Best",
        speed: "Very fast",
        description: "NVIDIA Parakeet ONNX — needs: brew install onnxruntime",
        recommended: false,
        advanced: true,
        languages: &["multi"],
    },
];

fn format_bytes(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1} GB", b as f64 / 1_000_000_000.0)
    } else if b >= 1_000_000 {
        format!("{} MB", (b + 500_000) / 1_000_000)
    } else {
        format!("{} KB", (b + 500) / 1000)
    }
}

pub fn is_model_downloaded(name: &str) -> bool {
    if name == "parakeet-tdt-0.6b-v3" {
        let dir = model_path(name);
        if !dir.is_dir() {
            return false;
        }
        let required = [
            "encoder-model.onnx",
            "encoder-model.onnx.data",
            "decoder_joint-model.onnx",
            "vocab.txt",
        ];
        return required.iter().all(|f| dir.join(f).exists());
    }
    if name.ends_with(".bin") {
        return model_path(name).exists();
    }
    // Catalog GGUF under models/catalog/<slug>/
    let dir = models_dir().join("catalog").join(name);
    if dir.is_dir() {
        return fs::read_dir(&dir)
            .map(|mut e| e.any(|x| x.map(|f| f.path().extension().map(|e| e == "gguf").unwrap_or(false)).unwrap_or(false)))
            .unwrap_or(false);
    }
    model_path(name).exists()
}

pub fn model_files(name: &str) -> Vec<ModelFile> {
    match name {
        "ggml-tiny.en.bin" => vec![ModelFile {
            filename: "ggml-tiny.en.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        }],
        "ggml-tiny.bin" => vec![ModelFile {
            filename: "ggml-tiny.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        }],
        "ggml-base.en.bin" => vec![ModelFile {
            filename: "ggml-base.en.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        }],
        "ggml-base.bin" => vec![ModelFile {
            filename: "ggml-base.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        }],
        "ggml-small.en.bin" => vec![ModelFile {
            filename: "ggml-small.en.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        }],
        "ggml-small.bin" => vec![ModelFile {
            filename: "ggml-small.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        }],
        "ggml-medium.en.bin" => vec![ModelFile {
            filename: "ggml-medium.en.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        }],
        "ggml-medium.bin" => vec![ModelFile {
            filename: "ggml-medium.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        }],
        "ggml-large-v3-turbo.bin" => vec![ModelFile {
            filename: "ggml-large-v3-turbo.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        }],
        "ggml-large-v3.bin" => vec![ModelFile {
            filename: "ggml-large-v3.bin",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        }],
        "parakeet-tdt-0.6b-v3" => vec![
            ModelFile {
                filename: "encoder-model.onnx",
                url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx",
            },
            ModelFile {
                filename: "encoder-model.onnx.data",
                url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx.data",
            },
            ModelFile {
                filename: "decoder_joint-model.onnx",
                url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.onnx",
            },
            ModelFile {
                filename: "vocab.txt",
                url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt",
            },
        ],
        _ => vec![],
    }
}

// ── Handy catalog ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CatalogRoot {
    models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogModel {
    id: String,
    slug: String,
    name: String,
    architecture: String,
    family: String,
    description: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    language_count: u32,
    #[serde(default)]
    recommended: bool,
    #[serde(default)]
    recommended_rank: Option<u32>,
    #[serde(default)]
    speed_score: Option<u32>,
    #[serde(default)]
    accuracy_score: Option<u32>,
    default_quant: String,
    files: Vec<CatalogFile>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogFile {
    filename: String,
    quant: String,
    size_bytes: u64,
}

fn load_catalog() -> &'static [CatalogModel] {
    static CATALOG: OnceLock<Vec<CatalogModel>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            // 1) Always try embedded catalog first (works in tauri dev + release)
            if let Ok(root) = serde_json::from_str::<CatalogRoot>(EMBEDDED_CATALOG) {
                log::info!("[catalog] embedded: {} models", root.models.len());
                return root.models;
            }
            // 2) Disk fallbacks
            let candidates = [
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("resources/catalog.json"))),
                std::env::current_exe().ok().and_then(|p| {
                    p.parent()
                        .map(|d| d.join("../Resources/resources/catalog.json"))
                }),
                Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/catalog.json")),
            ];
            for c in candidates.into_iter().flatten() {
                if let Ok(raw) = fs::read_to_string(&c) {
                    if let Ok(root) = serde_json::from_str::<CatalogRoot>(&raw) {
                        log::info!("[catalog] disk: {} from {}", root.models.len(), c.display());
                        return root.models;
                    }
                }
            }
            log::error!("[catalog] FAILED to load catalog.json");
            Vec::new()
        })
        .as_slice()
}

fn whisper_ggml_for_slug(slug: &str) -> Option<&'static str> {
    match slug {
        "whisper-tiny.en" => Some("ggml-tiny.en.bin"),
        "whisper-tiny" => Some("ggml-tiny.bin"),
        "whisper-base.en" => Some("ggml-base.en.bin"),
        "whisper-base" => Some("ggml-base.bin"),
        "whisper-small.en" => Some("ggml-small.en.bin"),
        "whisper-small" => Some("ggml-small.bin"),
        "whisper-medium.en" => Some("ggml-medium.en.bin"),
        "whisper-medium" => Some("ggml-medium.bin"),
        "whisper-large-v3-turbo" => Some("ggml-large-v3-turbo.bin"),
        "whisper-large-v3" | "whisper-large" | "whisper-large-v2" => Some("ggml-large-v3.bin"),
        _ => None,
    }
}

/// Full model list for the UI (runnable Whisper first, then entire Handy catalog).
pub fn list_models_for_ui() -> Vec<AppModel> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for w in WHISPER_MODELS {
        let name = w.name.to_string();
        seen.insert(name.clone());
        let engine = if name.starts_with("parakeet") {
            "parakeet"
        } else {
            "whisper"
        };
        out.push(AppModel {
            name: name.clone(),
            display_name: w.display.to_string(),
            engine: engine.into(),
            architecture: engine.into(),
            size: w.size_label.into(),
            size_bytes: w.size_bytes,
            quality: w.quality.into(),
            speed: w.speed.into(),
            description: w.description.into(),
            recommended: w.recommended,
            advanced: w.advanced,
            runnable: true,
            downloaded: is_model_downloaded(&name),
            languages: w.languages.iter().map(|s| (*s).to_string()).collect(),
            family: engine.into(),
        });
    }

    let mut catalog: Vec<&CatalogModel> = load_catalog().iter().collect();
    catalog.sort_by(|a, b| {
        match (a.recommended, b.recommended) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a
                .recommended_rank
                .unwrap_or(999)
                .cmp(&b.recommended_rank.unwrap_or(999))
                .then_with(|| {
                    b.accuracy_score
                        .unwrap_or(0)
                        .cmp(&a.accuracy_score.unwrap_or(0))
                })
                .then_with(|| a.name.cmp(&b.name)),
        }
    });

    for m in catalog {
        if let Some(ggml) = whisper_ggml_for_slug(&m.slug) {
            if seen.contains(ggml) {
                continue;
            }
        }
        if seen.contains(&m.slug) {
            continue;
        }
        seen.insert(m.slug.clone());

        let file = m
            .files
            .iter()
            .find(|f| f.quant == m.default_quant)
            .or_else(|| m.files.first());
        let size_bytes = file.map(|f| f.size_bytes).unwrap_or(0);

        let (name, runnable, downloaded) = if let Some(ggml) = whisper_ggml_for_slug(&m.slug) {
            (ggml.to_string(), true, is_model_downloaded(ggml))
        } else {
            let local = file
                .map(|f| {
                    models_dir()
                        .join("catalog")
                        .join(&m.slug)
                        .join(&f.filename)
                        .exists()
                })
                .unwrap_or(false);
            (m.slug.clone(), false, local)
        };

        let speed = m
            .speed_score
            .map(|s| format!("speed {s}"))
            .unwrap_or_else(|| "—".into());
        let quality = m
            .accuracy_score
            .map(|s| format!("acc {s}"))
            .unwrap_or_else(|| {
                if m.recommended {
                    "Recommended".into()
                } else {
                    "Catalog".into()
                }
            });

        let desc = if runnable {
            m.description.clone()
        } else {
            format!(
                "{} · GGUF catalog model (download OK; run needs full engine later)",
                m.description
            )
        };

        out.push(AppModel {
            name,
            display_name: m.name.clone(),
            engine: m.architecture.clone(),
            architecture: m.architecture.clone(),
            size: format_bytes(size_bytes),
            size_bytes,
            quality,
            speed,
            description: desc,
            recommended: m.recommended,
            advanced: !m.recommended,
            runnable,
            downloaded,
            languages: if m.languages.is_empty() {
                vec![format!("{} langs", m.language_count.max(1))]
            } else if m.languages.len() > 4 {
                vec![format!("{} languages", m.languages.len())]
            } else {
                m.languages.clone()
            },
            family: m.family.clone(),
        });
    }

    out
}

pub fn resolve_download(name: &str) -> Result<(PathBuf, Vec<(String, String)>), String> {
    let files = model_files(name);
    if !files.is_empty() {
        let dest = if name == "parakeet-tdt-0.6b-v3" {
            let d = model_path(name);
            fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            d
        } else {
            ensure_models_dir().map_err(|e| e.to_string())?;
            models_dir()
        };
        let list = files
            .into_iter()
            .map(|f| (f.filename.to_string(), f.url.to_string()))
            .collect();
        return Ok((dest, list));
    }

    let catalog = load_catalog();
    let m = catalog
        .iter()
        .find(|m| m.slug == name || m.id == name)
        .ok_or_else(|| format!("Unknown model: {name}"))?;

    if let Some(ggml) = whisper_ggml_for_slug(&m.slug) {
        return resolve_download(ggml);
    }

    let file = m
        .files
        .iter()
        .find(|f| f.quant == m.default_quant)
        .or_else(|| m.files.first())
        .ok_or_else(|| "No files in catalog entry".to_string())?;

    let dest = models_dir().join("catalog").join(&m.slug);
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        m.id, file.filename
    );
    Ok((dest, vec![(file.filename.clone(), url)]))
}

pub fn catalog_count() -> usize {
    load_catalog().len()
}
