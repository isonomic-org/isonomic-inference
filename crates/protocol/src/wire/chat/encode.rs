//! Hub → chat format: stream chunks, whole responses, errors.

use serde_json::{json, Value};

use super::types::{
    Choice, CompletionDetails, ErrorBody, PromptDetails, ReplyMessage, Response, ToolCall,
    WireUsage,
};
use crate::conversation::{Block, Citation, StopReason, Usage};
use crate::decode::Delta;
use crate::diag::{ErrorKind, HubError, ProtocolError};
use crate::encode::{frame, status_of, EncodeOptions, Encoder};
use crate::parse::Reply;

/// Emits one chunk per delta, a finish chunk, an optional usage chunk and
/// the done marker.
pub struct ChatEncoder {
    opts: EncodeOptions,
    id: String,
    model: String,
    started: bool,
    usage: Usage,
    done: bool,
}

impl ChatEncoder {
    pub(super) fn new(opts: EncodeOptions) -> Self {
        Self {
            id: opts.fallback_id.clone(),
            model: opts.fallback_model.clone(),
            opts,
            started: false,
            usage: Usage::default(),
            done: false,
        }
    }

    fn chunk(&self, choices: Value, usage: Option<Value>) -> String {
        let mut chunk = json!({
            "id": self.id, "object": "chat.completion.chunk",
            "created": self.opts.created, "model": self.model, "choices": choices,
        });
        if let Some(usage) = usage {
            chunk["usage"] = usage;
        }
        frame(None, &chunk.to_string())
    }

    fn delta(&self, delta: Value, finish: Option<&str>) -> String {
        self.chunk(
            json!([{ "index": 0, "delta": delta, "finish_reason": finish }]),
            None,
        )
    }
}

