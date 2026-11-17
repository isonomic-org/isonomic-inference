// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The P1a gate, client direction only:
//!
//! - emit, then parse the matching response: the reply means what the
//!   fixture says, and feeding it back as the next turn emits with only the
//!   listed diagnostics;
//! - cross-format: every fixture conversation, emitted as messages and as
//!   chat, carries the same meaning, with only the listed losses.
//!
//! Meaning is compared through a small, format-neutral projection of each
//! request, written here from the two formats' documentation.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use isonomic_model_protocol::{
    Block, Chat, Conversation, DiagKind, EmitOptions, Limits, Messages, Role, Turn, WireClient,
};
use serde_json::{json, Value};

fn read(path: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(path);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn conversation(name: &str) -> Conversation {
    serde_json::from_value(read(&format!("conversations/{name}.json"))).unwrap()
}

fn blobs() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([("img1".to_string(), vec![0x89, b'P', b'N', b'G'])])
}

fn kinds(diags: &[isonomic_model_protocol::Diag]) -> Vec<(DiagKind, String)> {
    diags.iter().map(|d| (d.kind, d.at.clone())).collect()
}

/// Emit, parse the reply, then send the reply back as the next turn.
fn continue_with<W: WireClient>(scenario: &str) -> Vec<(DiagKind, String)> {
    let fixture = read(&format!("{}/{scenario}.json", W::NAME));
    let mut conv = conversation(fixture["conversation"].as_str().unwrap());
    let blobs = blobs();
    let opts = EmitOptions {
        blobs: &blobs,
        ..EmitOptions::default()
    };
    W::emit(&conv, &opts).unwrap();
    let body = serde_json::to_vec(&fixture["response"]).unwrap();
    let (reply, diags) = W::parse_body(&body, &Limits::default()).unwrap();
    assert!(diags.is_empty(), "{scenario}: {diags:?}");
    assert_eq!(common::reply_json(&reply, &diags), fixture["reply"]);
    let turn = reply.turns[0].clone();
    // A tool call is answered before the conversation goes on.
    let calls: Vec<Block> = turn
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolUse { id, .. } => Some(Block::ToolResult {
                call_id: id.clone(),
                content: vec![Block::text("18C, sunny")],
                is_error: false,
            }),
            _ => None,
        })
        .collect();
    conv.turns.push(turn);
    if !calls.is_empty() {
        conv.turns.push(Turn {
            id: None,
            role: Role::User,
            blocks: calls,
        });
    }
    let (_, diags) = W::emit(&conv, &opts).unwrap();
    kinds(&diags)
}

#[test]
fn a_parsed_reply_feeds_the_next_turn() {
    for scenario in ["text", "tool_call", "reasoning"] {
        assert_eq!(
            continue_with::<Messages>(scenario),
            [],
            "messages/{scenario}"
        );
        let expected: Vec<(DiagKind, String)> = match scenario {
            // The chat format sends earlier reasoning back only when asked.
            "reasoning" => vec![(DiagKind::Dropped, "turns[1].blocks[0]".into())],
            _ => vec![],
        };
        assert_eq!(continue_with::<Chat>(scenario), expected, "chat/{scenario}");
    }
}

/// A format-neutral projection of a messages request.
fn meaning_of_messages(req: &Value) -> Value {
    let system: Vec<Value> = match &req["system"] {
        Value::String(s) => vec![json!(s)],
        Value::Array(blocks) => blocks.iter().map(|b| b["text"].clone()).collect(),
        _ => vec![],
    };
    let mut turns = Vec::new();
    for m in req["messages"].as_array().unwrap() {
        let items: Vec<Value> = match &m["content"] {
            Value::String(text) => vec![json!({ "text": text })],
            Value::Array(blocks) => blocks.iter().map(messages_item).collect(),
            _ => vec![],
        };
        turns.push(json!({ "role": m["role"], "items": items }));
    }
    let choice = &req["tool_choice"];
    json!({
        "system": system,
        "turns": turns,
        "tools": req["tools"].as_array().map(|t| t.iter().map(|t| json!({
            "name": t["name"], "description": t["description"], "schema": t["input_schema"],
        })).collect::<Vec<_>>()),
        "tool_choice": match choice["type"].as_str() {
            Some("any") => json!("required"),
            Some("tool") => json!({ "tool": choice["name"] }),
            Some(other) => json!(other),
            None => Value::Null,
        },
        "parallel": choice["disable_parallel_tool_use"].as_bool().map(|d| !d),
        "max_tokens": req["max_tokens"],
        "effort": req["output_config"]["effort"],
        "schema": req["output_config"]["format"]["schema"],
    })
}

