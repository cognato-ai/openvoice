// dictionary.rs — user vocabulary ("glossary") + correction memory.
//
// Two jobs:
//   1. A glossary of names/jargon/spellings the user cares about. It's injected
//      into the enhancement prompt as "known terms", so the LLM fixes homophones
//      and mangled proper nouns toward the exact spelling the user wants
//      (biasing the ASR model directly is impractical; the LLM pass is where we
//      already have leverage).
//   2. A memory of corrections. When the user fixes a pasted transcript in the
//      History view, we diff it and remember single-word substitutions. Once the
//      SAME substitution recurs `threshold` times, the corrected word is added
//      to the glossary automatically (if auto-add is on) so the fix sticks.
//
// Persisted as dictionary.json in the app-data dir, mirroring the settings /
// user-model stores in model_manager.

use serde::{Deserialize, Serialize};

use crate::model_manager::{app_data_dir, ensure_app_data_dir};

/// One glossary term. `sounds_like` is an optional phonetic hint ("Sean" heard
/// as "Shawn") the prompt shows the model; `auto` marks entries the correction
/// memory added on its own, so the UI can label them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictEntry {
    pub word: String,
    #[serde(default)]
    pub sounds_like: String,
    #[serde(default)]
    pub auto: bool,
}

/// A remembered `(heard -> corrected)` substitution and how often it's recurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Correction {
    pub heard: String,
    pub corrected: String,
    pub count: u32,
    pub last_seen: u64,
}

fn default_threshold() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dictionary {
    #[serde(default)]
    pub entries: Vec<DictEntry>,
    /// Auto-add frequently-corrected words to the glossary.
    #[serde(default = "default_true")]
    pub auto_add: bool,
    /// How many times the same correction must recur before it's auto-added.
    #[serde(default = "default_threshold")]
    pub threshold: u32,
    /// Correction memory (kept only until a pair auto-adds or is pruned).
    #[serde(default)]
    pub corrections: Vec<Correction>,
}

fn default_true() -> bool {
    true
}

impl Default for Dictionary {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            auto_add: true,
            threshold: default_threshold(),
            corrections: Vec::new(),
        }
    }
}

fn dictionary_path() -> std::path::PathBuf {
    app_data_dir().join("dictionary.json")
}