impl Encoder for ChatEncoder {
    fn encode(&mut self, delta: &Delta) -> Result<String, ProtocolError> {
        if self.done {
            return Err(ProtocolError::AfterEnd);
        }
        let mut out = String::new();
        if let Delta::Start { id, model } = delta {
            if self.started {
                return Err(ProtocolError::Invalid("a second start".into()));
            }
            self.id = id.clone().unwrap_or_else(|| self.id.clone());
            self.model = model.clone().unwrap_or_else(|| self.model.clone());
        }
        if !self.started {
            self.started = true;
            out.push_str(&self.delta(json!({ "role": "assistant", "content": "" }), None));
        }
        match delta {
            Delta::Start { .. }
            | Delta::Notice { .. }
            | Delta::ReasoningSignature { .. }
            | Delta::ReasoningRedacted { .. } => {}
            Delta::Text { text } => out.push_str(&self.delta(json!({ "content": text }), None)),
            Delta::Reasoning { text } => {
                out.push_str(&self.delta(json!({ "reasoning_content": text }), None));
            }
            Delta::Refusal { text } => out.push_str(&self.delta(json!({ "refusal": text }), None)),
            Delta::ToolCallStart { index, id, name } => {
                let id = id.clone().unwrap_or_else(|| format!("call_{index}"));
                out.push_str(&self.delta(
                    json!({ "tool_calls": [{ "index": index, "id": id, "type": "function",
                        "function": { "name": name, "arguments": "" } }] }),
                    None,
                ));
            }
            Delta::ToolCallArgs { index, fragment } => out.push_str(&self.delta(
                json!({ "tool_calls": [{ "index": index, "function": { "arguments": fragment } }] }),
                None,
            )),
            Delta::Citation { citation } => out.push_str(&self.delta(
                json!({ "annotations": [annotation(citation)] }),
                None,
            )),
            Delta::Usage { usage } => {
                let u = &mut self.usage;
                u.input_tokens = usage.input_tokens.or(u.input_tokens);
                u.output_tokens = usage.output_tokens.or(u.output_tokens);
                u.cached_input_tokens = usage.cached_input_tokens.or(u.cached_input_tokens);
                u.reasoning_tokens = usage.reasoning_tokens.or(u.reasoning_tokens);
            }
            Delta::Stop { reason } => {
                out.push_str(&self.delta(json!({}), Some(finish_reason(reason))));
            }
            Delta::Error { error } => {
                self.done = true;
                out.push_str(&frame(
                    None,
                    &json!({ "error": error_body(error) }).to_string(),
                ));
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> String {
        if self.done {
            return String::new();
        }
        self.done = true;
        let mut out = String::new();
        if !self.started {
            self.started = true;
            out.push_str(&self.delta(json!({ "role": "assistant", "content": "" }), None));
        }
        if self.opts.stream_usage && self.usage != Usage::default() {
            let usage = serde_json::to_value(wire_usage(&self.usage)).unwrap_or_default();
            out.push_str(&self.chunk(json!([]), Some(usage)));
        }
        out.push_str(&frame(None, "[DONE]"));
        out
    }

    fn keep_alive(&self) -> String {
        ": keep-alive\n\n".to_string()
    }
}

fn finish_reason(reason: &StopReason) -> &str {
    match reason {
        StopReason::EndTurn | StopReason::StopSequence | StopReason::Refusal => "stop",
        StopReason::MaxTokens => "length",
        StopReason::ToolUse => "tool_calls",
        StopReason::ContentFilter => "content_filter",
        StopReason::Other(other) => other,
    }
}

fn annotation(c: &Citation) -> Value {
    let native = |raw: &&Value| raw["type"] == "url_citation" && raw.get("url_citation").is_some();
    match c.raw.as_ref().filter(native) {
        Some(raw) => raw.clone(),
        None => {
            json!({ "type": "url_citation", "url_citation": { "url": c.url, "title": c.title } })
        }
    }
}

fn wire_usage(u: &Usage) -> WireUsage {
    WireUsage {
        prompt_tokens: u.input_tokens,
        completion_tokens: u.output_tokens,
        prompt_tokens_details: u.cached_input_tokens.map(|n| PromptDetails {
            cached_tokens: Some(n),
        }),
        completion_tokens_details: u.reasoning_tokens.map(|n| CompletionDetails {
            reasoning_tokens: Some(n),
        }),
    }
}

pub(super) fn encode_response(reply: &Reply, opts: &EncodeOptions) -> Response {
    let mut message = ReplyMessage {
        role: Some("assistant".to_string()),
        ..ReplyMessage::default()
    };
    let mut text = String::new();
    let mut reasoning = String::new();
    for block in &reply.turn.blocks {
        match block {
            Block::Text { text: t } => text.push_str(t),
            Block::Reasoning { text: r, .. } => reasoning.push_str(r),
            Block::Citation(c) => message.annotations.push(annotation(c)),
            Block::ToolUse { id, name, input } => message.tool_calls.push(ToolCall {
                id: id.clone(),
                kind: "function".to_string(),
                function: super::types::FunctionCall {
                    name: name.clone(),
                    arguments: input.to_string(),
                },
            }),
            _ => {}
        }
    }
    if reply.stop == Some(StopReason::Refusal) {
        message.refusal = Some(text);
    } else if !text.is_empty() || message.tool_calls.is_empty() {
        message.content = Some(text);
    }
    message.reasoning_content = (!reasoning.is_empty()).then_some(reasoning);
    Response {
        id: Some(reply.id.clone().unwrap_or_else(|| opts.fallback_id.clone())),
        object: Some("chat.completion".to_string()),
        created: Some(opts.created),
        model: Some(
            reply
                .model
                .clone()
                .unwrap_or_else(|| opts.fallback_model.clone()),
        ),
        choices: vec![Choice {
            index: 0,
            message,
            finish_reason: reply.stop.as_ref().map(|r| finish_reason(r).to_string()),
        }],
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
        ErrorKind::Overloaded => "unavailable_error",
        ErrorKind::Timeout => "timeout_error",
        ErrorKind::Server | ErrorKind::Other => "server_error",
    };
    ErrorBody {
        message: error.message.clone(),
        kind: Some(error.raw_type.clone().unwrap_or_else(|| kind.to_string())),
        code: None,
    }
}

pub(super) fn encode_error(error: &HubError) -> (u16, Vec<u8>) {
    let body = json!({ "error": error_body(error) });
    (status_of(error.kind), body.to_string().into_bytes())
}
