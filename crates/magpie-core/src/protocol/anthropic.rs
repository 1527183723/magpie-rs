//! Anthropic Messages dialect (`/v1/messages`). Claude Code speaks this and
//! nothing else, which is why it is a first-class input here and not a shim.

use crate::ir::*;
use crate::protocol::{jf32, jstr, jstr_list, ju64};
use serde_json::{Value, json};

fn flatten(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => {
            let parts: Vec<String> = a
                .iter()
                .filter_map(|p| {
                    if jstr(p, "type") == Some("text") {
                        jstr(p, "text").map(str::to_string)
                    } else {
                        None
                    }
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

fn image_to_ir(source: &Value) -> Option<Block> {
    match jstr(source, "type") {
        Some("base64") => {
            let media = jstr(source, "media_type").unwrap_or("image/png");
            let data = jstr(source, "data").unwrap_or_default();
            Some(Block::Image {
                source: format!("data:{media};base64,{data}"),
            })
        }
        _ => jstr(source, "url").map(|u| Block::Image {
            source: u.to_string(),
        }),
    }
}

fn image_to_anthropic(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:")
        && let Some((meta, data)) = rest.split_once(";base64,")
    {
        return json!({"type": "base64", "media_type": meta, "data": data});
    }
    json!({"type": "url", "url": url})
}

fn blocks_from_content(content: Option<&Value>) -> Vec<Block> {
    match content {
        Some(Value::String(s)) => vec![Block::text(s.clone())],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|p| match jstr(p, "type").unwrap_or("text") {
                "text" => jstr(p, "text").map(Block::text),
                "image" => p.get("source").and_then(image_to_ir),
                "tool_use" => Some(Block::ToolUse {
                    id: jstr(p, "id").unwrap_or_default().to_string(),
                    name: jstr(p, "name").unwrap_or_default().to_string(),
                    input: p.get("input").cloned().unwrap_or(json!({})),
                }),
                "tool_result" => Some(Block::ToolResult {
                    tool_use_id: jstr(p, "tool_use_id").unwrap_or_default().to_string(),
                    content: flatten(p.get("content")).unwrap_or_default(),
                    is_error: p.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false),
                }),
                // `thinking` and `redacted_thinking` blocks are dropped: they are
                // only meaningful back to the same vendor.
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn parse_request(v: &Value) -> Result<ChatRequest, String> {
    let model = jstr(v, "model").ok_or("`model` is required")?.to_string();
    let mut req = ChatRequest {
        model,
        stream: v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false),
        max_tokens: ju64(v, "max_tokens").map(|n| n as u32),
        temperature: jf32(v, "temperature"),
        top_p: jf32(v, "top_p"),
        stop: jstr_list(v, "stop_sequences"),
        ..Default::default()
    };

    if let Some(sys) = flatten(v.get("system")) {
        req.system.push(sys);
    }

    let messages = v
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or("`messages` is required")?;

    for msg in messages {
        let role = match jstr(msg, "role").unwrap_or("user") {
            "assistant" => Role::Assistant,
            _ => Role::User,
        };
        let blocks = blocks_from_content(msg.get("content"));
        if !blocks.is_empty() {
            req.messages.push(Message { role, blocks });
        }
    }

    if let Some(tools) = v.get("tools").and_then(|t| t.as_array()) {
        for t in tools {
            let Some(name) = jstr(t, "name") else {
                continue;
            };
            req.tools.push(ToolDef {
                name: name.to_string(),
                description: jstr(t, "description").unwrap_or_default().to_string(),
                schema: t
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object","properties":{}})),
            });
        }
    }

    req.tool_choice = match v.get("tool_choice") {
        Some(Value::Object(o)) => match o.get("type").and_then(|x| x.as_str()).unwrap_or("auto") {
            "any" => Some(ToolChoice::Any),
            "none" => Some(ToolChoice::None),
            "tool" => o
                .get("name")
                .and_then(|x| x.as_str())
                .map(|n| ToolChoice::Tool(n.to_string())),
            _ => Some(ToolChoice::Auto),
        },
        _ => None,
    };

    // `top_k` and `thinking` have no OpenAI equivalent; keep them so an
    // Anthropic-to-Anthropic hop stays lossless.
    let carry = json!({
        "top_k": v.get("top_k"),
        "thinking": v.get("thinking"),
        "metadata": v.get("metadata"),
    });
    if carry
        .as_object()
        .map(|o| o.values().any(|x| !x.is_null()))
        .unwrap_or(false)
    {
        req.extra = Some(carry);
    }

    Ok(req)
}

pub fn render_request(req: &ChatRequest, upstream_model: &str, stream: bool) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let blocks: Vec<Value> = m
                .blocks
                .iter()
                .map(|b| match b {
                    Block::Text { text } => json!({"type": "text", "text": text}),
                    Block::Image { source } => {
                        json!({"type": "image", "source": image_to_anthropic(source)})
                    }
                    Block::ToolUse { id, name, input } => json!({
                        "type": "tool_use", "id": id, "name": name, "input": input
                    }),
                    Block::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                        "is_error": is_error
                    }),
                })
                .collect();
            json!({"role": m.role, "content": blocks})
        })
        .collect();

    let mut body = json!({
        "model": upstream_model,
        "messages": messages,
        // Anthropic requires max_tokens; 8192 is a safe default that every
        // current model accepts.
        "max_tokens": req.max_tokens.unwrap_or(8192),
        "stream": stream,
    });

    if !req.system.is_empty() {
        body["system"] = json!(req.system.join("\n\n"));
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if !req.stop.is_empty() {
        body["stop_sequences"] = json!(req.stop);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.schema
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = match &req.tool_choice {
            Some(ToolChoice::None) => json!({"type": "none"}),
            Some(ToolChoice::Any) => json!({"type": "any"}),
            Some(ToolChoice::Tool(n)) => json!({"type": "tool", "name": n}),
            _ => json!({"type": "auto"}),
        };
    }
    if let Some(extra) = &req.extra
        && let Some(obj) = extra.as_object()
    {
        for (k, v) in obj {
            if !v.is_null() {
                body[k] = v.clone();
            }
        }
    }
    body
}

pub fn parse_response(v: &Value) -> Result<ChatResponse, String> {
    let blocks = blocks_from_content(v.get("content"));
    let stop_reason = jstr(v, "stop_reason").unwrap_or("end_turn").to_string();
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: ju64(u, "input_tokens").unwrap_or(0),
        output_tokens: ju64(u, "output_tokens").unwrap_or(0),
    });
    Ok(ChatResponse {
        id: jstr(v, "id").unwrap_or("msg_magpie").to_string(),
        model: jstr(v, "model").unwrap_or_default().to_string(),
        blocks,
        stop_reason,
        usage: usage.unwrap_or_default(),
    })
}

