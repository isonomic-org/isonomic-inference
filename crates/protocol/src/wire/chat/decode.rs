//! Chat-format replies → hub: stream chunks, whole responses, errors.

use std::collections::BTreeSet;

use serde_json::Value;

use super::types::{Chunk, ErrorBody, ErrorResponse, Event, Response, WireUsage};
use crate::conversation::{Block, Citation, Role, StopReason, Turn, Usage};
use crate::decode::{Decoder, Delta};
use crate::diag::{Diag, DiagKind, ErrorKind, HubError, ProtocolError};
use crate::limits::Limits;
use crate::parse::Reply;
use crate::sse::SseFrame;

pub(super) fn parse_event(frame: &SseFrame, limits: &Limits) -> Result<Event, ProtocolError> {
    let data = frame.data.trim();
    if data == "[DONE]" {
        return Ok(Event::Done);
    }
    limits.check_text_depth(data)?;
    let value: Value = serde_json::from_str(data)?;
    if let Some(error) = value.get("error") {
        return Ok(Event::Error(
            serde_json::from_value(error.clone()).unwrap_or_default(),
        ));
    }
    Ok(Event::Chunk(serde_json::from_value(value)?))
}

/// Holds which tool calls have started, bounded by the limits.
pub struct ChatDecoder {
    limits: Limits,
    started: bool,
    tools: BTreeSet<usize>,
    noted_other_choice: bool,
    done: bool,
}

impl ChatDecoder {
    pub(super) fn new(limits: Limits) -> Self {
        Self {
            limits,
            started: false,
            tools: BTreeSet::new(),
            noted_other_choice: false,
            done: false,
        }
    }

    fn chunk(&mut self, chunk: Chunk) -> Result<Vec<Delta>, ProtocolError> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(Delta::Start {
                id: chunk.id,
                model: chunk.model,
            });
        }
        for choice in chunk.choices {
            if choice.index != 0 {
                if !self.noted_other_choice {
                    self.noted_other_choice = true;
                    out.push(Delta::Notice {
                        diag: Diag::new(
                            DiagKind::Dropped,
                            "choices",
                            "only the first choice is decoded",
                        ),
                    });
                }
                continue;
            }
            let delta = choice.delta;
            if let Some(text) = delta.reasoning_content.or(delta.reasoning) {
                push_text(&mut out, text, |text| Delta::Reasoning { text });
            }
            if let Some(text) = delta.content {
                push_text(&mut out, text, |text| Delta::Text { text });
            }
            if let Some(text) = delta.refusal {
                push_text(&mut out, text, |text| Delta::Refusal { text });
            }
            for call in delta.tool_calls {
                if call.index >= self.limits.max_stream_tool_calls {
                    return Err(ProtocolError::Limit {
                        what: "tool calls in a stream",
                        limit: self.limits.max_stream_tool_calls,
                    });
                }
                let function = call.function.unwrap_or_default();
                if self.tools.insert(call.index) {
                    out.push(Delta::ToolCallStart {
                        index: call.index,
                        id: call.id,
                        name: function.name.unwrap_or_default(),
                    });
                }
                if let Some(fragment) = function.arguments.filter(|a| !a.is_empty()) {
                    out.push(Delta::ToolCallArgs {
                        index: call.index,
                        fragment,
                    });
                }
            }
            out.extend(delta.annotations.iter().map(|a| Delta::Citation {
                citation: citation_of(a),
            }));
            if let Some(reason) = choice.finish_reason {
                out.push(Delta::Stop {
                    reason: stop(&reason),
                });
            }
        }
        if let Some(usage) = chunk.usage {
            out.push(Delta::Usage {
                usage: hub_usage(usage),
            });
        }
        Ok(out)
    }
}

fn push_text(out: &mut Vec<Delta>, text: String, make: impl FnOnce(String) -> Delta) {
    if !text.is_empty() {
        out.push(make(text));
    }
}

impl Decoder for ChatDecoder {
    type Event = Event;

