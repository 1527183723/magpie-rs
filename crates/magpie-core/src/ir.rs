//! The intermediate representation every wire format is translated through.
//!
//! Upstream magpie does exactly this in `internal/gateway/ir.go`: parse the
//! caller's dialect into one shape, then render that shape into the provider's
//! dialect. Keeping the IR small is deliberate — every field here has to be
//! translated in both directions for three protocols, so the cost of adding one
//! is six code paths.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// One piece of a message. Text and images are what users send; tool blocks are
/// what agents send, and they are the reason a naive "just proxy the JSON" shim
/// breaks on Claude Code and Codex.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Image {
        /// A `data:` URL after normalization; remote URLs are passed through.
        source: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

impl Block {
    pub fn text(s: impl Into<String>) -> Block {
        Block::Text { text: s.into() }
    }

    /// Flatten a block to text, for the protocols that cannot carry tool blocks
    /// in a given position (Gemini's `system` slot, for instance).
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Block::Text { text } => Some(text),
            Block::ToolResult { content, .. } => Some(content),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Message {
        Message {
            role: Role::User,
            blocks: vec![Block::text(text)],
        }
    }

    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn has_tool_use(&self) -> bool {
        self.blocks
            .iter()
            .any(|b| matches!(b, Block::ToolUse { .. }))
    }
}

/// A tool the caller offers. `schema` is a JSON Schema object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "empty_object")]
    pub schema: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// The model decides.
    Auto,
    /// It must call some tool (Anthropic `any`, OpenAI `required`).
    Any,
    /// It may not call tools.
    None,
    /// It must call this exact tool.
    Tool(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatRequest {
    /// The model the caller asked for, already resolved to `provider/model` by
    /// the router; `upstream_model` is what actually goes on the wire.
    pub model: String,
    pub messages: Vec<Message>,
    /// System prompts are a separate field everywhere except OpenAI's chat
    /// format, so hoisting them is what makes translation lossless.
    pub system: Vec<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop: Vec<String>,
    pub stream: bool,
    pub tools: Vec<ToolDef>,
    pub tool_choice: Option<ToolChoice>,
    /// Anthropic `thinking` and friends ride along verbatim so a provider that
    /// understands them still gets them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub blocks: Vec<Block>,
    /// `end_turn`, `tool_use`, `max_tokens`, `stop_sequence` — normalized.
    pub stop_reason: String,
    pub usage: Usage,
}

impl ChatResponse {
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// One increment of a streamed answer. Every protocol's SSE events collapse
/// into this, and every protocol's SSE events are built back out of it.
#[derive(Debug, Clone)]
pub enum Delta {
    /// Text for the assistant's visible answer.
    Text(String),
    /// The model started a tool call: id + name are known, arguments follow.
    ToolStart { id: String, name: String },
    /// A fragment of that call's JSON arguments.
    ToolArgs(String),
    /// Token accounting, usually arriving at the end.
    Usage(Usage),
    /// Why generation stopped, once known.
    Stop(String),
}

/// Which dialect a byte stream is expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Openai,
    Anthropic,
    Gemini,
}
