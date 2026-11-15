//! Hub → responses format: whole responses and errors, and the helpers the
//! stream encoder shares with them.

use serde_json::{json, Value};

use super::emit::parse_signature;
use super::types::{InputDetails, OutputDetails, Response, WireUsage};
use crate::conversation::{Block, Citation, StopReason, Usage};
use crate::decode::Reply;
use crate::diag::{ErrorKind, HubError};
use crate::encode::{status_of, EncodeOptions};

pub(super) const INTERRUPTED: &str = "interrupted";

pub(super) fn item_id(prefix: &str, index: usize) -> String {
    format!("{prefix}_{index}")
}

pub(super) fn incomplete_reason(stop: Option<&StopReason>) -> Option<&'static str> {
    match stop {
        Some(StopReason::MaxTokens) => Some("max_output_tokens"),
        Some(StopReason::ContentFilter) => Some("content_filter"),
        _ => None,
    }
}

pub(super) fn annotation(c: &Citation) -> Value {
    let native = |raw: &&Value| raw["type"] == "url_citation" && raw.get("url").is_some();
    match c.raw.as_ref().filter(native) {
        Some(raw) => raw.clone(),
        None => json!({ "type": "url_citation", "url": c.url, "title": c.title,
            "start_index": 0, "end_index": 0 }),
    }
}

pub(super) fn wire_usage(u: &Usage) -> WireUsage {
    WireUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        input_tokens_details: u.cached_input_tokens.map(|n| InputDetails {
            cached_tokens: Some(n),
        }),
        output_tokens_details: u.reasoning_tokens.map(|n| OutputDetails {
            reasoning_tokens: Some(n),
        }),
        total_tokens: match (u.input_tokens, u.output_tokens) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        },
    }
}

pub(super) fn encode_response(reply: &Reply, opts: &EncodeOptions) -> Response {
    let mut output: Vec<Value> = Vec::new();
    let mut parts: Vec<Value> = Vec::new();
    let refused = reply.stop == Some(StopReason::Refusal);
    let flush = |parts: &mut Vec<Value>, output: &mut Vec<Value>| {
        if !parts.is_empty() {
            let index = output.len();
            output.push(
                json!({ "type": "message", "id": item_id("msg", index), "status": "completed",
                "role": "assistant", "content": std::mem::take(parts) }),
            );
        }
    };
    for block in &reply.turn.blocks {
        match block {
            Block::Text { text } if refused => {
                parts.push(json!({ "type": "refusal", "refusal": text }))
            }
            Block::Text { text } => {
                parts.push(json!({ "type": "output_text", "text": text, "annotations": [] }))
            }
            Block::Citation(c) => {
                if let Some(list) = parts
                    .last_mut()
                    .and_then(|p| p["annotations"].as_array_mut())
                {
                    list.push(annotation(c));
                }
            }
            Block::Reasoning {
                text, signature, ..
            } => {
                flush(&mut parts, &mut output);
                let index = output.len();
                let mut item = signature
                    .as_deref()
                    .and_then(parse_signature)
                    .unwrap_or_else(|| json!({ "type": "reasoning", "id": item_id("rs", index) }));
                item["summary"] = json!([{ "type": "summary_text", "text": text }]);
                output.push(item);
            }
            Block::ToolUse { id, name, input } => {
                flush(&mut parts, &mut output);
                let index = output.len();
                output.push(
                    json!({ "type": "function_call", "id": item_id("fc", index), "call_id": id,
                    "name": name, "arguments": input.to_string(), "status": "completed" }),
                );
            }
            _ => {}
        }
    }
    flush(&mut parts, &mut output);
    let reason = incomplete_reason(reply.stop.as_ref());
    Response {
        id: Some(reply.id.clone().unwrap_or_else(|| opts.fallback_id.clone())),
        object: Some("response".to_string()),
        created_at: Some(opts.created),
        status: Some(
            if reason.is_some() {
                "incomplete"
            } else {
                "completed"
            }
            .to_string(),
        ),
        model: Some(
            reply
                .model
                .clone()
                .unwrap_or_else(|| opts.fallback_model.clone()),
        ),
        output,
        incomplete_details: reason.map(|r| json!({ "reason": r })),
        error: None,
        usage: reply.usage.as_ref().map(wire_usage),
    }
}

pub(super) fn error_type(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::InvalidRequest => "invalid_request_error",
        ErrorKind::Authentication => "authentication_error",
        ErrorKind::PermissionDenied => "permission_error",
        ErrorKind::NotFound => "not_found_error",
        ErrorKind::RequestTooLarge => "request_too_large",
        ErrorKind::RateLimited => "rate_limit_error",
        ErrorKind::Overloaded => "unavailable_error",
        ErrorKind::Timeout => "timeout_error",
        ErrorKind::Server | ErrorKind::Other => "server_error",
    }
}

pub(super) fn encode_error(error: &HubError) -> (u16, Vec<u8>) {
    let kind = error
        .raw_type
        .clone()
        .unwrap_or_else(|| error_type(error.kind).to_string());
    let body =
        json!({ "error": { "message": error.message, "type": kind, "param": null, "code": null } });
    (status_of(error.kind), body.to_string().into_bytes())
}
