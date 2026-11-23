//! Wire-format adapters.
//!
//! Three dialects in, three dialects out. A request arriving as OpenAI chat is
//! parsed into [`crate::ir`] and rendered into whatever the chosen provider
//! speaks; the reply takes the same trip back. When both ends speak the same
//! dialect the gateway still goes through the IR for non-streaming replies (so
//! token accounting and error shaping are identical) but short-circuits the
//! stream path and copies the bytes, which is what keeps streaming latency flat.

pub mod anthropic;
pub mod gemini;
pub mod openai;

/// One `event:`/`data:` frame off the wire.
#[derive(Debug, Default, Clone)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

impl SseEvent {
    /// `[DONE]` ends an OpenAI stream.
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// Incremental parser for `text/event-stream`.
///
/// Handles both `\n\n` and `\r\n\r\n` frame separators and ignores comment
/// lines, which is what every provider actually emits.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((frame_end, skip)) = find_separator(&self.buf) {
            let frame: Vec<u8> = self.buf[..frame_end].to_vec();
            self.buf.drain(..frame_end + skip);
            if let Some(ev) = parse_frame(&frame) {
                out.push(ev);
            }
        }
        out
    }

    /// Flush a trailing frame that arrived without its blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if self.buf.is_empty() {
            return None;
        }
        let frame = std::mem::take(&mut self.buf);
        parse_frame(&frame)
    }
}

fn find_separator(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

fn parse_frame(frame: &[u8]) -> Option<SseEvent> {
    let text = String::from_utf8_lossy(frame);
    let mut event = None;
    let mut data = String::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() && event.is_none() {
        return None;
    }
    Some(SseEvent { event, data })
}

/// Build one SSE frame for a downstream client.
pub fn frame(event: Option<&str>, data: &str) -> String {
    match event {
        Some(e) => format!("event: {e}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

/// Trim a string field out of JSON without allocating twice.
pub fn jstr<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

pub fn ju64(v: &serde_json::Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}

pub fn jf32(v: &serde_json::Value, key: &str) -> Option<f32> {
    v.get(key).and_then(|x| x.as_f64()).map(|f| f as f32)
}

/// `stop` is `string` or `[string]` depending on the caller's mood.
pub fn jstr_list(v: &serde_json::Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}