    fn decode(&mut self, event: Event) -> Result<Vec<Delta>, ProtocolError> {
        if self.done {
            return match event {
                Event::Done => Ok(Vec::new()),
                _ => Err(ProtocolError::AfterEnd),
            };
        }
        match event {
            Event::Chunk(chunk) => self.chunk(chunk),
            Event::Done => {
                self.done = true;
                Ok(Vec::new())
            }
            Event::Error(body) => {
                self.done = true;
                Ok(vec![Delta::Error {
                    error: hub_error(&body, None),
                }])
            }
        }
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

pub(crate) fn stop(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::ContentFilter,
        other => StopReason::Other(other.to_string()),
    }
}

fn hub_usage(u: WireUsage) -> Usage {
    Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cached_input_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
        reasoning_tokens: u.completion_tokens_details.and_then(|d| d.reasoning_tokens),
    }
}

fn citation_of(raw: &Value) -> Citation {
    let inner = &raw["url_citation"];
    let s = |key: &str| inner[key].as_str().map(str::to_string);
    Citation {
        url: s("url"),
        title: s("title"),
        quote: None,
        raw: Some(raw.clone()),
    }
}

pub(super) fn decode_response(resp: Response) -> Result<Reply, ProtocolError> {
    let mut diags = Vec::new();
    if resp.choices.len() > 1 {
        diags.push(Diag::new(
            DiagKind::Dropped,
            "choices",
            "only the first choice is decoded",
        ));
    }
    let mut blocks = Vec::new();
    let mut stop_reason = None;
    if let Some(choice) = resp.choices.into_iter().next() {
        let message = choice.message;
        if let Some(text) = message.reasoning_content.or(message.reasoning) {
            blocks.push(Block::Reasoning {
                text,
                signature: None,
                redacted: None,
            });
        }
        if let Some(text) = message.content {
            blocks.push(Block::Text { text });
            blocks.extend(
                message
                    .annotations
                    .iter()
                    .map(|a| Block::Citation(citation_of(a))),
            );
        }
        if let Some(text) = message.refusal {
            diags.push(Diag::new(
                DiagKind::Approximated,
                "message.refusal",
                "refusal kept as text; the stop reason says refusal",
            ));
            blocks.push(Block::Text { text });
            stop_reason = Some(StopReason::Refusal);
        }
        for (i, call) in message.tool_calls.into_iter().enumerate() {
            let input = serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| {
                diags.push(Diag::new(
                    DiagKind::Approximated,
                    format!("message.tool_calls[{i}]"),
                    "arguments are not valid JSON; kept as a string",
                ));
                Value::String(call.function.arguments.clone())
            });
            blocks.push(Block::ToolUse {
                id: call.id,
                name: call.function.name,
                input,
            });
        }
        stop_reason = stop_reason.or_else(|| choice.finish_reason.as_deref().map(stop));
    }
    Ok(Reply {
        id: resp.id,
        model: resp.model,
        turn: Turn {
            id: None,
            role: Role::Assistant,
            blocks,
        },
        stop: stop_reason,
        usage: resp.usage.map(hub_usage),
        diags,
    })
}

fn hub_error(body: &ErrorBody, status: Option<u16>) -> HubError {
    let code = body
        .code
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = match (body.kind.as_deref().unwrap_or_default(), code) {
        (_, "rate_limit_exceeded") | ("rate_limit_error", _) => ErrorKind::RateLimited,
        ("invalid_request_error" | "exceed_context_size_error", _) => ErrorKind::InvalidRequest,
        ("authentication_error", _) => ErrorKind::Authentication,
        ("permission_error", _) => ErrorKind::PermissionDenied,
        ("not_found_error", _) => ErrorKind::NotFound,
        ("unavailable_error", _) => ErrorKind::Overloaded,
        ("server_error", _) => ErrorKind::Server,
        _ => status.map_or(ErrorKind::Other, ErrorKind::from_status),
    };
    HubError {
        kind,
        message: body.message.clone(),
        raw_type: body.kind.clone(),
    }
}

pub(super) fn decode_error(status: Option<u16>, body: &[u8]) -> HubError {
    match serde_json::from_slice::<ErrorResponse>(body) {
        Ok(resp) => hub_error(&resp.error, status),
        Err(_) => crate::wire::unparsed_error(status, body),
    }
}
