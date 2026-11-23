//! Round-trip tests for the protocol translators.
//!
//! These are the parts that break silently: a dropped `tool_use`, a `stop`
//! reason that means the opposite thing on the other side, a stream frame that
//! parses but carries no text. Every case here corresponds to a bug that would
//! otherwise only show up as "Claude Code hangs on tool calls".

use magpie_core::ir::*;
use magpie_core::protocol::{anthropic, gemini, openai};
use serde_json::json;

#[test]
fn openai_chat_roundtrips_text() {
    let body = json!({
        "model": "gpt-x",
        "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hello"}
        ],
        "max_tokens": 64,
        "temperature": 0.2,
        "stop": "END"
    });

    let req = openai::parse_request(&body).expect("parse");
    assert_eq!(req.model, "gpt-x");
    assert_eq!(req.system, vec!["be terse".to_string()]);
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].text(), "hello");
    assert_eq!(req.max_tokens, Some(64));
    assert_eq!(req.stop, vec!["END".to_string()]);

    let rendered = openai::render_request(&req, "upstream-model", false);
    assert_eq!(rendered["model"], "upstream-model");
    // System prompts go back out as their own message.
    assert_eq!(rendered["messages"][0]["role"], "system");
    assert_eq!(rendered["messages"][1]["content"], "hello");
    assert_eq!(rendered["max_tokens"], 64);
}

#[test]
fn anthropic_request_translates_to_openai() {
    let body = json!({
        "model": "claude-x",
        "max_tokens": 128,
        "system": [{"type": "text", "text": "you are a bot"}],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hi"}]}
        ],
        "tools": [{
            "name": "read_file",
            "description": "read a file",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}
        }],
        "tool_choice": {"type": "tool", "name": "read_file"}
    });

    let req = anthropic::parse_request(&body).expect("parse");
    assert_eq!(req.system, vec!["you are a bot".to_string()]);
    assert!(!req.stream);
    assert_eq!(req.tools.len(), 1);
    assert_eq!(req.tools[0].name, "read_file");
    assert_eq!(req.tool_choice, Some(ToolChoice::Tool("read_file".into())));

    let out = openai::render_request(&req, "gpt-4o", false);
    assert_eq!(out["tools"][0]["type"], "function");
    assert_eq!(out["tools"][0]["function"]["name"], "read_file");
    // `required` + a named function is how OpenAI expresses a forced tool.
    assert_eq!(out["tool_choice"]["function"]["name"], "read_file");
    assert_eq!(out["messages"][0]["role"], "system");
}

#[test]
fn tool_use_survives_both_directions() {
    // An assistant turn that called a tool, then the caller answering it.
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "user", "content": "list the files"},
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "ls", "arguments": "{\"path\":\".\"}"}
            }]},
            {"role": "tool", "tool_call_id": "call_1", "content": "a.txt\nb.txt"}
        ]
    });

    let req = openai::parse_request(&body).expect("parse");
    let assistant = req
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant turn");
    let Block::ToolUse { id, name, input } = &assistant.blocks[0] else {
        panic!("expected a tool_use block, got {:?}", assistant.blocks);
    };
    assert_eq!(id, "call_1");
    assert_eq!(name, "ls");
    assert_eq!(input["path"], ".");

    // Rendering back to Anthropic must produce a tool_use block and a
    // tool_result block, not a text blob.
    let rendered = anthropic::render_request(&req, "claude", false);
    let content = rendered["messages"][1]["content"]
        .as_array()
        .expect("blocks");
    assert_eq!(content[0]["type"], "tool_use");
    assert_eq!(content[0]["id"], "call_1");
    let tool_result = rendered["messages"][2]["content"]
        .as_array()
        .expect("blocks");
    assert_eq!(tool_result[0]["type"], "tool_result");
    assert_eq!(tool_result[0]["tool_use_id"], "call_1");
    assert!(
        tool_result[0]["content"]
            .as_str()
            .unwrap()
            .contains("a.txt")
    );
}

#[test]
fn stop_reasons_mean_the_same_thing_on_both_sides() {
    let openai_reply = json!({
        "id": "x",
        "model": "m",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "", "tool_calls": [{
                "id": "c", "type": "function", "function": {"name": "f", "arguments": "{}"}
            }]},
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4}
    });
    let resp = openai::parse_response(&openai_reply).expect("parse");
    assert_eq!(resp.stop_reason, "tool_use");
    assert_eq!(resp.usage.input_tokens, 3);
    assert_eq!(resp.usage.output_tokens, 4);

    let anthropic_reply = anthropic::render_response(&resp, "claude");
    assert_eq!(anthropic_reply["stop_reason"], "tool_use");
    assert_eq!(anthropic_reply["content"][0]["type"], "tool_use");
}

