//! OpenAI chat-completions dialect (also served by DeepSeek, Moonshot, Qwen,
//! Groq, Together, Ollama, LM Studio and most of the long tail).

use crate::ir::*;
use crate::protocol::{jf32, jstr, jstr_list, ju64};
use serde_json::{Value, json};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn flatten_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => {
            let parts: Vec<String> = a
                .iter()
                .filter_map(|p| {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(str::to_string)
                        .or_else(|| {
                            p.get("content")
                                .and_then(|c| c.as_str())
                                .map(str::to_string)
                        })
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Some(other) if other.is_object() => other
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        _ => None,
    }
}

fn blocks_from_content(content: Option<&Value>) -> Vec<Block> {
    match content {
        // An assistant turn that only calls tools sends `content: ""`; turning
        // that into an empty text block makes the Anthropic side render a
        // phantom `{"type":"text","text":""}` ahead of the tool call.
        Some(Value::String(s)) if s.is_empty() => Vec::new(),
        Some(Value::String(s)) => vec![Block::text(s.clone())],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|p| match jstr(p, "type").unwrap_or("text") {
                "text" | "input_text" | "output_text" => jstr(p, "text").map(Block::text),
                "image_url" => {
                    p.get("image_url")
                        .and_then(|u| jstr(u, "url"))
                        .map(|u| Block::Image {
                            source: u.to_string(),
                        })
                }
                "image" => jstr(p, "source").map(|u| Block::Image {
                    source: u.to_string(),
                }),
                _ => None,
            })
            .collect(),
        Some(other) if other.is_object() => flatten_text(Some(other))
            .map(|t| vec![Block::text(t)])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub fn parse_request(v: &Value) -> Result<ChatRequest, String> {
    let model = jstr(v, "model").ok_or("`model` is required")?.to_string();
    let mut req = ChatRequest {
        model,
        stream: v.get("stream").and_then(|x| x.as_bool()).unwrap_or(false),
        max_tokens: ju64(v, "max_completion_tokens")
            .or_else(|| ju64(v, "max_tokens"))
            .map(|n| n as u32),
        temperature: jf32(v, "temperature"),
        top_p: jf32(v, "top_p"),
        stop: jstr_list(v, "stop"),
        ..Default::default()
    };

    let messages = v
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or("`messages` is required")?;

    for msg in messages {
        let role = jstr(msg, "role").unwrap_or("user");
        match role {
            "system" | "developer" => {
                if let Some(text) = flatten_text(msg.get("content")) {
                    req.system.push(text);
                }
            }
            "tool" | "function" => {
                let id = jstr(msg, "tool_call_id")
                    .or_else(|| jstr(msg, "name"))
                    .unwrap_or_default()
                    .to_string();
                req.messages.push(Message {
                    role: Role::User,
                    blocks: vec![Block::ToolResult {
                        tool_use_id: id,
                        content: flatten_text(msg.get("content")).unwrap_or_default(),
                        is_error: false,
                    }],
                });
            }
            "assistant" => {
                let mut blocks = blocks_from_content(msg.get("content"));
                if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
                    for call in calls {
                        let id = jstr(call, "id").unwrap_or_default().to_string();
                        let f = call.get("function").cloned().unwrap_or(Value::Null);
                        let name = jstr(&f, "name").unwrap_or_default().to_string();
                        let args = f.get("arguments").cloned().unwrap_or(Value::Null);
                        let input = match args {
                            Value::String(s) if !s.is_empty() => {
                                serde_json::from_str(&s).unwrap_or(Value::String(s))
                            }
                            other => other,
                        };
                        blocks.push(Block::ToolUse { id, name, input });
                    }
                }
                if !blocks.is_empty() {
                    req.messages.push(Message {
                        role: Role::Assistant,
                        blocks,
                    });
                }
            }
            _ => {
                // `user` and anything unrecognized: treat as user content so a
                // lenient caller does not get a 400 from the gateway.
                let blocks = blocks_from_content(msg.get("content"));
                if !blocks.is_empty() {
                    req.messages.push(Message {
                        role: Role::User,
                        blocks,
                    });
                }
            }
        }
    }

    if let Some(tools) = v.get("tools").and_then(|t| t.as_array()) {
        for t in tools {
            let f = t.get("function").unwrap_or(t);
            let Some(name) = jstr(f, "name") else {
                continue;
            };
            req.tools.push(ToolDef {
                name: name.to_string(),
                description: jstr(f, "description").unwrap_or_default().to_string(),
                schema: f
                    .get("parameters")
                    .or_else(|| f.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object","properties":{}})),
            });
        }
    }

    req.tool_choice = match v.get("tool_choice") {
        Some(Value::String(s)) => Some(match s.as_str() {
            "none" => ToolChoice::None,
            "required" | "any" => ToolChoice::Any,
            _ => ToolChoice::Auto,
        }),
        Some(Value::Object(o)) => o
            .get("function")
            .and_then(|f| jstr(f, "name"))
            .map(|n| ToolChoice::Tool(n.to_string()))
            .or(Some(ToolChoice::Auto)),
        _ => None,
    };

    Ok(req)
}

pub fn render_request(req: &ChatRequest, upstream_model: &str, stream: bool) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for s in &req.system {
        messages.push(json!({"role": "system", "content": s}));
    }

    for m in &req.messages {
        // OpenAI puts tool results in their own `role: tool` messages, so one
        // IR message can fan out into several wire messages.
        let mut pending: Vec<Value> = Vec::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        let mut has_tool_use = false;

        for b in &m.blocks {
            match b {
                Block::Text { text } => pending.push(json!({"type":"text","text":text})),
                Block::Image { source } => {
                    pending.push(json!({"type":"image_url","image_url":{"url":source}}))
                }
                Block::ToolUse { id, name, input } => {
                    has_tool_use = true;
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()}
                    }));
                }
                Block::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": content
                    }));
                }
            }
        }

        if has_tool_use || !pending.is_empty() {
            let all_text = pending.iter().all(|p| p["type"] == "text");
            let content = if pending.is_empty() {
                Value::Null
            } else if all_text {
                Value::String(
                    pending
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else {
                Value::Array(pending)
            };
            let mut msg = json!({"role": m.role, "content": content});
            if has_tool_use {
                msg["tool_calls"] = Value::Array(tool_calls);
            }
            messages.push(msg);
        }
    }

    let mut body = json!({
        "model": upstream_model,
        "messages": messages,
        "stream": stream,
    });
    if let Some(n) = req.max_tokens {
        body["max_tokens"] = json!(n);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if !req.stop.is_empty() {
        body["stop"] = json!(req.stop);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.schema
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = match &req.tool_choice {
            Some(ToolChoice::None) => json!("none"),
            Some(ToolChoice::Any) => json!("required"),
            Some(ToolChoice::Tool(n)) => json!({"type":"function","function":{"name": n}}),
            _ => json!("auto"),
        };
    }
    if stream {
        body["stream_options"] = json!({"include_usage": true});
    }
    body
}

fn parse_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: ju64(v, "prompt_tokens")
            .or_else(|| ju64(v, "input_tokens"))
            .unwrap_or(0),
        output_tokens: ju64(v, "completion_tokens")
            .or_else(|| ju64(v, "output_tokens"))
            .unwrap_or(0),
    }
}