pub fn render_response(resp: &ChatResponse, requested_model: &str) -> Value {
    let content: Vec<Value> = resp
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(json!({"type": "text", "text": text})),
            Block::ToolUse { id, name, input } => Some(json!({
                "type": "tool_use", "id": id, "name": name, "input": input
            })),
            _ => None,
        })
        .collect();
    let stop = match resp.stop_reason.as_str() {
        "length" => "max_tokens",
        "stop" => "end_turn",
        "tool_calls" => "tool_use",
        other => other,
    };
    json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "model": if resp.model.is_empty() { requested_model } else { &resp.model },
        "content": content,
        "stop_reason": stop,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": resp.usage.input_tokens,
            "output_tokens": resp.usage.output_tokens
        }
    })
}

#[derive(Default)]
pub struct InState {
    pub usage: Usage,
    pub stop: String,
}

pub fn parse_stream(ev: &crate::protocol::SseEvent, st: &mut InState) -> Vec<Delta> {
    let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
        return Vec::new();
    };
    let kind = ev
        .event
        .clone()
        .or_else(|| jstr(&v, "type").map(str::to_string))
        .unwrap_or_default();
    let mut out = Vec::new();
    match kind.as_str() {
        "message_start" => {
            if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                st.usage.input_tokens = ju64(u, "input_tokens").unwrap_or(0);
                st.usage.output_tokens = ju64(u, "output_tokens").unwrap_or(0);
            }
        }
        "content_block_start" => {
            if let Some(cb) = v.get("content_block")
                && jstr(cb, "type") == Some("tool_use")
            {
                out.push(Delta::ToolStart {
                    id: jstr(cb, "id").unwrap_or_default().to_string(),
                    name: jstr(cb, "name").unwrap_or_default().to_string(),
                });
            }
        }
        "content_block_delta" => {
            if let Some(d) = v.get("delta") {
                match jstr(d, "type") {
                    Some("text_delta") => {
                        if let Some(t) = jstr(d, "text")
                            && !t.is_empty()
                        {
                            out.push(Delta::Text(t.to_string()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(j) = jstr(d, "partial_json")
                            && !j.is_empty()
                        {
                            out.push(Delta::ToolArgs(j.to_string()));
                        }
                    }
                    _ => {}
                }
            }
        }
        "message_delta" => {
            if let Some(d) = v.get("delta")
                && let Some(r) = jstr(d, "stop_reason")
            {
                st.stop = r.to_string();
                out.push(Delta::Stop(r.to_string()));
            }
            if let Some(u) = v.get("usage") {
                let out_tokens = ju64(u, "output_tokens").unwrap_or(st.usage.output_tokens);
                st.usage.output_tokens = out_tokens;
                out.push(Delta::Usage(st.usage.clone()));
            }
        }
        _ => {}
    }
    out
}

pub struct OutState {
    pub id: String,
    block_index: i64,
    open_kind: &'static str,
    pub usage: Usage,
    started: bool,
}

impl OutState {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            block_index: -1,
            open_kind: "",
            usage: Usage::default(),
            started: false,
        }
    }
}

