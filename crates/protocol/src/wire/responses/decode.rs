// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Responses-format replies → hub: stream events, whole responses, errors.

use std::collections::BTreeMap;

use serde_json::Value;

use super::emit::signature_of;
use super::types::{ErrorBody, ErrorResponse, Event, Response, WireUsage};
use crate::conversation::{Block, Citation, Role, StopReason, Turn, Usage};
use crate::decode::{Decoder, Delta};
use crate::diag::{Diag, DiagKind, ErrorKind, HubError, ProtocolError};
use crate::limits::Limits;
use crate::parse::Reply;
use crate::sse::SseFrame;

pub(super) fn parse_event(frame: &SseFrame, limits: &Limits) -> Result<Event, ProtocolError> {
    limits.check_text_depth(&frame.data)?;
    let data: Value = serde_json::from_str(&frame.data)?;
    let kind = data["type"]
        .as_str()
        .or(frame.event.as_deref())
        .unwrap_or_default()
        .to_string();
    Ok(Event { kind, data })
}

/// Holds which output items are function calls, bounded by the limits.
pub struct ResponsesDecoder {
    limits: Limits,
    tools: BTreeMap<usize, usize>,
    saw_refusal: bool,
    done: bool,
}

impl ResponsesDecoder {
    pub(super) fn new(limits: Limits) -> Self {
        Self {
            limits,
            tools: BTreeMap::new(),
            saw_refusal: false,
            done: false,
        }
    }

    fn tool(&self, data: &Value) -> Result<usize, ProtocolError> {
        let output = data["output_index"].as_u64().unwrap_or(u64::MAX) as usize;
        self.tools.get(&output).copied().ok_or_else(|| {
            ProtocolError::Invalid(format!("arguments for output {output}, not a call"))
        })
    }

