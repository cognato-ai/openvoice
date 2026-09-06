// llm.rs — Local text LLM for transcript enhancement (V2).
//
// Runs a small instruct model (Qwen3-0.6B, GGUF quantized) via Candle on Metal
// to clean up / reformat the raw ASR transcript before it's pasted. Candle is
// pure-Rust and vendors NO ggml C library, so it links cleanly beside
// transcribe-cpp-sys's statically-linked ggml — unlike a second ggml-based
// runner (llama-cpp-2), which would put two ggml copies in one binary and
// collide on duplicate symbols (the same ODR class of bug that made us drop
// whisper-rs; see the note in speech.rs).
//
// Like the GGUF speech engine, all Candle/Metal work runs on ONE dedicated,
// persistent OS thread that owns the loaded model exclusively — Metal state is
// never touched from more than the thread that created it. This mirrors
// speech.rs::gguf_worker exactly.

use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};
use candle_transformers::models::{
    quantized_llama, quantized_qwen2, quantized_qwen3,
};
use tokenizers::Tokenizer;

/// Chat family — selects both the Candle loader and the chat template. Mistral
/// shares the llama loader but uses a different prompt format, so it's distinct.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Family {
    Qwen3,
    Qwen2,
    Llama3,
    Mistral,
}

impl Family {
    fn parse(s: &str) -> Family {
        match s.trim().to_lowercase().as_str() {
            "qwen2" | "qwen2.5" | "qwen25" => Family::Qwen2,
            "llama" | "llama3" | "llama-3" | "llama3.1" | "llama3.2" | "smollm" => Family::Llama3,
            "mistral" => Family::Mistral,
            // Qwen3 is the default (covers our built-in models and any unknown).
            _ => Family::Qwen3,
        }
    }

    /// Special tokens that end generation for this family.
    fn eos_markers(self) -> &'static [&'static str] {
        match self {
            Family::Qwen3 | Family::Qwen2 => &["<|im_end|>", "<|endoftext|>"],
            Family::Llama3 => &["<|eot_id|>", "<|end_of_text|>"],
            Family::Mistral => &["</s>"],
        }
    }

    /// Wraps the system prompt + user text in this family's chat template.
    fn build_prompt(self, system: &str, user: &str) -> String {
        match self {
            // Qwen3: ChatML with thinking explicitly closed empty so it skips
            // its reasoning phase (faster; no <think> trace leaks).
            Family::Qwen3 => format!(
                "<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
            ),
            // Qwen2.5: plain ChatML (no thinking phase).
            Family::Qwen2 => format!(
                "<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n"
            ),
            // Llama 3.x chat template.
            Family::Llama3 => format!(
                "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{system}<|eot_id|><|start_header_id|>user<|end_header_id|>\n\n{user}<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\n"
            ),
            // Mistral Instruct — no system role, so fold it into the [INST] block.
            Family::Mistral => format!("<s>[INST] {system}\n\n{user} [/INST]"),
        }
    }
}

/// The loaded quantized model, wrapping the per-family Candle type behind a
/// uniform forward/clear interface. All three families expose the same
/// `from_gguf(ct, reader, device)` / `forward(&mut, &Tensor, pos)` /
/// `clear_kv_cache()` API (verified against candle-transformers 0.11).
enum LoadedModel {
    Qwen3(quantized_qwen3::ModelWeights),
    Qwen2(quantized_qwen2::ModelWeights),
    Llama(quantized_llama::ModelWeights),
}

impl LoadedModel {
    fn forward(&mut self, x: &Tensor, pos: usize) -> candle_core::Result<Tensor> {
        match self {
            LoadedModel::Qwen3(m) => m.forward(x, pos),
            LoadedModel::Qwen2(m) => m.forward(x, pos),
            LoadedModel::Llama(m) => m.forward(x, pos),
        }
    }

    fn clear_kv_cache(&mut self) {
        match self {
            LoadedModel::Qwen3(m) => m.clear_kv_cache(),
            LoadedModel::Qwen2(m) => m.clear_kv_cache(),
            LoadedModel::Llama(m) => m.clear_kv_cache(),
        }
    }
}

