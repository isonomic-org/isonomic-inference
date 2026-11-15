//! Every lossy step in emit and decode raises a diagnostic. This file is the
//! executable list of lossy paths; docs/status.md lists the same paths.

use std::collections::BTreeMap;

use isonomic_model_protocol::{
    Block, Chat, Citation, Conversation, Delta, DiagKind, Effort, EmitOptions, Limits, MediaRef,
    Messages, ReasoningConfig, ResponseFormat, Role, StreamDecoder, Turn, WireClient,
};
use serde_json::{json, Value};

fn kinds<W: WireClient>(conv: &Conversation, opts: &EmitOptions<'_>) -> Vec<(DiagKind, String)> {
    W::emit(conv, opts)
        .unwrap()
        .1
        .into_iter()
        .map(|d| (d.kind, d.at))
        .collect()
}

fn base() -> Conversation {
    Conversation {
        model: Some("m".into()),
        sampling: isonomic_model_protocol::Sampling {
            max_output_tokens: Some(16),
            ..Default::default()
        },
        ..Conversation::default()
    }
}

fn with_turns(turns: Vec<Turn>) -> Conversation {
    Conversation { turns, ..base() }
}

fn turn(role: Role, blocks: Vec<Block>) -> Turn {
    Turn {
        id: None,
        role,
        blocks,
    }
}

fn d(kind: DiagKind, at: &str) -> (DiagKind, String) {
    (kind, at.to_string())
}

fn opts() -> EmitOptions<'static> {
    EmitOptions::default()
}

#[test]
fn a_clean_conversation_emits_without_diagnostics() {
    let conv = with_turns(vec![Turn::user_text("hi"), Turn::assistant_text("hello")]);
    assert!(kinds::<Messages>(&conv, &opts()).is_empty());
    assert!(kinds::<Chat>(&conv, &opts()).is_empty());
}

#[test]
fn missing_model_and_limit_are_defaulted() {
    let conv = Conversation {
        turns: vec![Turn::user_text("hi")],
        ..Conversation::default()
    };
    assert_eq!(
        kinds::<Messages>(&conv, &opts()),
        [
            d(DiagKind::Defaulted, "model"),
            d(DiagKind::Defaulted, "sampling.max_output_tokens")
        ]
    );
    assert_eq!(
        kinds::<Chat>(&conv, &opts()),
        [d(DiagKind::Defaulted, "model")]
    );
}

#[test]
fn messages_lossy_paths() {
    let citation = Block::Citation(Citation {
        url: Some("https://example.org".into()),
        ..Citation::default()
    });
    let unsigned = Block::Reasoning {
        text: "hmm".into(),
        signature: None,
        redacted: None,
    };
    let result = Block::ToolResult {
        call_id: "call:1".into(),
        content: vec![Block::text("ok"), unsigned.clone()],
        is_error: false,
    };
    let mut conv = with_turns(vec![
        turn(
            Role::Assistant,
            vec![Block::text("a"), citation, unsigned.clone()],
        ),
        turn(Role::User, vec![unsigned, result]),
    ]);
    conv.metadata = BTreeMap::from([("team".to_string(), "x".to_string())]);
    conv.reasoning = Some(ReasoningConfig {
        effort: Some(Effort::Max),
        budget_tokens: None,
    });
    conv.response_format = Some(ResponseFormat::JsonSchema {
        name: Some("n".into()),
        schema: json!({}),
        strict: None,
    });
    conv.ext.insert("messages", "model", json!("other"));
    conv.ext.insert("messages", "top_k", json!(5));
    let (request, diags) = Messages::emit(&conv, &opts()).unwrap();
    let got: Vec<_> = diags.iter().map(|d| (d.kind, d.at.clone())).collect();
    assert_eq!(
        got,
        [
            d(DiagKind::Dropped, "turns[0].blocks[1]"),
            d(DiagKind::Dropped, "turns[0].blocks[2]"),
            d(DiagKind::Dropped, "turns[1].blocks[0]"),
            d(DiagKind::Dropped, "turns[1].blocks[1].content[1]"),
            d(DiagKind::IdMapped, "turns[1].blocks[1].call_id"),
            d(DiagKind::Approximated, "reasoning.effort"),
            d(DiagKind::Dropped, "response_format"),
            d(DiagKind::Dropped, "metadata.team"),
            d(DiagKind::Dropped, "ext.model"),
        ]
    );
    let request = serde_json::to_value(&request).unwrap();
    assert_eq!(request["top_k"], 5, "extensions are written back");
    assert_eq!(request["model"], "m");
    assert_eq!(request["output_config"]["effort"], "high");
}

