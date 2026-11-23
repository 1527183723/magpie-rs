//! The usage ledger: one JSON object per request, appended to `usage.jsonl`.
//!
//! Same file name and near-same shape as upstream (`internal/usage`), so the
//! numbers keep meaning what they meant before.

use crate::ir::Usage;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Ledger {
    path: PathBuf,
    lock: Mutex<()>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub provider: String,
    pub model: String,
    /// The model the caller asked for, which may differ from `model` when the
    /// caller used a `provider/model` alias or a bare name.
    pub requested: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub ms: u64,
    pub status: u16,
    pub stream: bool,
    /// `openai` or `anthropic` — the dialect the caller used.
    pub dialect: String,
}

impl Ledger {
    pub fn new(dir: impl AsRef<Path>) -> std::io::Result<Ledger> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        Ok(Ledger {
            path: dir.join("usage.jsonl"),
            lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, entry: &Entry) {
        let Ok(line) = serde_json::to_string(entry) else {
            return;
        };
        let _guard = self.lock.lock();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// Total tokens per provider over the whole file. Used by the web UI.
    pub fn totals(&self) -> Vec<(String, u64, u64, u64)> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut map: std::collections::BTreeMap<String, (u64, u64, u64)> =
            std::collections::BTreeMap::new();
        for line in text.lines().rev().take(20_000) {
            let Ok(e) = serde_json::from_str::<Entry>(line) else {
                continue;
            };
            let slot = map.entry(e.provider).or_default();
            slot.0 += e.input_tokens;
            slot.1 += e.output_tokens;
            slot.2 += 1;
        }
        map.into_iter().map(|(k, (i, o, n))| (k, i, o, n)).collect()
    }

    /// The most recent `n` entries, newest first.
    pub fn recent(&self, n: usize) -> Vec<Entry> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
            .take(n)
            .collect()
    }
}

/// Cheap token estimate for `/v1/messages/count_tokens`.
///
/// Real tokenizers are per-vendor and large; a 4-bytes-per-token heuristic plus
/// per-block overhead is what most gateways use for a pre-flight budget check,
/// and it is honest about being an estimate.
pub fn estimate_tokens(text: &str) -> u64 {
    let bytes = text.len() as u64;
    let words = text.split_whitespace().count() as u64;
    (bytes / 4).max(words).max(1)
}

pub fn estimate_request(req: &crate::ir::ChatRequest) -> Usage {
    let mut input = 0u64;
    for s in &req.system {
        input += estimate_tokens(s);
    }
    for m in &req.messages {
        input += 4;
        for b in &m.blocks {
            match b {
                crate::ir::Block::Text { text } => input += estimate_tokens(text),
                crate::ir::Block::Image { .. } => input += 1600,
                crate::ir::Block::ToolUse { name, input: v, .. } => {
                    input += estimate_tokens(name) + estimate_tokens(&v.to_string())
                }
                crate::ir::Block::ToolResult { content, .. } => input += estimate_tokens(content),
            }
        }
    }
    for t in &req.tools {
        input += estimate_tokens(&t.name)
            + estimate_tokens(&t.description)
            + estimate_tokens(&t.schema.to_string());
    }
    Usage {
        input_tokens: input,
        output_tokens: 0,
    }
}
