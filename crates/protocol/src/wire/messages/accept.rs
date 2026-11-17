// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Messages-format request → conversation.

use serde_json::{json, Value};

use super::types::{Content, ContentBlock, Request, Source, System, Thinking, ToolChoice};
use super::NAME;
use crate::accept::{Accepted, Inliner};
use crate::conversation::{
    Block, Conversation, Effort, MediaRef, ReasoningConfig, ResponseFormat, Role, Sampling,
    ToolChoice as Choice, ToolSpec, Turn,
};
use crate::diag::{Diag, DiagKind, ProtocolError};
use crate::limits::Limits;

use super::decode::citation_of;

pub(super) fn accept(req: Request, limits: &Limits) -> Result<Accepted, ProtocolError> {
    let mut diags = Vec::new();
    let mut media = Inliner::new(limits);
    let mut conv = Conversation {
        model: (!req.model.is_empty()).then_some(req.model),
        sampling: Sampling {
            max_output_tokens: Some(req.max_tokens),
            temperature: req.temperature,
            top_p: req.top_p,
            stop: req.stop_sequences,
        },
        ..Conversation::default()
    };

    match req.system {
        None => {}
        Some(System::Text(text)) => conv.system.push(text),
        Some(System::Blocks(blocks)) => {
            for (i, block) in blocks.into_iter().enumerate() {
                match block {
                    ContentBlock::Text { text, citations } => {
                        if !citations.is_empty() {
                            diags.push(dropped(format!("system[{i}]"), "citations on system text"));
                        }
                        conv.system.push(text);
                    }
                    _ => diags.push(dropped(
                        format!("system[{i}]"),
                        "system instructions are text",
                    )),
                }
            }
        }
    }

    for (t, message) in req.messages.into_iter().enumerate() {
        let role = match message.role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            other => {
                return Err(ProtocolError::Invalid(format!(
                    "messages[{t}]: unknown role {other:?}"
                )))
            }
        };
        let blocks = match message.content {
            Content::Text(text) => vec![Block::Text { text }],
            Content::Blocks(blocks) => {
                let mut out = Vec::with_capacity(blocks.len());
                for block in blocks {
                    hub_block(block, &mut out, &mut media)?;
                }
                out
            }
        };
        conv.turns.push(Turn {
            id: None,
            role,
            blocks,
        });
    }

    conv.tools = req
        .tools
        .into_iter()
        .map(|tool| ToolSpec {
            name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
            strict: tool.strict,
        })
        .collect();
    let disable = match req.tool_choice {
        None => None,
        Some(ToolChoice::Auto {
            disable_parallel_tool_use,
        }) => {
            conv.tool_choice = Some(Choice::Auto);
            disable_parallel_tool_use
        }
        Some(ToolChoice::Any {
            disable_parallel_tool_use,
        }) => {
            conv.tool_choice = Some(Choice::Required);
            disable_parallel_tool_use
        }
        Some(ToolChoice::Tool {
            name,
            disable_parallel_tool_use,
        }) => {
            conv.tool_choice = Some(Choice::Tool { name });
            disable_parallel_tool_use
        }
        Some(ToolChoice::None) => {
            conv.tool_choice = Some(Choice::None);
            None
        }
    };
    if disable == Some(true) {
        conv.parallel_tool_calls = Some(false);
    }

    let output = req.output_config.unwrap_or_default();
    let effort = match output.effort.as_deref() {
        None => None,
        Some("low") => Some(Effort::Low),
        Some("medium") => Some(Effort::Medium),
        Some("high") => Some(Effort::High),
        Some("max") => Some(Effort::Max),
        Some(other) => {
            diags.push(dropped(
                "output_config.effort",
                format!("effort {other:?} not known"),
            ));
            None
        }
    };
    match req.thinking {
        None => {
            conv.reasoning = effort.map(|e| ReasoningConfig {
                effort: Some(e),
                budget_tokens: None,
            });
            if effort.is_some() {
                diags.push(Diag::new(
                    DiagKind::Approximated,
                    "output_config.effort",
                    "an effort without thinking is sent back with adaptive thinking",
                ));
            }
        }
        Some(Thinking::Disabled) => {
            conv.reasoning = Some(ReasoningConfig {
                effort: Some(Effort::None),
                budget_tokens: None,
            });
        }
        Some(Thinking::Enabled { budget_tokens }) => {
            conv.reasoning = Some(ReasoningConfig {
                effort,
                budget_tokens: Some(budget_tokens),
            });
        }
        Some(Thinking::Adaptive) => match effort {
            Some(e) => {
                conv.reasoning = Some(ReasoningConfig {
                    effort: Some(e),
                    budget_tokens: None,
                })
            }
            None => conv
                .ext
                .insert(NAME, "thinking", json!({ "type": "adaptive" })),
        },
    }
    if let Some(format) = output.format {
        conv.response_format = Some(match format {
            Value::Object(ref map)
                if map.get("type") == Some(&json!("json_schema"))
                    && map.len() == 2
                    && map.contains_key("schema") =>
            {
                ResponseFormat::JsonSchema {
                    name: None,
                    schema: map["schema"].clone(),
                    strict: None,
                }
            }
            value => ResponseFormat::Opaque { value },
        });
    }

    if let Some(user_id) = req.metadata.and_then(|m| m.user_id) {
        conv.metadata.insert("user_id".to_string(), user_id);
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

fn dropped(at: impl Into<String>, detail: impl Into<String>) -> Diag {
    Diag::new(DiagKind::Dropped, at, detail)
}

fn media(source: Source, inliner: &mut Inliner<'_>) -> Result<MediaRef, ProtocolError> {
    match source {
        Source::Url { url } => Ok(MediaRef::Url { url }),
        Source::Base64 { media_type, data } => inliner.inline(&media_type, &data),
    }
}

fn hub_block(
    block: ContentBlock,
    out: &mut Vec<Block>,
    inliner: &mut Inliner<'_>,
) -> Result<(), ProtocolError> {
    match block {
        ContentBlock::Text { text, citations } => {
            out.push(Block::Text { text });
            out.extend(citations.iter().map(|c| Block::Citation(citation_of(c))));
        }
        ContentBlock::Image { source } => out.push(Block::Image {
            source: media(source, inliner)?,
        }),
        ContentBlock::Document { source, title } => out.push(Block::File {
            source: media(source, inliner)?,
            name: title,
        }),
        ContentBlock::ToolUse { id, name, input } => out.push(Block::ToolUse { id, name, input }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let mut inner = Vec::with_capacity(content.len());
            for part in content {
                hub_block(part, &mut inner, inliner)?;
            }
            out.push(Block::ToolResult {
                call_id: tool_use_id,
                content: inner,
                is_error: is_error == Some(true),
            });
        }
        ContentBlock::Thinking {
            thinking,
            signature,
        } => out.push(Block::Reasoning {
            text: thinking,
            signature: Some(signature),
            redacted: None,
        }),
        ContentBlock::RedactedThinking { data } => out.push(Block::Reasoning {
            text: String::new(),
            signature: None,
            redacted: Some(data),
        }),
    }
    Ok(())
}
