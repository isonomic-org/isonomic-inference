// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Each limit is enforced, and a hostile input costs bounded work.

use std::collections::BTreeMap;

use isonomic_model_protocol::{
    Block, Chat, Conversation, EmitOptions, Limits, MediaRef, Messages, ProtocolError,
    StreamDecoder, ToolSpec, Turn, WireClient,
};
use serde_json::{json, Value};

fn small() -> Limits {
    Limits {
        max_body_bytes: 200,
        max_depth: 8,
        max_turns: 3,
        max_blocks_per_turn: 3,
        max_tools: 2,
        max_schema_bytes: 64,
        max_inline_bytes: 4,
        max_line_bytes: 256,
        max_event_bytes: 512,
        max_stream_tool_calls: 2,
    }
}

fn limit_of(result: Result<impl Sized, ProtocolError>) -> &'static str {
    match result {
        Err(ProtocolError::Limit { what, .. }) => what,
        Err(other) => panic!("expected a limit error, got {other}"),
        Ok(_) => panic!("expected a limit error, got success"),
    }
}

fn emit_both(conv: &Conversation, opts: &EmitOptions<'_>) -> [&'static str; 2] {
    [
        limit_of(Messages::emit(conv, opts)),
        limit_of(Chat::emit(conv, opts)),
    ]
}

fn nested(depth: usize) -> Value {
    (0..depth).fold(json!(1), |inner, _| json!([inner]))
}

fn tool(schema: Value) -> ToolSpec {
    ToolSpec {
        name: "t".into(),
        description: None,
        input_schema: schema,
        strict: None,
    }
}

#[test]
fn conversation_limits_are_enforced_by_emit() {
    let opts = EmitOptions {
        limits: small(),
        ..EmitOptions::default()
    };
    let base = Conversation {
        turns: vec![Turn::user_text("hi")],
        ..Conversation::default()
    };

    let mut conv = base.clone();
    conv.turns = vec![Turn::user_text("hi"); 4];
    assert_eq!(emit_both(&conv, &opts), ["turns"; 2]);

    let mut conv = base.clone();
    conv.turns[0].blocks = vec![Block::text("x"); 4];
    assert_eq!(emit_both(&conv, &opts), ["blocks in a turn"; 2]);

    let mut conv = base.clone();
    conv.tools = vec![tool(json!({})); 3];
    assert_eq!(emit_both(&conv, &opts), ["tools"; 2]);

    let mut conv = base.clone();
    conv.tools = vec![tool(json!({ "description": "x".repeat(100) }))];
    assert_eq!(emit_both(&conv, &opts), ["schema bytes"; 2]);

    let mut conv = base.clone();
    conv.turns[0].blocks = vec![Block::ToolUse {
        id: "a".into(),
        name: "t".into(),
        input: nested(9),
    }];
    assert_eq!(emit_both(&conv, &opts), ["json depth"; 2]);
}

#[test]
fn inline_blobs_are_capped() {
    let blobs = BTreeMap::from([("b".to_string(), vec![0u8; 5])]);
    let opts = EmitOptions {
        limits: small(),
        blobs: &blobs,
        ..EmitOptions::default()
    };
    let conv = Conversation {
        turns: vec![Turn {
            id: None,
            role: isonomic_model_protocol::Role::User,
            blocks: vec![Block::Image {
                source: MediaRef::Blob {
                    id: "b".into(),
                    media_type: "image/png".into(),
                },
            }],
        }],
        ..Conversation::default()
    };
    assert_eq!(emit_both(&conv, &opts), ["inline blob bytes"; 2]);
    let missing = EmitOptions::default();
    assert_eq!(
        Messages::emit(&conv, &missing).unwrap_err(),
        ProtocolError::MissingBlob("b".into())
    );
}

#[test]
fn body_size_is_capped_both_ways() {
    let limits = small();
    let conv = Conversation {
        turns: vec![Turn::user_text("x".repeat(300))],
        ..Conversation::default()
    };
    let (request, _) = Chat::emit(&conv, &EmitOptions::default()).unwrap();
    assert_eq!(limit_of(limits.to_body(&request)), "body bytes");
    let body = vec![b' '; 201];
    assert_eq!(limit_of(Chat::parse_body(&body, &limits)), "body bytes");
    let deep = nested(9).to_string();
    assert_eq!(
        limit_of(Messages::parse_body(deep.as_bytes(), &limits)),
        "json depth"
    );
}

fn push<W: WireClient>(input: &str) -> Result<Vec<isonomic_model_protocol::Delta>, ProtocolError> {
    StreamDecoder::<W>::new(small()).push(input.as_bytes())
}

