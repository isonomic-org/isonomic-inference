//! Every lossy step in emit and decode raises a diagnostic. This file is the
//! executable list of lossy paths; docs/status.md lists the same paths.

use std::collections::BTreeMap;

use isonomic_model_protocol::{
    Block, Chat, Citation, Conversation, Delta, DiagKind, Effort, EmitOptions, Limits, MediaRef,
    Messages, ReasoningConfig, ResponseFormat, Role, StreamDecoder, Turn, Wire,
};
use serde_json::{json, Value};

fn kinds<W: Wire>(conv: &Conversation, opts: &EmitOptions<'_>) -> Vec<(DiagKind, String)> {
    W::emit(conv, opts)
        .unwrap()
        .diags
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
    Turn { role, blocks }
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
    let emitted = Messages::emit(&conv, &opts()).unwrap();
    let got: Vec<_> = emitted
        .diags
        .iter()
        .map(|d| (d.kind, d.at.clone()))
        .collect();
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
    let request = serde_json::to_value(&emitted.request).unwrap();
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
    let emitted = Chat::emit(&conv, &history).unwrap();
    assert_eq!(
        emitted.diags[0].detail,
        "the format has no place for a reasoning signature"
    );
    let request = serde_json::to_value(&emitted.request).unwrap();
    assert_eq!(request["messages"][0]["reasoning_content"], "r");
    assert_eq!(request["reasoning_effort"], "medium");
}

fn decode<W: Wire>(sse: &str) -> Vec<Delta> {
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
    let reply = Messages::decode_response(serde_json::from_value(body).unwrap()).unwrap();
    assert_eq!(reply.diags[0].kind, DiagKind::Unrecognized);
    assert_eq!(reply.turn.blocks, [Block::text("ok")]);

    let body: Value = json!({"choices": [
        {"message": {"refusal": "no", "tool_calls": [{"id": "c", "type": "function",
            "function": {"name": "t", "arguments": "{bad"}}]}, "finish_reason": "stop"},
        {"message": {"content": "second"}}]});
    let reply = Chat::decode_response(serde_json::from_value(body).unwrap()).unwrap();
    let kinds: Vec<_> = reply.diags.iter().map(|d| d.kind).collect();
    assert_eq!(
        kinds,
        [
            DiagKind::Dropped,
            DiagKind::Approximated,
            DiagKind::Approximated
        ]
    );
    assert_eq!(
        reply.stop,
        Some(isonomic_model_protocol::StopReason::Refusal)
    );
}