fn messages_item(b: &Value) -> Value {
    match b["type"].as_str().unwrap() {
        "text" => json!({ "text": b["text"] }),
        "image" => json!({ "image": format!("data:{};base64,{}",
            b["source"]["media_type"].as_str().unwrap(), b["source"]["data"].as_str().unwrap()) }),
        "tool_use" => json!({ "call": { "id": b["id"], "name": b["name"], "input": b["input"] } }),
        "tool_result" => {
            let text: String = b["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["text"].as_str())
                .collect();
            json!({ "result": { "id": b["tool_use_id"], "text": text } })
        }
        other => json!({ other: b }),
    }
}

/// A format-neutral projection of a chat request. Tool messages join the
/// user turn after them, as in the messages format.
fn meaning_of_chat(req: &Value) -> Value {
    let mut system = Vec::new();
    let mut turns: Vec<Value> = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    for m in req["messages"].as_array().unwrap() {
        match m["role"].as_str().unwrap() {
            "system" | "developer" => system.push(m["content"].clone()),
            "tool" => {
                pending.push(json!({ "result": { "id": m["tool_call_id"], "text": m["content"] } }))
            }
            "user" => {
                let mut items = std::mem::take(&mut pending);
                match &m["content"] {
                    Value::String(text) => items.push(json!({ "text": text })),
                    Value::Array(parts) => {
                        items.extend(parts.iter().map(|p| match p["type"].as_str().unwrap() {
                            "text" => json!({ "text": p["text"] }),
                            "image_url" => json!({ "image": p["image_url"]["url"] }),
                            other => json!({ other: p }),
                        }))
                    }
                    _ => {}
                }
                turns.push(json!({ "role": "user", "items": items }));
            }
            _ => {
                if !pending.is_empty() {
                    turns.push(json!({ "role": "user", "items": std::mem::take(&mut pending) }));
                }
                let mut items = Vec::new();
                if let Some(text) = m["content"].as_str() {
                    items.push(json!({ "text": text }));
                }
                for call in m["tool_calls"].as_array().into_iter().flatten() {
                    let input: Value =
                        serde_json::from_str(call["function"]["arguments"].as_str().unwrap())
                            .unwrap();
                    items.push(json!({ "call": { "id": call["id"], "name": call["function"]["name"], "input": input } }));
                }
                turns.push(json!({ "role": "assistant", "items": items }));
            }
        }
    }
    if !pending.is_empty() {
        turns.push(json!({ "role": "user", "items": pending }));
    }
    let choice = &req["tool_choice"];
    json!({
        "system": system,
        "turns": turns,
        "tools": req["tools"].as_array().map(|t| t.iter().map(|t| json!({
            "name": t["function"]["name"], "description": t["function"]["description"],
            "schema": t["function"]["parameters"],
        })).collect::<Vec<_>>()),
        "tool_choice": match choice {
            Value::String(mode) => json!(mode),
            Value::Object(_) => json!({ "tool": choice["function"]["name"] }),
            _ => Value::Null,
        },
        // An explicit "allowed" is every format's default.
        "parallel": req["parallel_tool_calls"].as_bool().filter(|p| !p),
        "max_tokens": req["max_completion_tokens"],
        "effort": req["reasoning_effort"],
        "schema": req["response_format"]["json_schema"]["schema"],
    })
}

#[test]
fn a_conversation_means_the_same_as_messages_and_as_chat() {
    let blobs = blobs();
    let opts = EmitOptions {
        blobs: &blobs,
        ..EmitOptions::default()
    };
    for scenario in [
        "text",
        "tool_call",
        "parallel_tool_calls",
        "reasoning",
        "image_input",
        "structured_output",
    ] {
        let conv = conversation(scenario);
        let (messages, messages_diags) = Messages::emit(&conv, &opts).unwrap();
        let (chat, chat_diags) = Chat::emit(&conv, &opts).unwrap();
        assert_eq!(
            meaning_of_messages(&serde_json::to_value(messages).unwrap()),
            meaning_of_chat(&serde_json::to_value(chat).unwrap()),
            "{scenario}"
        );
        // The losses on the way, each listed in docs/status-protocol.md.
        let expected_messages: Vec<(DiagKind, String)> = match scenario {
            "parallel_tool_calls" => vec![(DiagKind::Approximated, "parallel_tool_calls".into())],
            "structured_output" => vec![(DiagKind::Dropped, "response_format".into())],
            _ => vec![],
        };
        assert_eq!(
            kinds(&messages_diags),
            expected_messages,
            "messages/{scenario}"
        );
        assert_eq!(kinds(&chat_diags), [], "chat/{scenario}");
    }
}