pub fn load() -> Dictionary {
    std::fs::read_to_string(dictionary_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(sanitize)
        .unwrap_or_default()
}

pub fn save(dict: &Dictionary) -> Result<(), String> {
    ensure_app_data_dir().map_err(|e| e.to_string())?;
    let clean = sanitize(dict.clone());
    let json = serde_json::to_string_pretty(&clean).map_err(|e| e.to_string())?;
    std::fs::write(dictionary_path(), json).map_err(|e| e.to_string())
}

/// Clamp/normalize an untrusted or stale dictionary (imported or hand-edited).
fn sanitize(mut d: Dictionary) -> Dictionary {
    d.threshold = d.threshold.clamp(1, 20);
    // Drop blank words and dedupe (case-insensitively) keeping the first.
    let mut seen = std::collections::HashSet::new();
    d.entries.retain(|e| {
        let w = e.word.trim();
        !w.is_empty() && w.chars().count() <= 80 && seen.insert(w.to_lowercase())
    });
    // Keep the store bounded.
    if d.entries.len() > 500 {
        d.entries.truncate(500);
    }
    if d.corrections.len() > 200 {
        // Keep the most-recent by last_seen.
        d.corrections.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        d.corrections.truncate(200);
    }
    d
}

/// Builds the "known terms" block appended to the enhancement system prompt.
/// Empty string when there are no terms (so the prompt is byte-identical to
/// today when the dictionary is unused).
pub fn glossary_prompt_block(dict: &Dictionary) -> String {
    if dict.entries.is_empty() {
        return String::new();
    }
    let mut lines = String::new();
    for e in &dict.entries {
        let w = e.word.trim();
        if w.is_empty() {
            continue;
        }
        let hint = e.sounds_like.trim();
        if hint.is_empty() {
            lines.push_str(&format!("- {w}\n"));
        } else {
            lines.push_str(&format!("- {w} (may be transcribed as \"{hint}\")\n"));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "Known terms — proper nouns, names, and jargon the user uses. When the transcript \
contains one of these (or a homophone/misspelling of it), use this exact spelling and \
capitalization. Do not otherwise force these words into the text.\n{lines}"
    )
}

// ── Correction memory ────────────────────────────────────────────────────────

/// A single-word substitution found by diffing `original` → `corrected`.
struct Sub {
    heard: String,
    corrected: String,
}

/// Splits keeping original casing, so we can tell a proper noun from a common
/// word.
fn tokenize_cased(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Finds single-word substitutions between the pasted text and the user's
/// edited version. Deliberately narrow: only positions where exactly one word
/// changed to another, AND the corrected word is Capitalized — i.e. a proper
/// noun / name. This is the high-value, low-noise case (names OpenVoice spelled
/// or capitalized wrong); lowercase grammar tweaks like "gonna" → "going" are
/// intentionally ignored so the auto-glossary doesn't fill with noise. Users can
/// always add lowercase jargon by hand.
fn find_substitutions(original: &str, corrected: &str) -> Vec<Sub> {
    let a = tokenize_cased(original);
    let b = tokenize_cased(corrected);

    // Only attempt on same-length token streams: a clean 1:1 alignment. Word
    // insertions/deletions shift everything and produce noise, so we skip them.
    if a.len() != b.len() || a.is_empty() {
        return Vec::new();
    }

    let mut subs = Vec::new();
    for i in 0..a.len() {
        // Case-sensitive compare so a pure capitalization fix (cognato →
        // Cognato) still registers as a change.
        if a[i] == b[i] {
            continue;
        }
        let corrected_word = &b[i];
        let is_capitalized = corrected_word
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false);
        if is_capitalized && corrected_word.chars().count() >= 2 {
            subs.push(Sub {
                heard: a[i].clone(),
                corrected: corrected_word.clone(),
            });
        }
    }
    subs
}

/// Result of feeding a correction back to the dictionary.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnResult {
    /// Words newly auto-added to the glossary this call (for the "Added X" toast).
    pub added: Vec<String>,
}

/// Records the user's edit of a pasted transcript. Diffs `original` (what was
/// pasted) against `corrected` (what the user changed it to), remembers the
/// single-word substitutions, and — if auto-add is on — promotes any that have
/// now recurred `threshold` times to the glossary. Returns the words added.
pub fn learn_from_edit(original: &str, corrected: &str) -> LearnResult {
    let mut result = LearnResult { added: Vec::new() };
    let subs = find_substitutions(original, corrected);
    if subs.is_empty() {
        return result;
    }

    let mut dict = load();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Index existing glossary words (lowercased) to avoid duplicate adds.
    let existing: std::collections::HashSet<String> =
        dict.entries.iter().map(|e| e.word.to_lowercase()).collect();
    let mut existing = existing;

    for sub in subs {
        let key_heard = sub.heard.to_lowercase();
        let key_corrected = sub.corrected.to_lowercase();
        // Skip only a true no-op (identical case included) or a word already in
        // the glossary. A capitalization-only fix (cognato → Cognato) IS worth
        // learning, so we compare the raw forms here, not the lowercased keys.
        if sub.heard == sub.corrected || existing.contains(&key_corrected) {
            continue;
        }

        // Bump (or create) the correction record for this exact pair.
        let entry = dict.corrections.iter_mut().find(|c| {
            c.heard.to_lowercase() == key_heard && c.corrected.to_lowercase() == key_corrected
        });
        let count = match entry {
            Some(c) => {
                c.count += 1;
                c.last_seen = now;
                c.count
            }
            None => {
                dict.corrections.push(Correction {
                    heard: sub.heard.clone(),
                    corrected: sub.corrected.clone(),
                    count: 1,
                    last_seen: now,
                });
                1
            }
        };

        if dict.auto_add && count >= dict.threshold {
            dict.entries.push(DictEntry {
                word: sub.corrected.clone(),
                sounds_like: sub.heard.clone(),
                auto: true,
            });
            existing.insert(key_corrected.clone());
            result.added.push(sub.corrected.clone());
            // Drop the now-satisfied correction records for this pair.
            dict.corrections
                .retain(|c| c.corrected.to_lowercase() != key_corrected);
        }
    }

    // Expire stale, low-count corrections (older than ~30 days) so a one-off
    // never lingers forever.
    let cutoff = now.saturating_sub(30 * 24 * 3600);
    dict.corrections
        .retain(|c| c.last_seen >= cutoff || c.count >= dict.threshold);

    let _ = save(&dict);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_detection() {
        // Capitalization-only fix of a name is caught.
        let subs = find_substitutions("email cognato about it", "email Cognato about it");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].corrected, "Cognato");
        assert_eq!(subs[0].heard, "cognato");

        // A mangled proper noun (spelling + capitalization) is caught.
        let subs = find_substitutions("deploy to kubernetis now", "deploy to Kubernetes now");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].corrected, "Kubernetes");

        // Lowercase grammar/function-word swaps are ignored (noise).
        let subs = find_substitutions("i am gonna go", "i am going go");
        assert!(subs.is_empty());

        // Length mismatch (word inserted) → no noisy diff.
        let subs = find_substitutions("send it now", "send it right now");
        assert!(subs.is_empty());
    }

    #[test]
    fn glossary_block_format() {
        let mut d = Dictionary::default();
        assert_eq!(glossary_prompt_block(&d), "");
        d.entries.push(DictEntry {
            word: "Cognato".into(),
            sounds_like: "cognato".into(),
            auto: false,
        });
        let block = glossary_prompt_block(&d);
        assert!(block.contains("Cognato"));
        assert!(block.contains("known terms") || block.contains("Known terms"));
    }
}