#[test]
fn chat_lossy_paths() {
    let blobs = BTreeMap::from([("i".to_string(), vec![1u8, 2, 3])]);
    let image = Block::Image {
        source: MediaRef::Blob {
            id: "i".into(),
            media_type: "image/png".into(),
        },
    };
    let file_url = Block::File {
        source: MediaRef::Url {
            url: "https://example.org/a.pdf".into(),
        },
        name: None,
    };
    let result = Block::ToolResult {
        call_id: "c".into(),
        content: vec![Block::text("a"), Block::text("b"), image],
        is_error: true,
    };
    let signed = Block::Reasoning {
        text: "r".into(),
        signature: Some("s".into()),
        redacted: None,
    };
    let use_ = Block::ToolUse {
        id: "x".into(),
        name: "t".into(),
        input: json!({}),
    };
    let mut conv = with_turns(vec![
        turn(
            Role::Assistant,
            vec![Block::text("a"), signed, Block::text("b")],
        ),
        turn(Role::User, vec![Block::text("q"), result, file_url, use_]),
    ]);
    conv.reasoning = Some(ReasoningConfig {
        effort: None,
        budget_tokens: Some(5000),
    });
    conv.response_format = Some(ResponseFormat::JsonSchema {
        name: None,
        schema: json!({}),
        strict: None,
    });
    let opts = EmitOptions {
        blobs: &blobs,
        ..EmitOptions::default()
    };
    assert_eq!(
        kinds::<Chat>(&conv, &opts),
        [
            d(DiagKind::Dropped, "turns[0].blocks[1]"),
            d(DiagKind::Approximated, "turns[0]"),
            d(DiagKind::Relocated, "turns[1].blocks[1].content[2]"),
            d(DiagKind::Approximated, "turns[1].blocks[1]"),
            d(DiagKind::Dropped, "turns[1].blocks[1]"),
            d(DiagKind::Dropped, "turns[1].blocks[2]"),
            d(DiagKind::Dropped, "turns[1].blocks[3]"),
            d(DiagKind::Relocated, "turns[1]"),
            d(DiagKind::Defaulted, "response_format.name"),
            d(DiagKind::Approximated, "reasoning.budget_tokens"),
        ]
    );
    // With reasoning history on, the text goes back and only the signature is lost.
    let history = EmitOptions {
        chat_reasoning_history: true,
        blobs: &blobs,
        ..EmitOptions::default()
    };
    let (request, diags) = Chat::emit(&conv, &history).unwrap();
    assert_eq!(
        diags[0].detail,
        "the format has no place for a reasoning signature"
    );
    let request = serde_json::to_value(&request).unwrap();
    assert_eq!(request["messages"][0]["reasoning_content"], "r");
    assert_eq!(request["reasoning_effort"], "medium");
}

fn decode<W: WireClient>(sse: &str) -> Vec<Delta> {
    StreamDecoder::<W>::new(Limits::default())
        .push(sse.as_bytes())
        .unwrap()
}

fn notices(deltas: &[Delta]) -> usize {
    deltas
        .iter()
        .filter(|d| matches!(d, Delta::Notice { .. }))
        .count()
}

#[test]
fn decoders_raise_notices_for_what_they_do_not_know() {
    let messages = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"server_tool_use\"}}\n\n\
        data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"new_delta\"}}\n\n\
        data: {\"type\":\"new_event\"}\n\n";
    assert_eq!(notices(&decode::<Messages>(messages)), 3);
    let chat = "data: {\"choices\":[{\"index\":1,\"delta\":{\"content\":\"x\"}},{\"index\":2,\"delta\":{}}]}\n\n";
    assert_eq!(notices(&decode::<Chat>(chat)), 1);
}

#[test]
fn whole_replies_report_what_they_could_not_keep() {
    let body = json!({"content": [{"type": "server_tool_use", "id": "s"}, {"type": "text", "text": "ok"}]});
    let (reply, diags) = Messages::parse(serde_json::from_value(body).unwrap()).unwrap();
    assert_eq!(diags[0].kind, DiagKind::Unrecognized);
    assert_eq!(reply.turns[0].blocks, [Block::text("ok")]);

    let body: Value = json!({"choices": [
        {"message": {"refusal": "no", "tool_calls": [{"id": "c", "type": "function",
            "function": {"name": "t", "arguments": "{bad"}}]}, "finish_reason": "stop"},
        {"message": {"content": "second"}}]});
    let (reply, diags) = Chat::parse(serde_json::from_value(body).unwrap()).unwrap();
    let kinds: Vec<_> = diags.iter().map(|d| d.kind).collect();
    assert_eq!(
        kinds,
        [
            DiagKind::Dropped,
            DiagKind::Approximated,
            DiagKind::Approximated
        ]
    );
    assert_eq!(
        reply.stop_reason,
        Some(isonomic_model_protocol::StopReason::Refusal)
    );
}

#[test]
fn a_closing_frame_after_an_error_is_accepted_and_anything_else_is_not() {
    let error = "data: {\"error\":{\"message\":\"boom\",\"type\":\"server_error\"}}\n\n";
    let mut decoder = StreamDecoder::<Chat>::new(Limits::default());
    decoder.push(error.as_bytes()).unwrap();
    assert!(decoder.push(b"data: [DONE]\n\n").unwrap().is_empty());
    assert_eq!(
        decoder.push(b"data: {\"choices\":[]}\n\n").unwrap_err(),
        isonomic_model_protocol::ProtocolError::AfterEnd
    );

    let stop = "data: {\"type\":\"message_stop\"}\n\n";
    let mut decoder = StreamDecoder::<Messages>::new(Limits::default());
    decoder.push(stop.as_bytes()).unwrap();
    assert!(decoder.push(stop.as_bytes()).unwrap().is_empty());
    assert!(decoder
        .push(b"data: {\"type\":\"message_start\",\"message\":{}}\n\n")
        .is_err());
}

