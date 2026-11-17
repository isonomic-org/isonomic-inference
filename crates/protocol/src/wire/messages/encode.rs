// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hub → messages format: stream events, whole responses, errors.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::types::{ErrorBody, Response, WireUsage};
use crate::conversation::{Block, Citation, StopReason, Usage};
use crate::decode::Delta;
use crate::diag::{ErrorKind, HubError, ProtocolError};
use crate::encode::{frame, interleaved, status_of, EncodeOptions, Encoder};
use crate::parse::Reply;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Text,
    Thinking,
    Tool(usize),
}

enum ToolState {
    Pending { id: String, name: String },
    Opened,
}

/// Emits the format's event sequence: message start, content blocks one at
/// a time (start, deltas, stop), message delta, message stop.
pub struct MessagesEncoder {
    opts: EncodeOptions,
    start: Option<(Option<String>, Option<String>)>,
    started: bool,
    next_block: usize,
    open: Option<Open>,
    tools: BTreeMap<usize, ToolState>,
    late_usage: Usage,
    stop: Option<StopReason>,
    done: bool,
}

impl MessagesEncoder {
    pub(super) fn new(opts: EncodeOptions) -> Self {
        Self {
            opts,
            start: None,
            started: false,
            next_block: 0,
            open: None,
            tools: BTreeMap::new(),
            late_usage: Usage::default(),
            stop: None,
            done: false,
        }
    }

    fn event(out: &mut String, data: Value) {
        let kind = data["type"].as_str().unwrap_or_default().to_string();
        out.push_str(&frame(Some(&kind), &data.to_string()));
    }

    fn message_start(&mut self, out: &mut String, usage: Option<&Usage>) {
        if self.started {
            return;
        }
        self.started = true;
        let (id, model) = self.start.take().unwrap_or((None, None));
        let usage = usage.copied().unwrap_or_default();
        Self::event(
            out,
            json!({ "type": "message_start", "message": {
                "id": id.unwrap_or_else(|| self.opts.fallback_id.clone()),
                "type": "message", "role": "assistant",
                "model": model.unwrap_or_else(|| self.opts.fallback_model.clone()),
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {
                    "input_tokens": usage.input_tokens.unwrap_or(0),
                    "output_tokens": usage.output_tokens.unwrap_or(0),
                },
            }}),
        );
    }

    fn close(&mut self, out: &mut String) {
        if self.open.take().is_some() {
            let index = self.next_block - 1;
            Self::event(out, json!({ "type": "content_block_stop", "index": index }));
        }
    }

    fn open_block(&mut self, out: &mut String, kind: Open, block: Value) {
        self.close(out);
        let index = self.next_block;
        self.next_block += 1;
        self.open = Some(kind);
        Self::event(
            out,
            json!({ "type": "content_block_start", "index": index, "content_block": block }),
        );
    }

    fn delta(&self, out: &mut String, delta: Value) {
        let index = self.next_block - 1;
        Self::event(
            out,
            json!({ "type": "content_block_delta", "index": index, "delta": delta }),
        );
    }

    fn ensure(&mut self, out: &mut String, kind: Open) {
        if self.open != Some(kind) {
            let block = match kind {
                Open::Thinking => json!({ "type": "thinking", "thinking": "", "signature": "" }),
                _ => json!({ "type": "text", "text": "" }),
            };
            self.open_block(out, kind, block);
        }
    }

    fn open_tool(&mut self, out: &mut String, index: usize) -> Result<(), ProtocolError> {
        match self.tools.insert(index, ToolState::Opened) {
            Some(ToolState::Pending { id, name }) => {
                self.open_block(
                    out,
                    Open::Tool(index),
                    json!({ "type": "tool_use", "id": id, "name": name, "input": {} }),
                );
                Ok(())
            }
            _ => Err(interleaved(index)),
        }
    }

    /// Tool calls that started but never received arguments.
    fn flush_tools(&mut self, out: &mut String) {
        let pending: Vec<usize> = self
            .tools
            .iter()
            .filter(|(_, s)| matches!(s, ToolState::Pending { .. }))
            .map(|(i, _)| *i)
            .collect();
        for index in pending {
            let _ = self.open_tool(out, index);
        }
        self.close(out);
    }
}