/// Reads the `openvoice.json` family sidecar written at download time.
/// Defaults to Qwen3 (our built-in models, and the safest fallback).
fn detect_family(dir: &Path) -> Family {
    let raw = std::fs::read_to_string(dir.join("openvoice.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("family")
                .and_then(|f| f.as_str())
                .map(|s| s.to_string())
        });
    Family::parse(raw.as_deref().unwrap_or("qwen3"))
}

/// Sampling knobs for one enhancement pass.
#[derive(Clone, Debug)]
pub struct GenParams {
    /// 0.0 → greedy/argmax (deterministic). Higher → more liberty. Kept low so
    /// the model reformats without drifting from what the user said.
    pub temperature: f32,
    /// Output token budget from settings: `0` = auto (scale to the input),
    /// `-1` = unconstrained (bounded only by the wall-clock guard), positive =
    /// a hard user-chosen cap.
    pub max_tokens: i32,
}

struct LoadedLlm {
    model: LoadedModel,
    family: Family,
    tokenizer: Tokenizer,
    device: Device,
    /// Token ids that end generation (family-specific EOS markers).
    eos_ids: Vec<u32>,
}

enum LlmJob {
    EnsureLoaded {
        model_dir: PathBuf,
        respond: std_mpsc::Sender<Result<(), String>>,
    },
    Unload,
    Run {
        system_prompt: String,
        user_text: String,
        params: GenParams,
        respond: std_mpsc::Sender<Result<String, String>>,
    },
}

fn llm_worker() -> &'static std_mpsc::Sender<LlmJob> {
    static TX: OnceLock<std_mpsc::Sender<LlmJob>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = std_mpsc::channel::<LlmJob>();
        std::thread::Builder::new()
            .name("llm-worker".into())
            .spawn(move || {
                // Owned exclusively by this thread — no Mutex, and no other
                // thread ever touches Candle/Metal state.
                let mut cache: Option<(PathBuf, LoadedLlm)> = None;

                for job in rx {
                    match job {
                        LlmJob::EnsureLoaded { model_dir, respond } => {
                            let result = ensure_loaded(&mut cache, &model_dir);
                            let _ = respond.send(result);
                        }
                        LlmJob::Unload => {
                            cache = None;
                            log::info!("[llm] Unloaded enhancement model");
                        }
                        LlmJob::Run {
                            system_prompt,
                            user_text,
                            params,
                            respond,
                        } => {
                            let result = run_job(&mut cache, &system_prompt, &user_text, &params);
                            let _ = respond.send(result);
                        }
                    }
                }
            })
            .expect("failed to spawn llm-worker thread");
        tx
    })
}

fn run_job(
    cache: &mut Option<(PathBuf, LoadedLlm)>,
    system_prompt: &str,
    user_text: &str,
    params: &GenParams,
) -> Result<String, String> {
    let (_, llm) = cache
        .as_mut()
        .ok_or_else(|| "enhancement model not loaded".to_string())?;
    run_generate(llm, system_prompt, user_text, params)
}