pub fn parse_response(v: &Value) -> Result<ChatResponse, String> {
    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or("upstream reply has no `choices`")?;
    let message = choice.get("message").unwrap_or(&Value::Null);
    let mut blocks = blocks_from_content(message.get("content"));
    let mut has_tool = false;
    if let Some(calls) = message.get("tool_calls").and_then(|c| c.as_array()) {
        for call in calls {
            has_tool = true;
            let id = jstr(call, "id").unwrap_or_default().to_string();
            let f = call.get("function").cloned().unwrap_or(Value::Null);
            let name = jstr(&f, "name").unwrap_or_default().to_string();
            let args = f.get("arguments").cloned().unwrap_or(Value::Null);
            let input = match args {
                Value::String(s) if !s.is_empty() => {
                    serde_json::from_str(&s).unwrap_or(Value::String(s))
                }
                other => other,
            };
            blocks.push(Block::ToolUse { id, name, input });
        }
    }
    let finish = jstr(choice, "finish_reason").unwrap_or("stop");
    let stop_reason = match finish {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        other => other,
    }
    .to_string();
    if has_tool && stop_reason == "stop" {
        // Some providers report `stop` even when they emitted tool calls.
        // Normalizing here keeps the Anthropic translation honest.
    }
    Ok(ChatResponse {
        id: jstr(v, "id").unwrap_or("chatcmpl-magpie").to_string(),
        model: jstr(v, "model").unwrap_or_default().to_string(),
        blocks,
        stop_reason,
        usage: v.get("usage").map(parse_usage).unwrap_or_default(),
    })
}

pub fn render_response(resp: &ChatResponse, requested_model: &str) -> Value {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for b in &resp.blocks {
        match b {
            Block::Text { text: t } => text.push_str(t),
            Block::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()}
            })),
            _ => {}
        }
    }
    let finish = match resp.stop_reason.as_str() {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        other => other,
    };
    let mut message = json!({"role": "assistant", "content": text});
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    json!({
        "id": resp.id,
        "object": "chat.completion",
        "created": now_secs(),
        "model": if resp.model.is_empty() { requested_model } else { &resp.model },
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {
            "prompt_tokens": resp.usage.input_tokens,
            "completion_tokens": resp.usage.output_tokens,
            "total_tokens": resp.usage.total()
        }
    })
}

