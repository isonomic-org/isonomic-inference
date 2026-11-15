//! Messages-format replies → hub: stream events, whole responses, errors.

use std::collections::BTreeMap;

use serde_json::Value;

use super::types::{ContentBlock, ErrorBody, ErrorResponse, Event, Response, WireUsage};
use crate::conversation::{Block, Citation, Role, StopReason, Turn, Usage};
use crate::decode::{Decoder, Delta};
use crate::diag::{Diag, DiagKind, ErrorKind, HubError, ProtocolError};
use crate::limits::Limits;
use crate::parse::Reply;
use crate::sse::SseFrame;

pub(super) fn parse_event(frame: &SseFrame, limits: &Limits) -> Result<Event, ProtocolError> {
    limits.check_text_depth(&frame.data)?;
    let v: Value = serde_json::from_str(&frame.data)?;
    let kind = v["type"].as_str().unwrap_or_default();
    let index = || {
        v["index"]
            .as_u64()
            .map(|i| i as usize)
            .ok_or_else(|| ProtocolError::Invalid(format!("{kind} without an index")))
    };
    Ok(match kind {
        "message_start" => Event::MessageStart {
            id: v["message"]["id"].as_str().map(str::to_string),
            model: v["message"]["model"].as_str().map(str::to_string),
            usage: usage_of(&v["message"]["usage"]),
        },
        "content_block_start" => Event::BlockStart {
            index: index()?,
            block: v["content_block"].clone(),
        },
        "content_block_delta" => Event::BlockDelta {
            index: index()?,
            delta: v["delta"].clone(),
        },
        "content_block_stop" => Event::BlockStop { index: index()? },
        "message_delta" => Event::MessageDelta {
            stop_reason: v["delta"]["stop_reason"].as_str().map(str::to_string),
            usage: usage_of(&v["usage"]),
        },
        "message_stop" => Event::MessageStop,
        "ping" => Event::Ping,
        "error" => Event::Error(serde_json::from_value(v["error"].clone()).unwrap_or_default()),
        other => Event::Unknown(other.to_string()),
    })
}

fn usage_of(v: &Value) -> Option<WireUsage> {
    if v.is_null() {
        return None;
    }
    serde_json::from_value(v.clone()).ok()
}

#[derive(Debug, Clone, Copy)]
enum Open {
    Text,
    Thinking,
    Tool(usize),
    Other,
}

/// Holds only the open content blocks, so memory is bounded by the limits.
pub struct MessagesDecoder {
    limits: Limits,
    open: BTreeMap<usize, Open>,
    tool_calls: usize,
    done: bool,
}

impl MessagesDecoder {
    pub(super) fn new(limits: Limits) -> Self {
        Self {
            limits,
            open: BTreeMap::new(),
            tool_calls: 0,
            done: false,
        }
    }

    fn start_block(&mut self, index: usize, block: &Value) -> Result<Vec<Delta>, ProtocolError> {
        if self.open.len() >= self.limits.max_blocks_per_turn {
            return Err(ProtocolError::Limit {
                what: "open stream blocks",
                limit: self.limits.max_blocks_per_turn,
            });
        }
        let text = |key: &str| block[key].as_str().unwrap_or_default().to_string();
        let (open, deltas) = match block["type"].as_str().unwrap_or_default() {
            "text" => (
                Open::Text,
                non_empty(text("text"), |text| Delta::Text { text }),
            ),
            "thinking" => (
                Open::Thinking,
                non_empty(text("thinking"), |text| Delta::Reasoning { text }),
            ),
            "redacted_thinking" => (
                Open::Other,
                vec![Delta::ReasoningRedacted { data: text("data") }],
            ),
            "tool_use" => {
                if self.tool_calls >= self.limits.max_stream_tool_calls {
                    return Err(ProtocolError::Limit {
                        what: "tool calls in a stream",
                        limit: self.limits.max_stream_tool_calls,
                    });
                }
                let ordinal = self.tool_calls;
                self.tool_calls += 1;
                let mut deltas = vec![Delta::ToolCallStart {
                    index: ordinal,
                    id: block["id"].as_str().map(str::to_string),
                    name: text("name"),
                }];
                // Servers usually start with an empty input and stream it.
                match &block["input"] {
                    Value::Object(map) if map.is_empty() => {}
                    Value::Null => {}
                    input => deltas.push(Delta::ToolCallArgs {
                        index: ordinal,
                        fragment: input.to_string(),
                    }),
                }
                (Open::Tool(ordinal), deltas)
            }
            other => (Open::Other, vec![notice(format!("block type {other:?}"))]),
        };
        self.open.insert(index, open);
        Ok(deltas)
    }

    fn block_delta(&mut self, index: usize, delta: &Value) -> Result<Vec<Delta>, ProtocolError> {
        let open = *self
            .open
            .get(&index)
            .ok_or_else(|| ProtocolError::Invalid(format!("delta for block {index} not open")))?;
        let s = |key: &str| delta[key].as_str().unwrap_or_default().to_string();
        Ok(vec![
            match (delta["type"].as_str().unwrap_or_default(), open) {
                ("text_delta", _) => Delta::Text { text: s("text") },
                ("thinking_delta", _) => Delta::Reasoning {
                    text: s("thinking"),
                },
                ("signature_delta", _) => Delta::ReasoningSignature {
                    signature: s("signature"),
                },
                ("input_json_delta", Open::Tool(ordinal)) => Delta::ToolCallArgs {
                    index: ordinal,
                    fragment: s("partial_json"),
                },
                ("citations_delta", _) => Delta::Citation {
                    citation: citation_of(&delta["citation"]),
                },
                (other, _) => notice(format!("delta type {other:?}")),
            },
        ])
    }
}

