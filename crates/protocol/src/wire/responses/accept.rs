// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Responses-format request → conversation.

use serde_json::Value;

use super::decode::citation_of;
use super::emit::signature_of;
use super::types::{Input, Request};
use super::NAME;
use crate::accept::{parse_data_url, Accepted, Inliner};
use crate::conversation::{
    Block, Conversation, Effort, ReasoningConfig, ResponseFormat, Role, Sampling,
    ToolChoice as Choice, ToolSpec, Turn,
};
use crate::diag::{Diag, DiagKind, ProtocolError};
use crate::limits::Limits;

fn dropped(at: impl Into<String>, detail: impl Into<String>) -> Diag {
    Diag::new(DiagKind::Dropped, at, detail)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Gathers items into turns: user-side items (user messages, tool output)
/// and assistant-side items (assistant messages, calls, reasoning).
struct Turns {
    done: Vec<Turn>,
    open: Option<Turn>,
}

impl Turns {
    fn blocks(&mut self, role: Role, fresh: bool) -> &mut Vec<Block> {
        let switch = self.open.as_ref().is_some_and(|t| t.role != role || fresh);
        if switch {
            self.done.extend(self.open.take());
        }
        &mut self
            .open
            .get_or_insert_with(|| Turn {
                id: None,
                role,
                blocks: Vec::new(),
            })
            .blocks
    }

    /// Tool output after the user's own content starts a new turn.
    fn user_has_content(&self) -> bool {
        self.open.as_ref().is_some_and(|t| {
            t.role == Role::User
                && t.blocks
                    .iter()
                    .any(|b| !matches!(b, Block::ToolResult { .. }))
        })
    }
}

pub(super) fn accept(req: Request, limits: &Limits) -> Result<Accepted, ProtocolError> {
    let mut diags = Vec::new();
    let mut media = Inliner::new(limits);
    let mut conv = Conversation {
        model: (!req.model.is_empty()).then_some(req.model),
        system: req.instructions.into_iter().collect(),
        sampling: Sampling {
            max_output_tokens: req.max_output_tokens,
            temperature: req.temperature,
            top_p: req.top_p,
            stop: Vec::new(),
        },
        parallel_tool_calls: req.parallel_tool_calls,
        metadata: req.metadata,
        ..Conversation::default()
    };

    let mut turns = Turns {
        done: Vec::new(),
        open: None,
    };
    let items = match req.input {
        Input::Text(text) => vec![serde_json::json!({ "role": "user", "content": text })],
        Input::Items(items) => items,
    };
    for (i, item) in items.iter().enumerate() {
        let at = format!("input[{i}]");
        if item.get("id").is_some() && item["type"] != "reasoning" {
            diags.push(dropped(
                format!("{at}.id"),
                "item IDs are the server's; not kept",
            ));
        }
        let kind = item["type"].as_str().unwrap_or("message");
        match kind {
            "message" => match item["role"].as_str().unwrap_or_default() {
                "system" | "developer" => {
                    if !turns.done.is_empty() || turns.open.is_some() {
                        diags.push(Diag::new(
                            DiagKind::Relocated,
                            &at,
                            "system text after the first turn joins the system instructions",
                        ));
                    }
                    conv.system.push(text_of(&item["content"], &at, &mut diags));
                }
                "user" => {
                    let fresh = turns.user_has_content();
                    let mut parts = Vec::new();
                    match &item["content"] {
                        Value::String(text) => parts.push(Block::Text { text: text.clone() }),
                        content => {
                            for (p, part) in content.as_array().into_iter().flatten().enumerate() {
                                let part_at = format!("{at}.content[{p}]");
                                parts.extend(input_part(part, &part_at, &mut media, &mut diags)?);
                            }
                        }
                    }
                    turns.blocks(Role::User, fresh).extend(parts);
                }
                "assistant" => {
                    let blocks = turns.blocks(Role::Assistant, false);
                    match &item["content"] {
                        Value::String(text) => blocks.push(Block::Text { text: text.clone() }),
                        content => {
                            for part in content.as_array().into_iter().flatten() {
                                match part["type"].as_str() {
                                    Some("output_text") | Some("input_text") => {
                                        blocks.push(Block::Text {
                                            text: s(&part["text"]),
                                        });
                                        for a in
                                            part["annotations"].as_array().into_iter().flatten()
                                        {
                                            blocks.push(Block::Citation(citation_of(a)));
                                        }
                                    }
                                    Some("refusal") => blocks.push(Block::Text {
                                        text: s(&part["refusal"]),
                                    }),
                                    _ => diags.push(dropped(&at, "assistant part kept out")),
                                }
                            }
                        }
                    }
                }
                other => {
                    return Err(ProtocolError::Invalid(format!(
                        "{at}: unknown role {other:?}"
                    )))
                }
            },
            "function_call" => {
                let arguments = s(&item["arguments"]);
                let input = serde_json::from_str(&arguments).unwrap_or_else(|_| {
                    diags.push(Diag::new(
                        DiagKind::Approximated,
                        &at,
                        "arguments are not valid JSON; kept as a string",
                    ));
                    Value::String(arguments.clone())
                });
                turns.blocks(Role::Assistant, false).push(Block::ToolUse {
                    id: s(&item["call_id"]),
                    name: s(&item["name"]),
                    input,
                });
            }
            "function_call_output" => {
                let fresh = turns.user_has_content();
                let content = match &item["output"] {
                    Value::String(text) => vec![Block::Text { text: text.clone() }],
                    other => {
                        let mut out = Vec::new();
                        for (p, part) in other.as_array().into_iter().flatten().enumerate() {
                            let part_at = format!("{at}.output[{p}]");
                            out.extend(input_part(part, &part_at, &mut media, &mut diags)?);
                        }
                        out
                    }
                };
                turns.blocks(Role::User, fresh).push(Block::ToolResult {
                    call_id: s(&item["call_id"]),
                    content,
                    is_error: false,
                });
            }
            "reasoning" => {
                let text: String = item["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["text"].as_str())
                    .collect();
                turns.blocks(Role::Assistant, false).push(Block::Reasoning {
                    text,
                    signature: signature_of(item),
                    redacted: None,
                });
            }
            other => diags.push(dropped(&at, format!("item type {other:?} not kept"))),
        }
    }
    conv.turns = turns.done;
    conv.turns.extend(turns.open);

    for (i, tool) in req.tools.into_iter().enumerate() {
        if tool.kind != "function" {
            diags.push(dropped(
                format!("tools[{i}]"),
                format!("tool type {:?} not kept", tool.kind),
            ));
            continue;
        }
        conv.tools.push(ToolSpec {
            name: tool.name,
            description: tool.description,
            input_schema: tool.parameters,
            strict: tool.strict,
        });
    }
    conv.tool_choice = match req.tool_choice {
        None => None,
        Some(Value::String(mode)) => match mode.as_str() {
            "auto" => Some(Choice::Auto),
            "none" => Some(Choice::None),
            "required" => Some(Choice::Required),
            other => return Err(ProtocolError::Invalid(format!("tool_choice {other:?}"))),
        },
        Some(choice) if choice["type"] == "function" && choice["name"].is_string() => {
            Some(Choice::Tool {
                name: s(&choice["name"]),
            })
        }
        Some(choice) => {
            conv.ext.insert(NAME, "tool_choice", choice);
            None
        }
    };
    if let Some(text) = req.text {
        if !text.extra.is_empty() {
            diags.push(dropped("text", "text settings other than the format"));
        }
        conv.response_format = text.format.map(|format| {
            let plain = format["type"] == "json_schema"
                && format["name"].is_string()
                && format["schema"].is_object()
                && format.as_object().is_some_and(|m| {
                    m.keys()
                        .all(|k| ["type", "name", "schema", "strict"].contains(&k.as_str()))
                });
            if plain {
                ResponseFormat::JsonSchema {
                    name: Some(s(&format["name"])),
                    schema: format["schema"].clone(),
                    strict: format["strict"].as_bool(),
                }
            } else {
                ResponseFormat::Opaque { value: format }
            }
        });
    }
    if let Some(reasoning) = req.reasoning {
        if !reasoning.extra.is_empty() {
            diags.push(dropped(
                "reasoning",
                "reasoning settings other than the effort",
            ));
        }
        if let Some(effort) = reasoning.effort {
            match Effort::from_chat(&effort) {
                Some(effort) => {
                    conv.reasoning = Some(ReasoningConfig {
                        effort: Some(effort),
                        budget_tokens: None,
                    })
                }
                None => diags.push(dropped(
                    "reasoning.effort",
                    format!("effort {effort:?} not known"),
                )),
            }
        }
    }
    for (key, value) in req.extra {
        conv.ext.insert(NAME, key, value);
    }
    limits.check_conversation(&conv)?;
    Ok(Accepted {
        conversation: conv,
        stream: req.stream == Some(true),
        stream_usage: req.stream == Some(true),
        blobs: media.blobs,
        diags,
    })
}

fn text_of(content: &Value, at: &str, diags: &mut Vec<Diag>) -> String {
    match content {
        Value::String(text) => text.clone(),
        parts => {
            let mut out = String::new();
            for part in parts.as_array().into_iter().flatten() {
                match part["text"].as_str() {
                    Some(text) => out.push_str(text),
                    None => diags.push(dropped(at, "only text parts are kept here")),
                }
            }
            out
        }
    }
}

fn input_part(
    part: &Value,
    at: &str,
    media: &mut Inliner<'_>,
    diags: &mut Vec<Diag>,
) -> Result<Option<Block>, ProtocolError> {
    Ok(Some(match part["type"].as_str() {
        Some("input_text") => Block::Text {
            text: s(&part["text"]),
        },
        Some("input_image") if part["image_url"].is_string() => Block::Image {
            source: media.url(&s(&part["image_url"]))?,
        },
        Some("input_file") if part["file_data"].is_string() || part["file_url"].is_string() => {
            let source = match part["file_data"].as_str() {
                Some(data) => match parse_data_url(data) {
                    Some((media_type, base64)) => media.inline(media_type, base64)?,
                    None => media.inline("application/octet-stream", data)?,
                },
                None => media.url(&s(&part["file_url"]))?,
            };
            Block::File {
                source,
                name: part["filename"].as_str().map(str::to_string),
            }
        }
        _ => {
            diags.push(dropped(at, "a part by file ID, or of a kind not kept"));
            return Ok(None);
        }
    }))
}
