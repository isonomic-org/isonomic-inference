//! Hub → responses format: stream events, whole responses, errors.
//!
//! The format's closing events repeat each item in full, and the final event
//! carries the whole response, so this encoder keeps the reply's items. That
//! is the format's design; decoders never keep a reply.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::emit::parse_signature;
use super::types::{InputDetails, OutputDetails, Response, WireUsage};
use crate::conversation::{Block, Citation, StopReason, Usage};
use crate::decode::{Delta, Reply};
use crate::diag::{ErrorKind, HubError, ProtocolError};
use crate::encode::{frame, status_of, EncodeOptions, Encoder};

struct Message {
    index: usize,
    parts: Vec<Value>,
}

struct Reasoning {
    index: usize,
    text: String,
    encrypted: Option<String>,
}

struct Call {
    index: usize,
    call_id: String,
    name: String,
    args: String,
}

pub struct ResponsesEncoder {
    opts: EncodeOptions,
    id: String,
    model: String,
    seq: u64,
    started: bool,
    next: usize,
    message: Option<Message>,
    reasoning: Option<Reasoning>,
    calls: BTreeMap<usize, Call>,
    output: BTreeMap<usize, Value>,
    usage: Usage,
    stop: Option<StopReason>,
    done: bool,
}

fn item_id(prefix: &str, index: usize) -> String {
    format!("{prefix}_{index}")
}

impl ResponsesEncoder {
    pub(super) fn new(opts: EncodeOptions) -> Self {
        Self {
            id: opts.fallback_id.clone(),
            model: opts.fallback_model.clone(),
            opts,
            seq: 0,
            started: false,
            next: 0,
            message: None,
            reasoning: None,
            calls: BTreeMap::new(),
            output: BTreeMap::new(),
            usage: Usage::default(),
            stop: None,
            done: false,
        }
    }

    fn ev(&mut self, out: &mut String, kind: &str, mut data: Value) {
        data["type"] = json!(kind);
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        out.push_str(&frame(Some(kind), &data.to_string()));
    }

    fn response(&self, status: &str) -> Value {
        let mut response = json!({
            "id": self.id, "object": "response", "created_at": self.opts.created,
            "status": status, "model": self.model,
            "output": self.output.values().cloned().collect::<Vec<_>>(),
        });
        if self.usage != Usage::default() {
            response["usage"] = serde_json::to_value(wire_usage(&self.usage)).unwrap_or_default();
        }
        if status == "incomplete" {
            let reason = incomplete_reason(self.stop.as_ref()).unwrap_or(INTERRUPTED);
            response["incomplete_details"] = json!({ "reason": reason });
        }
        response
    }

    fn start(&mut self, out: &mut String) {
        if self.started {
            return;
        }
        self.started = true;
        let response = self.response("in_progress");
        self.ev(out, "response.created", json!({ "response": response }));
        self.ev(out, "response.in_progress", json!({ "response": response }));
    }

    fn close_part(&mut self, out: &mut String) {
        let Some(message) = &self.message else { return };
        let (index, content_index) = (message.index, message.parts.len().saturating_sub(1));
        let Some(part) = message.parts.last().cloned() else {
            return;
        };
        let item = item_id("msg", index);
        let base =
            json!({ "item_id": item, "output_index": index, "content_index": content_index });
        let mut done = base.clone();
        if part["type"] == "refusal" {
            done["refusal"] = part["refusal"].clone();
            self.ev(out, "response.refusal.done", done);
        } else {
            done["text"] = part["text"].clone();
            self.ev(out, "response.output_text.done", done);
        }
        let mut part_done = base;
        part_done["part"] = part;
        self.ev(out, "response.content_part.done", part_done);
    }

    fn close_message(&mut self, out: &mut String) {
        self.close_part(out);
        if let Some(message) = self.message.take() {
            let item = json!({ "type": "message", "id": item_id("msg", message.index),
                "status": "completed", "role": "assistant", "content": message.parts });
            self.output.insert(message.index, item.clone());
            self.ev(
                out,
                "response.output_item.done",
                json!({ "output_index": message.index, "item": item }),
            );
        }
    }

    fn close_reasoning(&mut self, out: &mut String) {
        let Some(r) = self.reasoning.take() else {
            return;
        };
        let id = item_id("rs", r.index);
        let base = json!({ "item_id": id, "output_index": r.index, "summary_index": 0 });
        let mut text_done = base.clone();
        text_done["text"] = json!(r.text);
        self.ev(out, "response.reasoning_summary_text.done", text_done);
        let mut part_done = base;
        part_done["part"] = json!({ "type": "summary_text", "text": r.text });
        self.ev(out, "response.reasoning_summary_part.done", part_done);
        let mut item = json!({ "type": "reasoning", "id": id,
            "summary": [{ "type": "summary_text", "text": r.text }] });
        if let Some(encrypted) = r.encrypted {
            item["encrypted_content"] = json!(encrypted);
        }
        self.output.insert(r.index, item.clone());
        self.ev(
            out,
            "response.output_item.done",
            json!({ "output_index": r.index, "item": item }),
        );
    }