fn ensure_loaded(cache: &mut Option<(PathBuf, LoadedLlm)>, model_dir: &Path) -> Result<(), String> {
    if let Some((cached_dir, _)) = cache.as_ref() {
        if cached_dir == model_dir {
            return Ok(());
        }
    }

    let gguf_path = find_gguf(model_dir)?;
    let tokenizer_path = model_dir.join("tokenizer.json");
    if !tokenizer_path.exists() {
        return Err(format!(
            "tokenizer.json missing in {} — re-download the model",
            model_dir.display()
        ));
    }

    // Prefer Metal; fall back to CPU (still usable at 0.6B, just slower).
    let device = match Device::new_metal(0) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[llm] Metal unavailable ({e}); falling back to CPU");
            Device::Cpu
        }
    };

    let family = detect_family(model_dir);
    log::info!(
        "[llm] Loading enhancement model ({family:?}) from {}",
        gguf_path.display()
    );
    let mut file =
        std::fs::File::open(&gguf_path).map_err(|e| format!("open gguf: {e}"))?;
    let content =
        gguf_file::Content::read(&mut file).map_err(|e| format!("read gguf: {e}"))?;
    let mut model = match family {
        Family::Qwen3 => LoadedModel::Qwen3(
            quantized_qwen3::ModelWeights::from_gguf(content, &mut file, &device)
                .map_err(|e| format!("load qwen3 weights: {e}"))?,
        ),
        Family::Qwen2 => LoadedModel::Qwen2(
            quantized_qwen2::ModelWeights::from_gguf(content, &mut file, &device)
                .map_err(|e| format!("load qwen2 weights: {e}"))?,
        ),
        Family::Llama3 | Family::Mistral => LoadedModel::Llama(
            quantized_llama::ModelWeights::from_gguf(content, &mut file, &device)
                .map_err(|e| format!("load llama weights: {e}"))?,
        ),
    };

    // Warm up: run one throwaway forward pass NOW so Metal compiles its shaders
    // during load (off the hot path). Without this, the very first enhancement
    // triggers ~20s of one-time shader compilation, which blows past the
    // caller's timeout AND trips the generation wall-clock guard mid-decode,
    // truncating the output. After warmup, real calls run in well under a
    // second. Errors here are non-fatal — real inference will just pay the cost.
    if let Ok(warm) = Tensor::new(&[1u32, 2u32], &device).and_then(|t| t.unsqueeze(0)) {
        let t = Instant::now();
        let _ = model.forward(&warm, 0);
        model.clear_kv_cache();
        log::info!("[llm] Metal warmup done in {:?}", t.elapsed());
    }

    let tokenizer =
        Tokenizer::from_file(&tokenizer_path).map_err(|e| format!("load tokenizer: {e}"))?;

    let eos_ids = family
        .eos_markers()
        .iter()
        .filter_map(|t| tokenizer.token_to_id(t))
        .collect::<Vec<_>>();

    *cache = Some((
        model_dir.to_path_buf(),
        LoadedLlm {
            model,
            family,
            tokenizer,
            device,
            eos_ids,
        },
    ));
    log::info!("[llm] Enhancement model ready");
    Ok(())
}