    fn finished(&mut self, response: &Value) -> Vec<Delta> {
        self.done = true;
        let mut out = Vec::new();
        if let Some(usage) = usage_of(&response["usage"]) {
            out.push(Delta::Usage { usage });
        }
        out.push(Delta::Stop {
            reason: stop_of(response, !self.tools.is_empty(), self.saw_refusal),
        });
        out
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn notice(what: String) -> Delta {
    Delta::Notice {
        diag: Diag::new(DiagKind::Unrecognized, "stream", what),
    }
}

impl Decoder for ResponsesDecoder {
    type Event = Event;

    fn decode(&mut self, event: Event) -> Result<Vec<Delta>, ProtocolError> {
        let data = &event.data;
        if self.done {
            return Err(ProtocolError::AfterEnd);
        }
        Ok(match event.kind.as_str() {
            "response.created" => vec![Delta::Start {
                id: data["response"]["id"].as_str().map(str::to_string),
                model: data["response"]["model"].as_str().map(str::to_string),
            }],
            "response.output_item.added" => {
                let item = &data["item"];
                if item["type"] != "function_call" {
                    return Ok(Vec::new());
                }
                if self.tools.len() >= self.limits.max_stream_tool_calls {
                    return Err(ProtocolError::Limit {
                        what: "tool calls in a stream",
                        limit: self.limits.max_stream_tool_calls,
                    });
                }
                let index = self.tools.len();
                let output = data["output_index"].as_u64().unwrap_or(u64::MAX) as usize;
                self.tools.insert(output, index);
                let mut out = vec![Delta::ToolCallStart {
                    index,
                    id: item["call_id"].as_str().map(str::to_string),
                    name: s(&item["name"]),
                }];
                let args = s(&item["arguments"]);
                if !args.is_empty() {
                    out.push(Delta::ToolCallArgs {
                        index,
                        fragment: args,
                    });
                }
                out
            }
            "response.output_item.done" => {
                let item = &data["item"];
                match (item["type"].as_str(), signature_of(item)) {
                    (Some("reasoning"), Some(signature)) => {
                        vec![Delta::ReasoningSignature { signature }]
                    }
                    _ => Vec::new(),
                }
            }
            "response.output_text.delta" => vec![Delta::Text {
                text: s(&data["delta"]),
            }],
            "response.refusal.delta" => {
                self.saw_refusal = true;
                vec![Delta::Refusal {
                    text: s(&data["delta"]),
                }]
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                vec![Delta::Reasoning {
                    text: s(&data["delta"]),
                }]
            }
            "response.function_call_arguments.delta" => vec![Delta::ToolCallArgs {
                index: self.tool(data)?,
                fragment: s(&data["delta"]),
            }],
            "response.output_text.annotation.added" => vec![Delta::Citation {
                citation: citation_of(&data["annotation"]),
            }],
            "response.completed" | "response.incomplete" => self.finished(&data["response"]),
            "response.failed" => {
                self.done = true;
                let error = &data["response"]["error"];
                vec![Delta::Error {
                    error: hub_error(&error_body(error), None),
                }]
            }
            "error" => {
                self.done = true;
                vec![Delta::Error {
                    error: hub_error(&error_body(data), None),
                }]
            }
            // Progress and "done" events repeat what the deltas carried.
            "response.in_progress"
            | "response.queued"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done" => Vec::new(),
            other => vec![notice(format!("event type {other:?}"))],
        })
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

/// An error object: `code` is the format's kind, `type` the older name.
fn error_body(v: &Value) -> ErrorBody {
    let kind = v["type"]
        .as_str()
        .filter(|t| *t != "error")
        .or(v["code"].as_str());
    ErrorBody {
        message: s(&v["message"]),
        kind: kind.map(str::to_string),
        code: None,
    }
}

pub(crate) fn usage_of(v: &Value) -> Option<Usage> {
    if v.is_null() {
        return None;
    }
    serde_json::from_value(v.clone()).ok().map(hub_usage)
}

fn hub_usage(u: WireUsage) -> Usage {
    Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cached_input_tokens: u.input_tokens_details.and_then(|d| d.cached_tokens),
        reasoning_tokens: u.output_tokens_details.and_then(|d| d.reasoning_tokens),
    }
}

/// Why a reply stopped, from its status and what it held.
fn stop_of(response: &Value, called: bool, refused: bool) -> StopReason {
    match response["status"].as_str() {
        Some("incomplete") => match response["incomplete_details"]["reason"].as_str() {
            Some("max_output_tokens") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::ContentFilter,
            other => StopReason::Other(other.unwrap_or("incomplete").to_string()),
        },
        _ if refused => StopReason::Refusal,
        _ if called => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    }
}

pub(crate) fn citation_of(raw: &Value) -> Citation {
    Citation {
        url: raw["url"].as_str().map(str::to_string),
        title: raw["title"].as_str().map(str::to_string),
        quote: None,
        raw: Some(raw.clone()),
    }
}

pub(super) fn decode_response(resp: Response) -> Result<Reply, ProtocolError> {
    let mut diags = Vec::new();
    let mut blocks = Vec::new();
    let (mut called, mut refused) = (false, false);
    for (i, item) in resp.output.iter().enumerate() {
        let at = format!("output[{i}]");
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("output_text") => {
                            blocks.push(Block::Text {
                                text: s(&part["text"]),
                            });
                            for a in part["annotations"].as_array().into_iter().flatten() {
                                blocks.push(Block::Citation(citation_of(a)));
                            }
                        }
                        Some("refusal") => {
                            refused = true;
                            blocks.push(Block::Text {
                                text: s(&part["refusal"]),
                            });
                        }
                        _ => diags.push(Diag::new(
                            DiagKind::Unrecognized,
                            &at,
                            "message part kept out",
                        )),
                    }
                }
            }
            Some("reasoning") => {
                let text: String = item["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(item["content"].as_array().into_iter().flatten())
                    .filter_map(|p| p["text"].as_str())
                    .collect();
                blocks.push(Block::Reasoning {
                    text,
                    signature: signature_of(item),
                    redacted: None,
                });
            }
            Some("function_call") => {
                called = true;
                let arguments = s(&item["arguments"]);
                let input = serde_json::from_str(&arguments).unwrap_or_else(|_| {
                    diags.push(Diag::new(
                        DiagKind::Approximated,
                        &at,
                        "arguments are not valid JSON; kept as a string",
                    ));
                    Value::String(arguments.clone())
                });
                blocks.push(Block::ToolUse {
                    id: s(&item["call_id"]),
                    name: s(&item["name"]),
                    input,
                });
            }
            _ => diags.push(Diag::new(
                DiagKind::Unrecognized,
                at,
                "output item kept out",
            )),
        }
    }
    let value = serde_json::to_value(&resp)?;
    Ok(Reply {
        id: resp.id,
        model: resp.model,
        turn: Turn {
            id: None,
            role: Role::Assistant,
            blocks,
        },
        stop: resp
            .status
            .is_some()
            .then(|| stop_of(&value, called, refused)),
        usage: resp.usage.map(hub_usage),
        diags,
    })
}

pub(crate) fn hub_error(body: &ErrorBody, status: Option<u16>) -> HubError {
    let kind = match body.kind.as_deref().unwrap_or_default() {
        "invalid_request_error" | "invalid_prompt" => ErrorKind::InvalidRequest,
        "authentication_error" => ErrorKind::Authentication,
        "permission_error" => ErrorKind::PermissionDenied,
        "not_found_error" => ErrorKind::NotFound,
        "rate_limit_error" | "rate_limit_exceeded" => ErrorKind::RateLimited,
        "unavailable_error" | "server_is_overloaded" => ErrorKind::Overloaded,
        "server_error" => ErrorKind::Server,
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
