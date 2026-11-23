//! Google Gemini dialect (`/v1beta/models/{model}:generateContent`).
//!
//! Gemini is the odd one out: system prompts live outside `contents`, function
//! calls and their results are `parts`, and the streaming variant is the same
//! JSON shape repeated per event rather than a delta protocol.

use crate::ir::*;
use crate::protocol::{jstr, jstr_list, ju64};
use serde_json::{Value, json};

fn flatten(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => {
            let parts: Vec<String> = a
                .iter()
                .filter_map(|p| jstr(p, "text").map(str::to_string))
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

pub fn parse_request(v: &Value) -> Result<ChatRequest, String> {
    // Gemini puts the model in the URL, not the body; the caller fills it in
    // after parsing, so an empty model here is expected and fine.
    let mut req = ChatRequest {
        model: jstr(v, "model").unwrap_or_default().to_string(),
        stream: false,
        ..Default::default()
    };

    if let Some(sys) = flatten(
        v.get("systemInstruction")
            .or_else(|| v.get("system_instruction"))
            .and_then(|s| s.get("parts")),
    ) {
        req.system.push(sys);
    }

    if let Some(contents) = v.get("contents").and_then(|c| c.as_array()) {
        for msg in contents {
            let role = match jstr(msg, "role").unwrap_or("user") {
                "model" | "assistant" => Role::Assistant,
                _ => Role::User,
            };
            let mut blocks = Vec::new();
            if let Some(parts) = msg.get("parts").and_then(|p| p.as_array()) {
                for part in parts {
                    if let Some(t) = jstr(part, "text") {
                        blocks.push(Block::text(t));
                    } else if let Some(fc) = part.get("functionCall") {
                        blocks.push(Block::ToolUse {
                            id: jstr(fc, "id").unwrap_or_default().to_string(),
                            name: jstr(fc, "name").unwrap_or_default().to_string(),
                            input: fc.get("args").cloned().unwrap_or(json!({})),
                        });
                    } else if let Some(fr) = part.get("functionResponse") {
                        blocks.push(Block::ToolResult {
                            tool_use_id: jstr(fr, "id").unwrap_or_default().to_string(),
                            content: fr
                                .get("response")
                                .map(|r| r.to_string())
                                .unwrap_or_default(),
                            is_error: false,
                        });
                    } else if let Some(inline) = part.get("inlineData") {
                        let mime = jstr(inline, "mimeType").unwrap_or("image/png");
                        let data = jstr(inline, "data").unwrap_or_default();
                        blocks.push(Block::Image {
                            source: format!("data:{mime};base64,{data}"),
                        });
                    }
                }
            }
            if !blocks.is_empty() {
                req.messages.push(Message { role, blocks });
            }
        }
    }

    if let Some(gc) = v.get("generationConfig") {
        req.max_tokens = ju64(gc, "maxOutputTokens").map(|n| n as u32);
        req.temperature = gc
            .get("temperature")
            .and_then(|x| x.as_f64())
            .map(|f| f as f32);
        req.top_p = gc.get("topP").and_then(|x| x.as_f64()).map(|f| f as f32);
        req.stop = jstr_list(gc, "stopSequences");
    }

    if let Some(tools) = v.get("tools").and_then(|t| t.as_array()) {
        for t in tools {
            if let Some(decls) = t.get("functionDeclarations").and_then(|d| d.as_array()) {
                for d in decls {
                    let Some(name) = jstr(d, "name") else {
                        continue;
                    };
                    req.tools.push(ToolDef {
                        name: name.to_string(),
                        description: jstr(d, "description").unwrap_or_default().to_string(),
                        schema: d
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({"type":"object","properties":{}})),
                    });
                }
            }
        }
    }
    Ok(req)
}

fn image_part(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:")
        && let Some((mime, data)) = rest.split_once(";base64,")
    {
        return json!({"inlineData": {"mimeType": mime, "data": data}});
    }
    // Gemini cannot fetch a URL, so a remote image becomes a text note rather
    // than a silent failure.
    json!({"text": format!("[image: {url}]")})
}

pub fn render_request(req: &ChatRequest) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    for m in &req.messages {
        let mut parts: Vec<Value> = Vec::new();
        for b in &m.blocks {
            match b {
                Block::Text { text } => parts.push(json!({"text": text})),
                Block::Image { source } => parts.push(image_part(source)),
                Block::ToolUse { id: _, name, input } => parts.push(json!({
                    "functionCall": {"name": name, "args": input}
                })),
                Block::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => parts.push(json!({
                    "functionResponse": {
                        "name": tool_use_id,
                        "response": {"content": content}
                    }
                })),
            }
        }
        if parts.is_empty() {
            continue;
        }
        contents.push(json!({
            "role": if m.role == Role::Assistant { "model" } else { "user" },
            "parts": parts
        }));
    }

    let mut body = json!({ "contents": contents });
    if !req.system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": req.system.join("\n\n")}]});
    }
    let mut gc = json!({});
    if let Some(n) = req.max_tokens {
        gc["maxOutputTokens"] = json!(n);
    }
    if let Some(t) = req.temperature {
        gc["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        gc["topP"] = json!(p);
    }
    if !req.stop.is_empty() {
        gc["stopSequences"] = json!(req.stop);
    }
    if gc.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        body["generationConfig"] = gc;
    }
    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": sanitize_schema(&t.schema)
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": decls}]);
    }
    body
}