fn run_generate(
    llm: &mut LoadedLlm,
    system: &str,
    user: &str,
    params: &GenParams,
) -> Result<String, String> {
    // The model is reused across calls, so wipe attention state each time.
    llm.model.clear_kv_cache();

    // Wrap in the loaded family's chat template (ChatML for Qwen, Llama-3
    // headers for Llama, [INST] for Mistral).
    let prompt = llm.family.build_prompt(system, user);

    let encoding = llm
        .tokenizer
        .encode(prompt, true)
        .map_err(|e| format!("tokenize: {e}"))?;
    let prompt_tokens = encoding.get_ids().to_vec();
    if prompt_tokens.is_empty() {
        return Err("empty prompt".into());
    }

    // Output budget. Reformatting scales with the input, but command mode ("write
    // me an email…") produces a NEW artifact far longer than the short input, so
    // the floor must be generous enough for a full email/note — otherwise the
    // output is truncated mid-sentence. The model emits EOS when genuinely done
    // (short cleanups stop early regardless), so a high cap only bounds runaways;
    // the wall-clock guard below is the hard backstop.
    let user_len = llm
        .tokenizer
        .encode(user, false)
        .map(|e| e.get_ids().len())
        .unwrap_or(64);
    // Honor the user's setting: -1 → unconstrained (a huge cap; the wall-clock
    // guard below is the real stop), positive → that exact cap, 0 → auto (scale
    // to the input, the default that suits both short cleanups and emails).
    let max_new = match params.max_tokens {
        -1 => usize::MAX,
        n if n > 0 => n as usize,
        _ => ((user_len as f32 * 2.0) as usize).clamp(320, 768),
    };

    // Internal hard backstop, scaled to the requested budget.
    let time_budget = gen_time_budget(params.max_tokens);

    let seed = 42;
    let sampling = if params.temperature <= 0.0 {
        Sampling::ArgMax
    } else {
        Sampling::All {
            temperature: params.temperature as f64,
        }
    };
    let mut logits_processor = LogitsProcessor::from_sampling(seed, sampling);

    let device = &llm.device;
    let start = Instant::now();

    // Prefill.
    let input = Tensor::new(prompt_tokens.as_slice(), device)
        .and_then(|t| t.unsqueeze(0))
        .map_err(|e| format!("input tensor: {e}"))?;
    let logits = llm
        .model
        .forward(&input, 0)
        .map_err(|e| format!("forward: {e}"))?;
    let mut next = sample_last(&mut logits_processor, &logits)?;

    let mut out_tokens: Vec<u32> = Vec::new();
    let mut pos = prompt_tokens.len();
    for _ in 0..max_new {
        if llm.eos_ids.contains(&next) {
            break;
        }
        out_tokens.push(next);
        // Hard backstop against a runaway. Return an error (not the partial) so
        // the caller falls back to the raw transcript rather than pasting a
        // truncated artifact.
        if start.elapsed() > time_budget {
            log::warn!("[llm] generation wall-clock guard tripped");
            return Err("enhancement exceeded time budget".into());
        }
        let input = Tensor::new(&[next], device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| format!("input tensor: {e}"))?;
        let logits = llm
            .model
            .forward(&input, pos)
            .map_err(|e| format!("forward: {e}"))?;
        next = sample_last(&mut logits_processor, &logits)?;
        pos += 1;
    }

    // Decode with skip_special_tokens=FALSE: this tokenizer's skip=true path
    // corrupts byte-level BPE reconstruction, dropping everything after the
    // first newline (so multi-line output like bullet lists was truncated to
    // its first line). `out_tokens` already excludes the EOS token — the
    // generation loop breaks before pushing it — so any stray ChatML markers
    // are stripped textually below instead.
    let text = llm
        .tokenizer
        .decode(&out_tokens, false)
        .map_err(|e| format!("decode: {e}"))?;
    let text = strip_special_markers(&text);

    if std::env::var("OPENVOICE_LLM_DEBUG").is_ok() {
        let hit_eos = llm.eos_ids.contains(&next);
        eprintln!(
            "[llm-debug] out_tokens={} hit_eos={} max_new={} text={:?}",
            out_tokens.len(),
            hit_eos,
            max_new,
            text
        );
    }

    Ok(strip_think(text.trim()).to_string())
}

/// Reduce whatever rank the model returns (e.g. (1, vocab) or (1, seq, vocab))
/// to the final-position 1-D vocab logits, then sample.
fn sample_last(lp: &mut LogitsProcessor, logits: &Tensor) -> Result<u32, String> {
    let mut l = logits.clone();
    while l.dims().len() > 1 {
        let d0 = l.dim(0).map_err(|e| format!("logits dim: {e}"))?;
        l = l.get(d0 - 1).map_err(|e| format!("logits narrow: {e}"))?;
    }
    lp.sample(&l).map_err(|e| format!("sample: {e}"))
}

/// Removes any residual ChatML special markers (`<|...|>`) that decoding with
/// skip_special_tokens=false may leave in the text.
fn strip_special_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("|>") {
            rest = &rest[start + end + 2..];
        } else {
            rest = &rest[start..];
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Defensive: if a <think>…</think> block ever slips through, drop it and keep
/// only what follows.
fn strip_think(s: &str) -> &str {
    if let Some(end) = s.rfind("</think>") {
        s[end + "</think>".len()..].trim_start()
    } else {
        s
    }
}

fn find_gguf(dir: &Path) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read model dir: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
            return Ok(path);
        }
    }
    Err(format!("no .gguf file in {}", dir.display()))
}

// ── Public API (mirrors speech.rs) ──────────────────────────────────────────