/// Mutable state while decoding one upstream OpenAI stream.
#[derive(Default)]
pub struct InState {
    pub usage: Usage,
    started: bool,
}

pub fn parse_stream(data: &str, st: &mut InState) -> Vec<Delta> {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(u) = v.get("usage")
        && !u.is_null()
    {
        st.usage = parse_usage(u);
        out.push(Delta::Usage(st.usage.clone()));
    }
    let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
        return out;
    };
    for ch in choices {
        if let Some(d) = ch.get("delta") {
            if let Some(t) = d.get("content").and_then(|x| x.as_str())
                && !t.is_empty()
            {
                out.push(Delta::Text(t.to_string()));
                st.started = true;
            }
            if let Some(calls) = d.get("tool_calls").and_then(|x| x.as_array()) {
                for call in calls {
                    if let Some(f) = call.get("function") {
                        if let Some(name) = f.get("name").and_then(|x| x.as_str())
                            && !name.is_empty()
                        {
                            let id = call
                                .get("id")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default()
                                .to_string();
                            out.push(Delta::ToolStart {
                                id,
                                name: name.to_string(),
                            });
                            st.started = true;
                        }
                        if let Some(args) = f.get("arguments").and_then(|x| x.as_str())
                            && !args.is_empty()
                        {
                            out.push(Delta::ToolArgs(args.to_string()));
                        }
                    }
                }
            }
        }
        if let Some(fr) = ch.get("finish_reason").and_then(|x| x.as_str())
            && !fr.is_empty()
        {
            out.push(Delta::Stop(fr.to_string()));
        }
    }
    out
}

/// Mutable state while encoding one downstream OpenAI stream.
pub struct OutState {
    pub id: String,
    created: u64,
    role_sent: bool,
    tool_index: i64,
    tool_open: bool,
    input_tokens: u64,
    output_tokens: u64,
}

impl OutState {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            created: now_secs(),
            role_sent: false,
            tool_index: -1,
            tool_open: false,
            input_tokens: 0,
            output_tokens: 0,
        }
    }
}

fn chunk(state: &OutState, model: &str, delta: Value, finish: Value) -> String {
    let v = json!({
        "id": state.id,
        "object": "chat.completion.chunk",
        "created": state.created,
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
    });
    format!("data: {v}\n\n")
}

/// Render one delta as an OpenAI SSE frame, or `None` when it produces nothing.
pub fn render_delta(d: &Delta, state: &mut OutState, model: &str) -> Option<String> {
    match d {
        Delta::Text(t) => {
            if t.is_empty() {
                return None;
            }
            let mut out = String::new();
            if !state.role_sent {
                state.role_sent = true;
                out.push_str(&chunk(
                    state,
                    model,
                    json!({"role":"assistant","content":""}),
                    Value::Null,
                ));
            }
            out.push_str(&chunk(state, model, json!({"content": t}), Value::Null));
            Some(out)
        }
        Delta::ToolStart { id, name } => {
            state.tool_index += 1;
            state.tool_open = true;
            state.role_sent = true;
            let idx = state.tool_index;
            Some(chunk(
                state,
                model,
                json!({"tool_calls":[{
                    "index": idx,
                    "id": if id.is_empty() { format!("call_magpie_{idx}") } else { id.clone() },
                    "type": "function",
                    "function": {"name": name, "arguments": ""}
                }]}),
                Value::Null,
            ))
        }
        Delta::ToolArgs(args) => {
            if !state.tool_open {
                return None;
            }
            let idx = state.tool_index;
            Some(chunk(
                state,
                model,
                json!({"tool_calls":[{"index": idx, "function": {"arguments": args}}]}),
                Value::Null,
            ))
        }
        Delta::Usage(u) => {
            state.input_tokens = u.input_tokens;
            state.output_tokens = u.output_tokens;
            None
        }
        Delta::Stop(reason) => {
            state.tool_open = false;
            let finish = match reason.as_str() {
                "max_tokens" => "length",
                "tool_use" => "tool_calls",
                "end_turn" => "stop",
                other => other,
            };
            Some(chunk(state, model, json!({}), json!(finish)))
        }
    }
}

/// The final usage frame plus `[DONE]`, sent once the upstream stream ends.
pub fn finish_stream(state: &OutState) -> String {
    let usage = json!({
        "id": state.id,
        "object": "chat.completion.chunk",
        "created": state.created,
        "model": "",
        "choices": [],
        "usage": {
            "prompt_tokens": state.input_tokens,
            "completion_tokens": state.output_tokens,
            "total_tokens": state.input_tokens + state.output_tokens
        }
    });
    format!("data: {usage}\n\ndata: [DONE]\n\n")
}