pub fn render_delta(d: &Delta, state: &mut OutState, model: &str) -> Option<String> {
    let mut out = String::new();
    let start = |out: &mut String, state: &OutState| {
        if !state.started {
            let msg = json!({
                "type": "message_start",
                "message": {
                    "id": state.id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": {"input_tokens": state.usage.input_tokens, "output_tokens": 0}
                }
            });
            out.push_str(&crate::protocol::frame(
                Some("message_start"),
                &msg.to_string(),
            ));
        }
    };
    match d {
        Delta::Text(t) => {
            if t.is_empty() {
                return None;
            }
            start(&mut out, state);
            state.started = true;
            if state.open_kind != "text" {
                if state.block_index >= 0 {
                    let stop = json!({"type":"content_block_stop","index": state.block_index});
                    out.push_str(&crate::protocol::frame(
                        Some("content_block_stop"),
                        &stop.to_string(),
                    ));
                }
                state.block_index += 1;
                state.open_kind = "text";
                let cb = json!({
                    "type": "content_block_start",
                    "index": state.block_index,
                    "content_block": {"type": "text", "text": ""}
                });
                out.push_str(&crate::protocol::frame(
                    Some("content_block_start"),
                    &cb.to_string(),
                ));
            }
            let d = json!({
                "type": "content_block_delta",
                "index": state.block_index,
                "delta": {"type": "text_delta", "text": t}
            });
            out.push_str(&crate::protocol::frame(
                Some("content_block_delta"),
                &d.to_string(),
            ));
        }
        Delta::ToolStart { id, name } => {
            start(&mut out, state);
            state.started = true;
            if state.block_index >= 0 {
                let stop = json!({"type":"content_block_stop","index": state.block_index});
                out.push_str(&crate::protocol::frame(
                    Some("content_block_stop"),
                    &stop.to_string(),
                ));
            }
            state.block_index += 1;
            state.open_kind = "tool";
            let cb = json!({
                "type": "content_block_start",
                "index": state.block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": if id.is_empty() { format!("toolu_magpie_{}", state.block_index) } else { id.clone() },
                    "name": name,
                    "input": {}
                }
            });
            out.push_str(&crate::protocol::frame(
                Some("content_block_start"),
                &cb.to_string(),
            ));
        }
        Delta::ToolArgs(j) => {
            if state.open_kind != "tool" {
                return None;
            }
            let d = json!({
                "type": "content_block_delta",
                "index": state.block_index,
                "delta": {"type": "input_json_delta", "partial_json": j}
            });
            out.push_str(&crate::protocol::frame(
                Some("content_block_delta"),
                &d.to_string(),
            ));
        }
        Delta::Usage(u) => {
            state.usage = u.clone();
            return None;
        }
        Delta::Stop(reason) => {
            start(&mut out, state);
            if state.block_index >= 0 {
                let stop = json!({"type":"content_block_stop","index": state.block_index});
                out.push_str(&crate::protocol::frame(
                    Some("content_block_stop"),
                    &stop.to_string(),
                ));
            }
            let stop = match reason.as_str() {
                "length" => "max_tokens",
                "stop" => "end_turn",
                "tool_calls" => "tool_use",
                other => other,
            };
            let d = json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop, "stop_sequence": Value::Null},
                "usage": {"output_tokens": state.usage.output_tokens}
            });
            out.push_str(&crate::protocol::frame(
                Some("message_delta"),
                &d.to_string(),
            ));
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

pub fn finish_stream() -> String {
    crate::protocol::frame(
        Some("message_stop"),
        &json!({"type": "message_stop"}).to_string(),
    )
}