/// Force the worker thread to spawn now (at startup) rather than on first use.
pub fn init_llm() {
    let _ = llm_worker();
}

/// Drop the cached enhancement model.
pub fn unload_llm() {
    let _ = llm_worker().send(LlmJob::Unload);
}

/// Load a model directory (containing the GGUF + tokenizer.json) into memory so
/// the first enhancement is fast. Blocks until loaded.
pub fn preload_llm(model_dir: &Path) -> Result<(), String> {
    let (tx, rx) = std_mpsc::channel();
    llm_worker()
        .send(LlmJob::EnsureLoaded {
            model_dir: model_dir.to_path_buf(),
            respond: tx,
        })
        .map_err(|_| "llm worker unavailable".to_string())?;
    rx.recv().map_err(|_| "llm worker gone".to_string())?
}

/// Enhance `text` with the given system prompt. Blocks on the worker with a
/// timeout — on ANY failure (timeout, load error, generation error) the caller
/// falls back to the raw transcript, so this never blocks output for long.
pub fn enhance(
    model_dir: &Path,
    system_prompt: &str,
    text: &str,
    params: GenParams,
    timeout: Duration,
) -> Result<String, String> {
    // Ensure loaded first (blocking, but a no-op once warm).
    preload_llm(model_dir)?;

    let (tx, rx) = std_mpsc::channel();
    llm_worker()
        .send(LlmJob::Run {
            system_prompt: system_prompt.to_string(),
            user_text: text.to_string(),
            params,
            respond: tx,
        })
        .map_err(|_| "llm worker unavailable".to_string())?;

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(std_mpsc::RecvTimeoutError::Timeout) => Err("enhancement timed out".to_string()),
        Err(std_mpsc::RecvTimeoutError::Disconnected) => Err("llm worker gone".to_string()),
    }
}

// ── Prompt construction ─────────────────────────────────────────────────────

/// Rewrite levels 1–3 (the user-facing intensity). Level 2+ deliberately tells
/// the model that near-identical output is a failure — the forcing language is
/// what makes a small model actually reorganize instead of parroting the input.
const LEVEL_1: &str = "Rewrite level: 1 (light).\nFix grammar and agreement, split run-on \
sentences at natural boundaries, and add paragraph breaks. Keep the user's wording and sentence \
order.";
const LEVEL_2: &str = "Rewrite level: 2 (medium). You MUST make visible changes.\nTighten wordy \
phrasing, reorder clauses for readability, merge redundant sentences, convert clear enumerations \
into a list, and add a heading if the text is long enough to need one. Keep every fact and the \
user's voice and vocabulary. Expect the output to be noticeably shorter and better organized than \
the input — output that is nearly identical to the input is a failure at this level.";
const LEVEL_3: &str = "Rewrite level: 3 (heavy). You MUST substantially rewrite.\nProduce the \
polished text the user was clearly aiming at. Restructure freely: reorder for logical flow, cut \
repetition entirely, choose the register the destination calls for, and add the structural \
furniture the format needs (subject line, greeting, sign-off, bullets, headings) even if the user \
never spoke them. Every fact from the transcript must survive; nothing new may be added. Output \
that closely tracks the input's original sentence order is a failure at this level.";

/// Appended when "Follow spoken commands" is on. Teaches the model to act on
/// instructions embedded in the transcript rather than transcribe them.
const INSTRUCTION_BLOCK: &str = "Instruction handling:\nThe transcript may contain an instruction \
addressed to you rather than content to transcribe — for example \"make this more formal\", \"turn \
this into bullet points\", \"write me an email to Priya asking for the Q3 numbers\", \"summarize \
this in two lines\", \"reply saying I can't make it\".\nWhen you detect one:\n- Carry it out. The \
instruction overrides the rewrite level above.\n- The instruction words themselves must NOT appear \
in the output.\n- If the instruction asks you to produce a new artifact (an email, a message, a \
note), produce the finished artifact, not a description of it.\n- If part of the transcript is an \
instruction and the rest is content, apply the instruction to that content.\n- If the whole \
transcript is an instruction with no content, write the thing it asks for.\nOnly treat it as an \
instruction if it is clearly addressed to you. \"I told him to make it more formal\" is content, \
not an instruction.";