impl Encoder for MessagesEncoder {
    fn encode(&mut self, delta: &Delta) -> Result<String, ProtocolError> {
        if self.done {
            return Err(ProtocolError::AfterEnd);
        }
        let mut out = String::new();
        if let Delta::Start { id, model } = delta {
            if self.started || self.start.is_some() {
                return Err(ProtocolError::Invalid("a second start".into()));
            }
            self.start = Some((id.clone(), model.clone()));
            return Ok(out);
        }
        if !self.started {
            if let Delta::Usage { usage } = delta {
                self.message_start(&mut out, Some(usage));
                return Ok(out);
            }
            self.message_start(&mut out, None);
        }
        match delta {
            Delta::Start { .. } | Delta::Notice { .. } => {}
            Delta::Text { text } | Delta::Refusal { text } => {
                self.ensure(&mut out, Open::Text);
                self.delta(&mut out, json!({ "type": "text_delta", "text": text }));
            }
            Delta::Reasoning { text } => {
                self.ensure(&mut out, Open::Thinking);
                self.delta(
                    &mut out,
                    json!({ "type": "thinking_delta", "thinking": text }),
                );
            }
            Delta::ReasoningSignature { signature } => {
                self.ensure(&mut out, Open::Thinking);
                self.delta(
                    &mut out,
                    json!({ "type": "signature_delta", "signature": signature }),
                );
            }
            Delta::ReasoningRedacted { data } => {
                self.open_block(
                    &mut out,
                    Open::Text,
                    json!({ "type": "redacted_thinking", "data": data }),
                );
                self.close(&mut out);
            }
            Delta::ToolCallStart { index, id, name } => {
                if self.tools.contains_key(index) {
                    return Err(ProtocolError::Invalid(format!(
                        "tool call {index} started twice"
                    )));
                }
                let id = id.clone().unwrap_or_else(|| format!("toolu_{index}"));
                self.tools.insert(
                    *index,
                    ToolState::Pending {
                        id,
                        name: name.clone(),
                    },
                );
            }
            Delta::ToolCallArgs { index, fragment } => {
                if self.open != Some(Open::Tool(*index)) {
                    self.open_tool(&mut out, *index)?;
                }
                self.delta(
                    &mut out,
                    json!({ "type": "input_json_delta", "partial_json": fragment }),
                );
            }
            Delta::Citation { citation } => {
                self.ensure(&mut out, Open::Text);
                self.delta(
                    &mut out,
                    json!({ "type": "citations_delta", "citation": wire_citation(citation) }),
                );
            }
            Delta::Usage { usage } => merge(&mut self.late_usage, usage),
            Delta::Stop { reason } => {
                self.flush_tools(&mut out);
                self.stop = Some(reason.clone());
            }
            Delta::Error { error } => {
                self.done = true;
                let body = error_body(error);
                out.push_str(&frame(
                    Some("error"),
                    &json!({ "type": "error", "error": body }).to_string(),
                ));
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> String {
        let mut out = String::new();
        if self.done {
            return out;
        }
        self.done = true;
        self.message_start(&mut out, None);
        self.flush_tools(&mut out);
        let usage = serde_json::to_value(wire_usage(&self.late_usage)).unwrap_or_default();
        let has_usage = usage.as_object().is_some_and(|m| !m.is_empty());
        if self.stop.is_some() || has_usage {
            let mut event = json!({ "type": "message_delta", "delta": {
                "stop_reason": self.stop.as_ref().map(stop_str), "stop_sequence": null,
            }});
            if has_usage {
                event["usage"] = usage;
            }
            Self::event(&mut out, event);
        }
        Self::event(&mut out, json!({ "type": "message_stop" }));
        out
    }

    fn keep_alive(&self) -> String {
        frame(Some("ping"), r#"{"type":"ping"}"#)
    }
}

fn merge(into: &mut Usage, from: &Usage) {
    into.input_tokens = from.input_tokens.or(into.input_tokens);
    into.output_tokens = from.output_tokens.or(into.output_tokens);
    into.cached_input_tokens = from.cached_input_tokens.or(into.cached_input_tokens);
    into.reasoning_tokens = from.reasoning_tokens.or(into.reasoning_tokens);
}

fn wire_usage(u: &Usage) -> WireUsage {
    WireUsage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_input_tokens: u.cached_input_tokens,
    }
}

/// A citation in this format's shape: its own raw form when it came from
/// this format, otherwise a web-result location built from the hub fields.
fn wire_citation(c: &Citation) -> Value {
    let native = c.raw.as_ref().filter(|raw| {
        raw["type"]
            .as_str()
            .is_some_and(|t| t.ends_with("_location"))
    });
    match native {
        Some(raw) => raw.clone(),
        None => json!({
            "type": "web_search_result_location",
            "url": c.url, "title": c.title, "cited_text": c.quote.clone().unwrap_or_default(),
        }),
    }
}

pub(crate) fn stop_str(reason: &StopReason) -> &str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::ToolUse => "tool_use",
        StopReason::Refusal | StopReason::ContentFilter => "refusal",
        StopReason::Other(other) => other,
    }
}

pub(super) fn encode_response(reply: &Reply, opts: &EncodeOptions) -> Response {
    let mut content: Vec<Value> = Vec::new();
    for block in &reply.turn.blocks {
        match block {
            Block::Text { text } => content.push(json!({ "type": "text", "text": text })),
            Block::Citation(c) => {
                if let Some(last) = content.last_mut().filter(|b| b["type"] == "text") {
                    let list = last
                        .as_object_mut()
                        .expect("a text block is an object")
                        .entry("citations")
                        .or_insert_with(|| json!([]));
                    if let Value::Array(items) = list {
                        items.push(wire_citation(c));
                    }
                }
            }
            Block::Reasoning {
                redacted: Some(data),
                ..
            } => content.push(json!({ "type": "redacted_thinking", "data": data })),
            Block::Reasoning {
                text, signature, ..
            } => content.push(json!({
                "type": "thinking", "thinking": text,
                "signature": signature.clone().unwrap_or_default(),
            })),
            Block::ToolUse { id, name, input } => content.push(json!({
                "type": "tool_use", "id": id, "name": name, "input": input,
            })),
            _ => {}
        }
    }
    Response {
        id: Some(reply.id.clone().unwrap_or_else(|| opts.fallback_id.clone())),
        kind: Some("message".to_string()),
        role: Some("assistant".to_string()),
        model: Some(
            reply
                .model
                .clone()
                .unwrap_or_else(|| opts.fallback_model.clone()),
        ),
        content,
        stop_reason: reply.stop.as_ref().map(|r| stop_str(r).to_string()),
        stop_sequence: None,
        usage: reply.usage.as_ref().map(wire_usage),
    }
}

fn error_body(error: &HubError) -> ErrorBody {
    let kind = match error.kind {
        ErrorKind::InvalidRequest => "invalid_request_error",
        ErrorKind::Authentication => "authentication_error",
        ErrorKind::PermissionDenied => "permission_error",
        ErrorKind::NotFound => "not_found_error",
        ErrorKind::RequestTooLarge => "request_too_large",
        ErrorKind::RateLimited => "rate_limit_error",
        ErrorKind::Overloaded => "overloaded_error",
        ErrorKind::Timeout => "timeout_error",
        ErrorKind::Server | ErrorKind::Other => "api_error",
    };
    ErrorBody {
        kind: error.raw_type.clone().unwrap_or_else(|| kind.to_string()),
        message: error.message.clone(),
    }
}

pub(super) fn encode_error(error: &HubError) -> (u16, Vec<u8>) {
    let status = match error.kind {
        ErrorKind::Overloaded => 529,
        kind => status_of(kind),
    };
    let body = json!({ "type": "error", "error": error_body(error) });
    (status, body.to_string().into_bytes())
}
