//! Hub → responses format: the event stream.
//!
//! The format's closing events repeat each item in full, and the final event
//! carries the whole response, so this encoder keeps the reply's items. That
//! is the format's design; decoders never keep a reply.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::emit::parse_signature;
use super::reply::{annotation, error_type, incomplete_reason, item_id, wire_usage, INTERRUPTED};
use crate::conversation::{StopReason, Usage};
use crate::decode::Delta;
use crate::diag::ProtocolError;
use crate::encode::{frame, EncodeOptions, Encoder};

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