/// Builds the system prompt. Structure: hard rules + destination-app conventions
/// (BASE) → rewrite level (from intensity) → optional instruction handling.
///
/// - `mode` selects the destination the model should match (`auto` uses the
///   real foreground `app`; the others are fixed destinations).
/// - `voice_commands` appends the instruction-handling block.
/// - `grounding` is optional pre-composed context (glossary terms, on-screen
///   context) appended verbatim; empty string adds nothing.
pub fn system_prompt(
    mode: &str,
    intensity: &str,
    custom: &str,
    app: Option<&str>,
    voice_commands: bool,
    grounding: &str,
) -> String {
    // Destination that fills the {app} slot — either the detected app (auto) or
    // a fixed descriptor for the manual modes.
    let dest: String = match mode {
        "auto" | "custom" => app.unwrap_or("a plain text field").to_string(),
        "email" => "an email (a clear, professional email body; greeting/sign-off only if \
appropriate)".to_string(),
        "message" => "a casual chat or messaging app (friendly, concise)".to_string(),
        "notes" => "a notes app (short lines and bullet points where they help)".to_string(),
        // clean / unknown
        _ => "a plain text field".to_string(),
    };

    let base = format!(
        "You are a dictation post-processor. You receive a raw speech-to-text transcript and \
produce the final written text the user intended.\n\nHard rules (never violate):\n\
- Never translate. Output in the same language as the transcript.\n\
- Never invent facts, names, numbers, or dates that aren't in the transcript or supplied by the \
user's own instruction.\n\
- Never add preamble, commentary, quotes, code fences, or explanations. Output only the final \
text.\n\
- Never ask questions or refuse. If the transcript is empty or unintelligible, return it \
unchanged.\n\
- Always apply correct capitalization and terminal punctuation.\n\
- Fix obvious speech-to-text errors using context (homophones, mangled proper nouns, run-together \
words).\n\
- Remove fillers and disfluencies: um, uh, like, you know, false starts, immediate repeats, \
self-corrections (keep only the corrected version).\n\n\
Destination app: {dest}\nMatch the conventions of {dest}: register, length, greeting/sign-off, \
formatting, and whether markdown is appropriate."
    );

    let level = match intensity {
        "light" => LEVEL_1,
        "strong" => LEVEL_3,
        _ => LEVEL_2, // balanced
    };

    let mut out = format!("{base}\n\n{level}");

    if mode == "custom" {
        let c = custom.trim();
        if !c.is_empty() {
            out.push_str(&format!("\n\nAdditional standing instruction from the user: {c}"));
        }
    }

    if voice_commands {
        out.push_str("\n\n");
        out.push_str(INSTRUCTION_BLOCK);
    }

    let g = grounding.trim();
    if !g.is_empty() {
        out.push_str("\n\n");
        out.push_str(g);
    }

    out
}

/// Wall-clock budget for one generation, derived from the token setting so that
/// a larger/unconstrained cap gets proportionally longer to finish instead of
/// being cut off and discarded. Conservatively assumes ~20 tok/s (worst case,
/// 1.7B on Metal) and always allows at least the default window. Even
/// "unconstrained" is time-bounded so the paste can never hang forever.
pub fn gen_time_budget(max_tokens: i32) -> Duration {
    const DEFAULT_SECS: u64 = 14;
    const CEILING_SECS: u64 = 180;
    let secs = match max_tokens {
        -1 => 90, // unconstrained: generous but finite
        n if n > 0 => (n as u64 / 20).max(DEFAULT_SECS),
        _ => DEFAULT_SECS, // auto
    };
    Duration::from_secs(secs.min(CEILING_SECS))
}