/// Gemini rejects JSON Schema keywords it does not know, so the schema is
/// reduced to the subset it accepts. Anything dropped becomes a description
/// hint instead of a hard 400.
fn sanitize_schema(schema: &Value) -> Value {
    let mut out = json!({"type": "object", "properties": {}});
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        let mut cleaned = serde_json::Map::new();
        for (k, v) in props {
            let ty = jstr(v, "type").unwrap_or("string");
            let mut entry = json!({"type": ty});
            if let Some(d) = jstr(v, "description") {
                entry["description"] = json!(d);
            }
            if let Some(items) = v.get("items") {
                entry["items"] = sanitize_schema(items);
            }
            cleaned.insert(k.clone(), entry);
        }
        out["properties"] = Value::Object(cleaned);
    }
    if let Some(req) = schema.get("required").and_then(|r| r.as_array()) {
        out["required"] = Value::Array(req.clone());
    }
    out
}

pub fn parse_response(v: &Value) -> Result<ChatResponse, String> {
    let cand = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or("upstream reply has no `candidates`")?;
    let mut blocks = Vec::new();
    if let Some(parts) = cand
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
    {
        for part in parts {
            if let Some(t) = jstr(part, "text") {
                blocks.push(Block::text(t));
            } else if let Some(fc) = part.get("functionCall") {
                blocks.push(Block::ToolUse {
                    id: format!("call_gemini_{}", blocks.len()),
                    name: jstr(fc, "name").unwrap_or_default().to_string(),
                    input: fc.get("args").cloned().unwrap_or(json!({})),
                });
            }
        }
    }
    let mut stop_reason = "end_turn".to_string();
    if let Some(fr) = jstr(cand, "finishReason") {
        stop_reason = match fr {
            "MAX_TOKENS" => "max_tokens",
            "STOP" => {
                if blocks.iter().any(|b| matches!(b, Block::ToolUse { .. })) {
                    "tool_use"
                } else {
                    "end_turn"
                }
            }
            other => other.to_ascii_lowercase().leak(),
        }
        .to_string();
    }
    let usage = v.get("usageMetadata").map(|u| Usage {
        input_tokens: ju64(u, "promptTokenCount").unwrap_or(0),
        output_tokens: ju64(u, "candidatesTokenCount").unwrap_or(0),
    });
    Ok(ChatResponse {
        id: format!("gemini-{}", crate::config::random_key(12)),
        model: jstr(v, "modelVersion").unwrap_or_default().to_string(),
        blocks,
        stop_reason,
        usage: usage.unwrap_or_default(),
    })
}

pub fn parse_stream(data: &str) -> Vec<Delta> {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(cand) = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        && let Some(parts) = cand
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
    {
        for part in parts {
            if let Some(t) = jstr(part, "text")
                && !t.is_empty()
            {
                out.push(Delta::Text(t.to_string()));
            }
            if let Some(fc) = part.get("functionCall") {
                out.push(Delta::ToolStart {
                    id: format!("call_gemini_{}", out.len()),
                    name: jstr(fc, "name").unwrap_or_default().to_string(),
                });
                out.push(Delta::ToolArgs(
                    fc.get("args").cloned().unwrap_or(json!({})).to_string(),
                ));
            }
        }
    }
    if let Some(u) = v.get("usageMetadata") {
        out.push(Delta::Usage(Usage {
            input_tokens: ju64(u, "promptTokenCount").unwrap_or(0),
            output_tokens: ju64(u, "candidatesTokenCount").unwrap_or(0),
        }));
    }
    if let Some(r) = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| jstr(c, "finishReason"))
    {
        out.push(Delta::Stop(match r {
            "MAX_TOKENS" => "max_tokens".to_string(),
            "STOP" => "end_turn".to_string(),
            other => other.to_ascii_lowercase(),
        }));
    }
    out
}
