//! Helpers shared by the integration tests.
#![allow(dead_code)]

use isonomic_model_protocol::{Conversation, Diag, Turn};
use serde_json::{json, Value};

/// A parsed reply in the fixtures' `reply` shape.
pub fn reply_json(conv: &Conversation, diags: &[Diag]) -> Value {
    let turn = &conv.turns[0];
    json!({
        "id": turn.id,
        "model": conv.model,
        "turn": { "role": turn.role, "blocks": turn.blocks },
        "stop": conv.stop_reason,
        "usage": conv.usage,
        "diags": diags,
    })
}

/// A fixture `reply` as the conversation a server would answer with.
pub fn conversation_of_reply(reply: &Value) -> Conversation {
    let mut turn: Turn = serde_json::from_value(reply["turn"].clone()).unwrap();
    turn.id = reply["id"].as_str().map(str::to_string);
    Conversation {
        model: reply["model"].as_str().map(str::to_string),
        turns: vec![turn],
        usage: serde_json::from_value(reply["usage"].clone()).unwrap(),
        stop_reason: serde_json::from_value(reply["stop"].clone()).unwrap(),
        ..Conversation::default()
    }
}