    fn close_calls(&mut self, out: &mut String) {
        for (_, call) in std::mem::take(&mut self.calls) {
            let id = item_id("fc", call.index);
            self.ev(
                out,
                "response.function_call_arguments.done",
                json!({ "item_id": id, "output_index": call.index, "arguments": call.args }),
            );
            let item = json!({ "type": "function_call", "id": id, "call_id": call.call_id,
                "name": call.name, "arguments": call.args, "status": "completed" });
            self.output.insert(call.index, item.clone());
            self.ev(
                out,
                "response.output_item.done",
                json!({ "output_index": call.index, "item": item }),
            );
        }
    }

    /// The open message's current part, of the given type.
    fn part(&mut self, out: &mut String, kind: &str) -> (usize, usize) {
        self.close_reasoning(out);
        if self.message.is_none() {
            let index = self.next;
            self.next += 1;
            self.message = Some(Message {
                index,
                parts: Vec::new(),
            });
            let item = json!({ "type": "message", "id": item_id("msg", index),
                "status": "in_progress", "role": "assistant", "content": [] });
            self.ev(
                out,
                "response.output_item.added",
                json!({ "output_index": index, "item": item }),
            );
        }
        let same = self
            .message
            .as_ref()
            .and_then(|m| m.parts.last())
            .is_some_and(|p| p["type"] == kind);
        if !same {
            self.close_part(out);
            let part = if kind == "refusal" {
                json!({ "type": "refusal", "refusal": "" })
            } else {
                json!({ "type": "output_text", "text": "", "annotations": [] })
            };
            let message = self.message.as_mut().expect("opened above");
            message.parts.push(part.clone());
            let (index, content_index) = (message.index, message.parts.len() - 1);
            self.ev(
                out,
                "response.content_part.added",
                json!({ "item_id": item_id("msg", index),
                "output_index": index, "content_index": content_index, "part": part }),
            );
        }
        let message = self.message.as_ref().expect("opened above");
        (message.index, message.parts.len() - 1)
    }

    fn reasoning_item(&mut self, out: &mut String) -> usize {
        self.close_message(out);
        if let Some(r) = &self.reasoning {
            return r.index;
        }
        let index = self.next;
        self.next += 1;
        self.reasoning = Some(Reasoning {
            index,
            text: String::new(),
            encrypted: None,
        });
        let id = item_id("rs", index);
        self.ev(out, "response.output_item.added",
            json!({ "output_index": index, "item": { "type": "reasoning", "id": id, "summary": [] } }));
        self.ev(out, "response.reasoning_summary_part.added", json!({ "item_id": id,
            "output_index": index, "summary_index": 0, "part": { "type": "summary_text", "text": "" } }));
        index
    }
}