/// Maps an intensity label to a sampling temperature.
pub fn temperature_for(intensity: &str) -> f32 {
    match intensity {
        "light" => 0.0,
        "strong" => 0.3,
        _ => 0.2, // balanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_composition() {
        // Auto mode fills the destination with the detected app name.
        let auto = system_prompt("auto", "balanced", "", Some("Slack"), false, "");
        assert!(auto.contains("Destination app: Slack"));

        // Manual modes fill a fixed destination.
        assert!(system_prompt("email", "balanced", "", None, false, "").contains("an email"));

        // Rewrite levels map from intensity, with the forcing language at 2/3.
        assert!(system_prompt("clean", "light", "", None, false, "").contains("level: 1"));
        assert!(system_prompt("clean", "balanced", "", None, false, "")
            .contains("You MUST make visible changes"));
        assert!(system_prompt("clean", "strong", "", None, false, "")
            .contains("You MUST substantially rewrite"));

        // Instruction block only present when voice commands are on.
        let vc = system_prompt("clean", "balanced", "", None, true, "");
        assert!(vc.contains("Instruction handling:"));
        assert!(!system_prompt("clean", "balanced", "", None, false, "")
            .contains("Instruction handling:"));

        // Custom appends the standing instruction.
        let c = system_prompt("custom", "balanced", "Write in pirate speak", None, false, "");
        assert!(c.contains("Write in pirate speak"));

        // Grounding context is appended when present.
        let g = system_prompt("clean", "balanced", "", None, false, "Known terms:\n- Cognato");
        assert!(g.contains("Cognato"));
    }

    // Manual integration smoke test: verifies Candle loads the Qwen3 GGUF and
    // produces coherent, complete (multi-line-safe) output for each mode.
    // Skipped unless the model is downloaded. Run with:
    //   cargo test --lib llm::tests::enhance_smoke -- --exact --ignored --nocapture
    #[test]
    #[ignore]
    fn enhance_smoke() {
        run_smoke("qwen3-0.6b");
    }

    #[test]
    #[ignore]
    fn enhance_smoke_17b() {
        run_smoke("qwen3-1.7b");
    }

    fn run_smoke(slug: &str) {
        let home = std::env::var("HOME").unwrap();
        let dir = std::path::PathBuf::from(home)
            .join("Library/Application Support/com.openvoice.app/models/catalog")
            .join(slug);
        if !dir.join("tokenizer.json").exists() || find_gguf(&dir).is_err() {
            eprintln!("SKIP: model not downloaded at {}", dir.display());
            return;
        }
        eprintln!("\n########## {slug} ##########");
        // (mode, intensity, app, voice_commands, input).
        let cases = [
            ("clean", "balanced", None, false, "yeah so um i think we should uh probably ship the the feature on friday you know if the tests pass"),
            ("email", "balanced", None, false, "hey just wanted to check if you got the report i sent yesterday let me know"),
            ("notes", "balanced", None, false, "okay so first we need to fix the login bug then update the docs and also uh talk to the design team about the new icons"),
            ("auto", "balanced", Some("Slack"), false, "hey um can you send me the slides when you get a chance no rush thanks"),
            ("auto", "strong", Some("Mail"), false, "hey just wanted to check if you got the report i sent yesterday let me know"),
            ("clean", "balanced", None, true, "so the login is broken um can you make this sound more professional"),
        ];
        for (mode, intensity, app, vc, input) in cases {
            let sys = system_prompt(mode, intensity, "", app, vc, "");
            let out = enhance(
                &dir,
                &sys,
                input,
                GenParams { temperature: temperature_for(intensity), max_tokens: 0 },
                Duration::from_secs(90),
            )
            .expect("enhance failed");
            eprintln!(
                "\n[{mode} · {intensity}{}{}]\n  IN : {input}\n  OUT: {out}",
                app.map(|a| format!(" · {a}")).unwrap_or_default(),
                if vc { " · voice-cmd" } else { "" }
            );
            assert!(!out.trim().is_empty(), "empty output for {mode}");
            assert!(!out.contains("<think>"), "think leaked in {mode}");
            assert!(!out.contains("<|"), "special marker leaked in {mode}");
        }
    }
}