#[test]
fn openai_stream_deltas_carry_text_and_tools() {
    let mut state = openai::InState::default();
    let deltas = openai::parse_stream(r#"{"choices":[{"delta":{"content":"he"}}]}"#, &mut state);
    assert!(matches!(&deltas[0], Delta::Text(t) if t == "he"));

    let deltas = openai::parse_stream(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"f","arguments":"{\"a\""}}]}}]}"#,
        &mut state,
    );
    assert!(matches!(&deltas[0], Delta::ToolStart { name, .. } if name == "f"));
    assert!(matches!(&deltas[1], Delta::ToolArgs(a) if a == "{\"a\""));

    let deltas = openai::parse_stream(
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":2}}"#,
        &mut state,
    );
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, Delta::Stop(s) if s == "stop"))
    );
    assert_eq!(state.usage.input_tokens, 9);
}

#[test]
fn anthropic_stream_events_become_deltas() {
    use magpie_core::protocol::SseEvent;
    let mut state = anthropic::InState::default();

    let ev = SseEvent {
        event: Some("content_block_start".into()),
        data:
            r#"{"index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"ls","input":{}}}"#
                .into(),
    };
    let deltas = anthropic::parse_stream(&ev, &mut state);
    assert!(matches!(&deltas[0], Delta::ToolStart { id, name } if id == "tu_1" && name == "ls"));

    let ev = SseEvent {
        event: Some("content_block_delta".into()),
        data: r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"p\""}}"#.into(),
    };
    let deltas = anthropic::parse_stream(&ev, &mut state);
    assert!(matches!(&deltas[0], Delta::ToolArgs(j) if j == "{\"p\""));

    let ev = SseEvent {
        event: Some("message_delta".into()),
        data: r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#.into(),
    };
    let deltas = anthropic::parse_stream(&ev, &mut state);
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, Delta::Stop(s) if s == "tool_use"))
    );
    assert_eq!(state.usage.output_tokens, 7);
}

#[test]
fn gemini_rejects_but_renders_schemas_it_understands() {
    let body = json!({
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        "systemInstruction": {"parts": [{"text": "sys"}]},
        "generationConfig": {"maxOutputTokens": 32, "temperature": 0.1}
    });
    let req = gemini::parse_request(&body).expect("parse");
    assert_eq!(req.system, vec!["sys".to_string()]);
    assert_eq!(req.max_tokens, Some(32));

    let rendered = gemini::render_request(&req);
    assert_eq!(rendered["contents"][0]["role"], "user");
    assert_eq!(rendered["generationConfig"]["maxOutputTokens"], 32);

    let reply = json!({
        "candidates": [{"content": {"parts": [{"text": "yo"}]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 1}
    });
    let resp = gemini::parse_response(&reply).expect("parse response");
    assert_eq!(resp.text(), "yo");
    assert_eq!(resp.usage.input_tokens, 5);
}

#[test]
fn sse_parser_handles_split_frames_and_crlf() {
    use magpie_core::protocol::SseParser;
    let mut p = SseParser::default();
    // A frame split across two TCP reads, and one with CRLF endings.
    assert!(p.push(b"data: {\"a\":1").is_empty());
    let events = p.push(b"}\n\ndata: [DONE]\r\n\r\n");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data, "{\"a\":1}");
    assert!(events[1].is_done());
}

#[test]
fn images_survive_a_data_url_hop() {
    let body = json!({
        "model": "m",
        "max_tokens": 8,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "what is this"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}}
        ]}]
    });
    let req = anthropic::parse_request(&body).expect("parse");
    let out = openai::render_request(&req, "gpt-4o", false);
    let parts = out["messages"][0]["content"].as_array().expect("parts");
    assert_eq!(parts[1]["type"], "image_url");
    assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,QUJD");

    // And back again.
    let req2 = openai::parse_request(&out).expect("parse back");
    let out2 = anthropic::render_request(&req2, "claude", false);
    let blocks = out2["messages"][0]["content"].as_array().expect("blocks");
    assert_eq!(blocks[1]["source"]["media_type"], "image/png");
    assert_eq!(blocks[1]["source"]["data"], "QUJD");
}
