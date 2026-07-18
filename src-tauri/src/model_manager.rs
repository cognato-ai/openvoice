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
    /// 1-indexed position after ranking by speed + accuracy (recommended
    /// models first). Lower is better — shown as a "#N" badge in the UI.
    pub rank: u32,
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

/// Whisper models run as GGUF through transcribe-cpp via the catalog below
/// (see `list_models_for_ui`) — matching Handy, which has no whisper-rs
/// dependency at all. This one hand-coded entry is the exception: Parakeet
/// TDT runs through parakeet-rs/ONNX Runtime, a completely separate engine
/// from transcribe-cpp, so it isn't part of the GGUF catalog.
const EXTRA_MODELS: &[WhisperSpec] = &[WhisperSpec {
    name: "parakeet-tdt-0.6b-v3",
    display: "Parakeet TDT 0.6B v3 (ONNX)",
    size_label: "2.6 GB",
    size_bytes: 2_600_000_000,
    quality: "Best",
    speed: "Very fast",
    description: "NVIDIA Parakeet ONNX — needs: brew install onnxruntime",
    recommended: true,
    advanced: true,
    languages: &["multi"],
}];

/// speed_score/accuracy_score aren't in the catalog for this hand-coded
/// entry, so give it numbers consistent with its "Very fast"/"Best" labels —
/// keeps it ranked alongside the top catalog picks instead of falling to
/// the bottom of the (score.unwrap_or(0)) sort below.
const PARAKEET_SPEED_SCORE: u32 = 90;
const PARAKEET_ACCURACY_SCORE: u32 = 90;

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
    // Catalog GGUF under models/catalog/<slug>/
    let dir = models_dir().join("catalog").join(name);
    if dir.is_dir() {
        return fs::read_dir(&dir)
            .map(|mut e| e.any(|x| x.map(|f| f.path().extension().map(|e| e == "gguf").unwrap_or(false)).unwrap_or(false)))
            .unwrap_or(false);
    }
    model_path(name).exists()
}

/// Resolves a model name to its actual file path on disk for transcription /
/// preloading. The Parakeet ONNX directory lives directly under
/// `models_dir()`; catalog GGUF downloads live nested under
/// `models_dir()/catalog/<slug>/<quant-file>.gguf`.
pub fn resolved_model_path(name: &str) -> PathBuf {
    if name == "parakeet-tdt-0.6b-v3" {
        return model_path(name);
    }
    let dir = models_dir().join("catalog").join(name);
    if dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "gguf").unwrap_or(false) {
                    return p;
                }
            }
        }
    }
    model_path(name)
}

pub fn model_files(name: &str) -> Vec<ModelFile> {
    match name {
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

/// Models empirically confirmed to return an empty transcript for real
/// speech via transcribe-cpp on this engine version (verified by feeding
/// synthesized speech directly to the engine, not a guess) — an upstream
/// architecture-specific decoder bug, not something fixable from our side.
/// canary-180m-flash was also the catalog's top-recommended, highest
/// speed-scored entry, so the ranking added in this same session actively
/// promoted it to #1 — exactly what a user would pick, and exactly what
/// silently produced nothing. Demoted here rather than deleted, since other
/// canary variants are untested and may be fine.
const KNOWN_BROKEN_SLUGS: &[&str] = &["canary-180m-flash"];

fn load_catalog() -> &'static [CatalogModel] {
    static CATALOG: OnceLock<Vec<CatalogModel>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            let mut models = load_catalog_raw();
            for m in &mut models {
                if KNOWN_BROKEN_SLUGS.contains(&m.slug.as_str()) {
                    m.recommended = false;
                    m.recommended_rank = None;
                    m.speed_score = None;
                    m.accuracy_score = None;
                    m.description = format!(
                        "⚠ Known issue: produces no transcript for real speech on this engine. {}",
                        m.description
                    );
                }
            }
            models
        })
        .as_slice()
}

fn load_catalog_raw() -> Vec<CatalogModel> {
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
}

/// Full model list for the UI, ranked by effectiveness: recommended models
/// first, then by combined speed + accuracy score (both out of ~100,
/// matching the catalog's scale) descending.
pub fn list_models_for_ui() -> Vec<AppModel> {
    let mut scored: Vec<(AppModel, bool, u32)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for w in EXTRA_MODELS {
        let name = w.name.to_string();
        seen.insert(name.clone());
        let engine = "parakeet";
        scored.push((
            AppModel {
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
                rank: 0,
            },
            w.recommended,
            PARAKEET_SPEED_SCORE + PARAKEET_ACCURACY_SCORE,
        ));
    }

    for m in load_catalog() {
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

        // Every catalog entry is a GGUF file transcribe-cpp can run —
        // architecture is auto-detected from the file header. Except known
        // broken ones (see KNOWN_BROKEN_SLUGS): mark unrunnable so the UI
        // can't be used to select them at all, not just deprioritized.
        let is_broken = KNOWN_BROKEN_SLUGS.contains(&m.slug.as_str());
        let (name, runnable, downloaded) =
            (m.slug.clone(), !is_broken, is_model_downloaded(&m.slug));

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

        let combined_score = m.speed_score.unwrap_or(0) + m.accuracy_score.unwrap_or(0);

        scored.push((
            AppModel {
                name,
                display_name: m.name.clone(),
                engine: m.architecture.clone(),
                architecture: m.architecture.clone(),
                size: format_bytes(size_bytes),
                size_bytes,
                quality,
                speed,
                description: m.description.clone(),
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
                rank: 0,
            },
            m.recommended,
            combined_score,
        ));
    }

    scored.sort_by(|(a_model, a_rec, a_score), (b_model, b_rec, b_score)| {
        b_rec
            .cmp(a_rec)
            .then_with(|| b_score.cmp(a_score))
            .then_with(|| a_model.name.cmp(&b_model.name))
    });

    scored
        .into_iter()
        .enumerate()
        .map(|(i, (mut model, _, _))| {
            model.rank = (i + 1) as u32;
            model
        })
        .collect()
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