#[cfg(feature = "server")]
fn accept<W: isonomic_model_protocol::WireServer>(body: Value) -> Vec<(DiagKind, String)> {
    W::accept_body(&serde_json::to_vec(&body).unwrap(), &Limits::default())
        .unwrap()
        .diags
        .into_iter()
        .map(|d| (d.kind, d.at))
        .collect()
}

#[cfg(feature = "server")]
#[test]
fn accept_lossy_paths() {
    let messages = json!({
        "model": "m", "max_tokens": 8,
        "system": [{ "type": "text", "text": "s", "citations": [{ "type": "char_location" }] },
                   { "type": "image", "source": { "type": "url", "url": "https://example.org/a.png" } }],
        "messages": [{ "role": "user", "content": "hi" }],
        "output_config": { "effort": "extreme" },
    });
    assert_eq!(
        accept::<Messages>(messages),
        [
            d(DiagKind::Dropped, "system[0]"),
            d(DiagKind::Dropped, "system[1]"),
            d(DiagKind::Dropped, "output_config.effort"),
        ]
    );
    let effort_only = json!({ "model": "m", "max_tokens": 8, "messages": [],
        "output_config": { "effort": "low" } });
    assert_eq!(
        accept::<Messages>(effort_only),
        [d(DiagKind::Approximated, "output_config.effort")]
    );

    let chat = json!({ "model": "m", "messages": [
        { "role": "user", "content": [{ "type": "file", "file": { "file_id": "f1" } }] },
        { "role": "developer", "content": [{ "type": "text", "text": "late" },
            { "type": "image_url", "image_url": { "url": "https://example.org/a.png" } }] },
        { "role": "assistant", "tool_calls": [{ "id": "c", "type": "function",
            "function": { "name": "t", "arguments": "{bad" } }] },
    ], "tools": [{ "type": "custom", "function": { "name": "x", "parameters": {} } }] });
    assert_eq!(
        accept::<Chat>(chat),
        [
            d(DiagKind::Dropped, "messages[0].content[0]"),
            d(DiagKind::Relocated, "messages[1]"),
            d(DiagKind::Dropped, "messages[1]"),
            d(DiagKind::Approximated, "messages[2].tool_calls[0]"),
            d(DiagKind::Dropped, "tools[0]"),
        ]
    );
}

#[cfg(feature = "server")]
#[test]
fn responses_lossy_paths() {
    use isonomic_model_protocol::Responses;
    let reasoning = Block::Reasoning {
        text: "r".into(),
        signature: Some("not-this-format".into()),
        redacted: None,
    };
    let mut conv = with_turns(vec![
        turn(
            Role::User,
            vec![
                Block::text("q"),
                Block::ToolResult {
                    call_id: "c".into(),
                    content: vec![Block::text("ok")],
                    is_error: true,
                },
            ],
        ),
        turn(
            Role::Assistant,
            vec![
                Block::text("a"),
                Block::Citation(Citation::default()),
                reasoning,
            ],
        ),
    ]);
    conv.system = vec!["one".into(), "two".into()];
    conv.sampling.stop = vec!["x".into()];
    assert_eq!(
        kinds::<Responses>(&conv, &opts()),
        [
            d(DiagKind::Approximated, "system"),
            d(DiagKind::Dropped, "sampling.stop"),
            d(DiagKind::Relocated, "turns[0].blocks[1]"),
            d(DiagKind::Dropped, "turns[0].blocks[1]"),
            d(DiagKind::Dropped, "turns[1].blocks[1]"),
            d(DiagKind::Dropped, "turns[1].blocks[2]"),
        ]
    );

    let request = json!({ "model": "m", "input": [
        { "type": "message", "id": "msg_1", "role": "assistant",
          "content": [{ "type": "output_text", "text": "a" }, { "type": "audio" }] },
        { "type": "web_search_call", "id": "ws_1" },
        { "role": "user", "content": [{ "type": "input_image", "file_id": "f1" }] },
        { "role": "developer", "content": "late" },
      ],
      "tools": [{ "type": "web_search", "name": "" }],
      "text": { "verbosity": "low" },
      "reasoning": { "effort": "extreme", "summary": "auto" } });
    assert_eq!(
        accept::<Responses>(request),
        [
            d(DiagKind::Dropped, "input[0].id"),
            d(DiagKind::Dropped, "input[0]"),
            d(DiagKind::Dropped, "input[1].id"),
            d(DiagKind::Dropped, "input[1]"),
            d(DiagKind::Dropped, "input[2].content[0]"),
            d(DiagKind::Relocated, "input[3]"),
            d(DiagKind::Dropped, "tools[0]"),
            d(DiagKind::Dropped, "text"),
            d(DiagKind::Dropped, "reasoning"),
            d(DiagKind::Dropped, "reasoning.effort"),
        ]
    );
}