impl Encoder for ResponsesEncoder {
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
        self.start(&mut out);
        match delta {
            Delta::Start { .. } | Delta::Notice { .. } | Delta::ReasoningRedacted { .. } => {}
            Delta::Text { text } => {
                let (index, content_index) = self.part(&mut out, "output_text");
                let part = self
                    .message
                    .as_mut()
                    .and_then(|m| m.parts.last_mut())
                    .expect("open");
                part["text"] = json!(format!(
                    "{}{text}",
                    part["text"].as_str().unwrap_or_default()
                ));
                self.ev(
                    &mut out,
                    "response.output_text.delta",
                    json!({ "item_id": item_id("msg", index),
                    "output_index": index, "content_index": content_index, "delta": text }),
                );
            }
            Delta::Refusal { text } => {
                let (index, content_index) = self.part(&mut out, "refusal");
                let part = self
                    .message
                    .as_mut()
                    .and_then(|m| m.parts.last_mut())
                    .expect("open");
                part["refusal"] = json!(format!(
                    "{}{text}",
                    part["refusal"].as_str().unwrap_or_default()
                ));
                self.ev(
                    &mut out,
                    "response.refusal.delta",
                    json!({ "item_id": item_id("msg", index),
                    "output_index": index, "content_index": content_index, "delta": text }),
                );
            }
            Delta::Citation { citation } => {
                let (index, content_index) = self.part(&mut out, "output_text");
                let annotation = annotation(citation);
                let part = self
                    .message
                    .as_mut()
                    .and_then(|m| m.parts.last_mut())
                    .expect("open");
                let list = part["annotations"]
                    .as_array_mut()
                    .expect("text parts have annotations");
                list.push(annotation.clone());
                let annotation_index = list.len() - 1;
                self.ev(
                    &mut out,
                    "response.output_text.annotation.added",
                    json!({
                    "item_id": item_id("msg", index), "output_index": index,
                    "content_index": content_index, "annotation_index": annotation_index,
                    "annotation": annotation }),
                );
            }
            Delta::Reasoning { text } => {
                let index = self.reasoning_item(&mut out);
                if let Some(r) = self.reasoning.as_mut() {
                    r.text.push_str(text);
                }
                self.ev(
                    &mut out,
                    "response.reasoning_summary_text.delta",
                    json!({
                    "item_id": item_id("rs", index), "output_index": index,
                    "summary_index": 0, "delta": text }),
                );
            }
            Delta::ReasoningSignature { signature } => {
                self.reasoning_item(&mut out);
                let encrypted = parse_signature(signature)
                    .and_then(|item| item["encrypted_content"].as_str().map(str::to_string));
                if let Some(r) = self.reasoning.as_mut() {
                    r.encrypted = encrypted;
                }
            }
            Delta::ToolCallStart { index, id, name } => {
                if self.calls.contains_key(index) {
                    return Err(ProtocolError::Invalid(format!(
                        "tool call {index} started twice"
                    )));
                }
                self.close_message(&mut out);
                self.close_reasoning(&mut out);
                let output_index = self.next;
                self.next += 1;
                let call_id = id.clone().unwrap_or_else(|| format!("call_{index}"));
                let item = json!({ "type": "function_call", "id": item_id("fc", output_index),
                    "call_id": call_id, "name": name, "arguments": "", "status": "in_progress" });
                self.calls.insert(
                    *index,
                    Call {
                        index: output_index,
                        call_id,
                        name: name.clone(),
                        args: String::new(),
                    },
                );
                self.ev(
                    &mut out,
                    "response.output_item.added",
                    json!({ "output_index": output_index, "item": item }),
                );
            }
            Delta::ToolCallArgs { index, fragment } => {
                let call = self.calls.get_mut(index).ok_or_else(|| {
                    ProtocolError::Invalid(format!(
                        "arguments for tool call {index} before its start"
                    ))
                })?;
                call.args.push_str(fragment);
                let output_index = call.index;
                self.ev(&mut out, "response.function_call_arguments.delta", json!({
                    "item_id": item_id("fc", output_index), "output_index": output_index, "delta": fragment }));
            }
            Delta::Usage { usage } => {
                let u = &mut self.usage;
                u.input_tokens = usage.input_tokens.or(u.input_tokens);
                u.output_tokens = usage.output_tokens.or(u.output_tokens);
                u.cached_input_tokens = usage.cached_input_tokens.or(u.cached_input_tokens);
                u.reasoning_tokens = usage.reasoning_tokens.or(u.reasoning_tokens);
            }
            Delta::Stop { reason } => {
                self.close_message(&mut out);
                self.close_reasoning(&mut out);
                self.close_calls(&mut out);
                self.stop = Some(reason.clone());
            }
            Delta::Error { error } => {
                self.done = true;
                let code = error
                    .raw_type
                    .clone()
                    .unwrap_or_else(|| error_type(error.kind).to_string());
                self.ev(
                    &mut out,
                    "error",
                    json!({ "code": code, "message": error.message, "param": null }),
                );
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> String {
        let mut out = String::new();
        if self.done {
            return out;
        }
        self.start(&mut out);
        self.close_message(&mut out);
        self.close_reasoning(&mut out);
        self.close_calls(&mut out);
        self.done = true;
        // The format must end with a status. A stream that ended before the
        // model stopped is reported as incomplete, reason "interrupted".
        let complete = self.stop.is_some() && incomplete_reason(self.stop.as_ref()).is_none();
        let (kind, status) = if complete {
            ("response.completed", "completed")
        } else {
            ("response.incomplete", "incomplete")
        };
        let response = self.response(status);
        self.ev(&mut out, kind, json!({ "response": response }));
        out
    }

    fn keep_alive(&self) -> String {
        ": keep-alive\n\n".to_string()
    }
}

const INTERRUPTED: &str = "interrupted";

fn incomplete_reason(stop: Option<&StopReason>) -> Option<&'static str> {
    match stop {
        Some(StopReason::MaxTokens) => Some("max_output_tokens"),
        Some(StopReason::ContentFilter) => Some("content_filter"),
        _ => None,
    }
}

fn annotation(c: &Citation) -> Value {
    let native = |raw: &&Value| raw["type"] == "url_citation" && raw.get("url").is_some();
    match c.raw.as_ref().filter(native) {
        Some(raw) => raw.clone(),
        None => json!({ "type": "url_citation", "url": c.url, "title": c.title,
            "start_index": 0, "end_index": 0 }),
    }
}

fn wire_usage(u: &Usage) -> WireUsage {
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

fn error_type(kind: ErrorKind) -> &'static str {
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