fn non_empty(text: String, make: impl FnOnce(String) -> Delta) -> Vec<Delta> {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![make(text)]
    }
}

fn notice(what: String) -> Delta {
    Delta::Notice {
        diag: Diag::new(DiagKind::Unrecognized, "stream", what),
    }
}

impl Decoder for MessagesDecoder {
    type Event = Event;

    fn decode(&mut self, event: Event) -> Result<Vec<Delta>, ProtocolError> {
        if self.done {
            return match event {
                Event::MessageStop | Event::Ping => Ok(Vec::new()),
                _ => Err(ProtocolError::AfterEnd),
            };
        }
        Ok(match event {
            Event::MessageStart { id, model, usage } => {
                let mut deltas = vec![Delta::Start { id, model }];
                deltas.extend(usage.map(|u| Delta::Usage {
                    usage: hub_usage(u),
                }));
                deltas
            }
            Event::BlockStart { index, block } => self.start_block(index, &block)?,
            Event::BlockDelta { index, delta } => self.block_delta(index, &delta)?,
            Event::BlockStop { index } => {
                self.open.remove(&index);
                Vec::new()
            }
            Event::MessageDelta { stop_reason, usage } => {
                let mut deltas: Vec<Delta> = usage
                    .map(|u| Delta::Usage {
                        usage: hub_usage(u),
                    })
                    .into_iter()
                    .collect();
                deltas.extend(stop_reason.map(|r| Delta::Stop { reason: stop(&r) }));
                deltas
            }
            Event::MessageStop => {
                self.done = true;
                Vec::new()
            }
            Event::Ping => Vec::new(),
            Event::Error(body) => {
                self.done = true;
                vec![Delta::Error {
                    error: hub_error(&body, None),
                }]
            }
            Event::Unknown(kind) => vec![notice(format!("event type {kind:?}"))],
        })
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

pub(crate) fn stop(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}

fn hub_usage(u: WireUsage) -> Usage {
    Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cached_input_tokens: u.cache_read_input_tokens,
        reasoning_tokens: None,
    }
}

pub(super) fn citation_of(raw: &Value) -> Citation {
    let s = |key: &str| raw[key].as_str().map(str::to_string);
    Citation {
        url: s("url"),
        title: s("title").or_else(|| s("document_title")),
        quote: s("cited_text"),
        raw: Some(raw.clone()),
    }
}

pub(super) fn decode_response(resp: Response) -> Result<Reply, ProtocolError> {
    let mut diags = Vec::new();
    let mut blocks = Vec::with_capacity(resp.content.len());
    for (i, value) in resp.content.into_iter().enumerate() {
        let at = format!("content[{i}]");
        match serde_json::from_value::<ContentBlock>(value) {
            Ok(ContentBlock::Text { text, citations }) => {
                blocks.push(Block::Text { text });
                blocks.extend(citations.iter().map(|c| Block::Citation(citation_of(c))));
            }
            Ok(ContentBlock::Thinking {
                thinking,
                signature,
            }) => blocks.push(Block::Reasoning {
                text: thinking,
                signature: Some(signature),
                redacted: None,
            }),
            Ok(ContentBlock::RedactedThinking { data }) => blocks.push(Block::Reasoning {
                text: String::new(),
                signature: None,
                redacted: Some(data),
            }),
            Ok(ContentBlock::ToolUse { id, name, input }) => {
                blocks.push(Block::ToolUse { id, name, input });
            }
            _ => diags.push(Diag::new(
                DiagKind::Unrecognized,
                at,
                "reply block kept out",
            )),
        }
    }
    Ok(Reply {
        id: resp.id,
        model: resp.model,
        turn: Turn {
            id: None,
            role: Role::Assistant,
            blocks,
        },
        stop: resp.stop_reason.as_deref().map(stop),
        usage: resp.usage.map(hub_usage),
        diags,
    })
}

fn hub_error(body: &ErrorBody, status: Option<u16>) -> HubError {
    let kind = match body.kind.as_str() {
        "invalid_request_error" => ErrorKind::InvalidRequest,
        "authentication_error" => ErrorKind::Authentication,
        "permission_error" => ErrorKind::PermissionDenied,
        "not_found_error" => ErrorKind::NotFound,
        "request_too_large" => ErrorKind::RequestTooLarge,
        "rate_limit_error" => ErrorKind::RateLimited,
        "overloaded_error" => ErrorKind::Overloaded,
        "timeout_error" => ErrorKind::Timeout,
        "api_error" => ErrorKind::Server,
        _ => status.map_or(ErrorKind::Other, ErrorKind::from_status),
    };
    HubError {
        kind,
        message: body.message.clone(),
        raw_type: (!body.kind.is_empty()).then(|| body.kind.clone()),
    }
}

pub(super) fn decode_error(status: Option<u16>, body: &[u8]) -> HubError {
    match serde_json::from_slice::<ErrorResponse>(body) {
        Ok(resp) => hub_error(&resp.error, status),
        Err(_) => crate::wire::unparsed_error(status, body),
    }
}