#[test]
fn stream_lines_and_events_are_capped() {
    let long = format!("data: {}\n", "x".repeat(300));
    assert_eq!(limit_of(push::<Chat>(&long)), "stream line bytes");
    let many = "data: 0123456789\n".repeat(60);
    assert_eq!(limit_of(push::<Chat>(&many)), "stream event bytes");
    let deep = format!("data: {}\n\n", nested(9));
    assert_eq!(limit_of(push::<Chat>(&deep)), "json depth");
    let deep = format!("data: {{\"type\":\"ping\",\"x\":{}}}\n\n", nested(9));
    assert_eq!(limit_of(push::<Messages>(&deep)), "json depth");
}

#[test]
fn tool_calls_per_stream_are_capped() {
    let call = |i: usize| {
        format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":{i},\"id\":\"c{i}\",\"function\":{{\"name\":\"t\"}}}}]}}}}]}}\n\n"
        )
    };
    let stream: String = (0..3).map(call).collect();
    assert_eq!(limit_of(push::<Chat>(&stream)), "tool calls in a stream");

    let block = |i: usize| {
        format!(
            "data: {{\"type\":\"content_block_start\",\"index\":{i},\"content_block\":{{\"type\":\"tool_use\",\"id\":\"t{i}\",\"name\":\"t\",\"input\":{{}}}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":{i}}}\n\n"
        )
    };
    let stream: String = (0..3).map(block).collect();
    assert_eq!(
        limit_of(push::<Messages>(&stream)),
        "tool calls in a stream"
    );
}

#[test]
fn open_blocks_per_stream_are_capped() {
    let stream: String = (0..4)
        .map(|i| {
            format!("data: {{\"type\":\"content_block_start\",\"index\":{i},\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n")
        })
        .collect();
    assert_eq!(limit_of(push::<Messages>(&stream)), "open stream blocks");
}

#[cfg(feature = "server")]
#[test]
fn accept_enforces_the_same_limits() {
    use isonomic_model_protocol::WireServer;
    let limits = small();
    let body = |messages: Value| {
        serde_json::to_vec(&json!({ "model": "m", "max_tokens": 1, "messages": messages })).unwrap()
    };
    let turn = json!({ "role": "user", "content": "hi" });
    assert_eq!(
        limit_of(Messages::accept_body(
            &body(json!([turn, turn, turn, turn])),
            &limits
        )),
        "turns"
    );
    assert_eq!(
        limit_of(Messages::accept_body(&vec![b' '; 201], &limits)),
        "body bytes"
    );
    let deep = body(
        json!([{ "role": "user", "content": [{ "type": "tool_use", "id": "a", "name": "t", "input": nested(9) }] }]),
    );
    assert_eq!(
        limit_of(Messages::accept_body(
            &deep,
            &Limits {
                max_body_bytes: 1 << 20,
                ..limits
            }
        )),
        "json depth"
    );
    let image = body(json!([{ "role": "user", "content": [{ "type": "image",
        "source": { "type": "base64", "media_type": "image/png", "data": "AAAAAAAA" } }] }]));
    assert_eq!(
        limit_of(Messages::accept_body(&image, &limits)),
        "inline blob bytes"
    );
    let chat = serde_json::to_vec(
        &json!({ "model": "m", "messages": [{ "role": "user", "content": [
        { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAAAAAA" } }] }] }),
    )
    .unwrap();
    assert_eq!(
        limit_of(Chat::accept_body(&chat, &limits)),
        "inline blob bytes"
    );
    let bad = serde_json::to_vec(
        &json!({ "model": "m", "messages": [{ "role": "user", "content": [
        { "type": "image_url", "image_url": { "url": "data:image/png;base64,!!!" } }] }] }),
    )
    .unwrap();
    assert!(matches!(
        Chat::accept_body(&bad, &Limits::default()),
        Err(ProtocolError::Invalid(_))
    ));
}

#[cfg(feature = "server")]
#[test]
fn responses_accept_and_decode_enforce_the_limits() {
    use isonomic_model_protocol::{Responses, WireServer};
    let limits = small();
    let image = serde_json::to_vec(
        &json!({ "model": "m", "input": [{ "role": "user", "content": [
        { "type": "input_image", "image_url": "data:image/png;base64,AAAAAAAA" }] }] }),
    )
    .unwrap();
    assert_eq!(
        limit_of(Responses::accept_body(&image, &limits)),
        "inline blob bytes"
    );
    let turns = serde_json::to_vec(&json!({ "model": "m", "input": [
        { "role": "user", "content": "a" }, { "role": "assistant", "content": "b" },
        { "role": "user", "content": "c" }, { "role": "assistant", "content": "d" }] }))
    .unwrap();
    assert_eq!(limit_of(Responses::accept_body(&turns, &limits)), "turns");
    let call = |i: usize| {
        format!(
        "data: {{\"type\":\"response.output_item.added\",\"output_index\":{i},\"item\":{{\"type\":\"function_call\",\"call_id\":\"c{i}\",\"name\":\"t\"}}}}\n\n")
    };
    let stream: String = (0..3).map(call).collect();
    assert_eq!(
        limit_of(push::<Responses>(&stream)),
        "tool calls in a stream"
    );
}
