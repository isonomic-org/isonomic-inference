//! Conversation → messages-format request.

use serde_json::{json, Map, Value};

use super::types::{
    Content, ContentBlock, Message, Metadata, OutputConfig, Request, Source, System, Thinking,
    Tool, ToolChoice,
};
use super::NAME;
use crate::conversation::{
    Block, Conversation, Effort, ResponseFormat, Role, ToolChoice as Choice,
};
use crate::diag::{Diag, DiagKind, ProtocolError};
use crate::emit::{merge_ext, resolve, safe_id, EmitOptions, Emitted, Media};

pub(super) fn emit(
    conv: &Conversation,
    opts: &EmitOptions<'_>,
) -> Result<Emitted<Request>, ProtocolError> {
    opts.limits.check_conversation(conv)?;
    let mut diags = Vec::new();

    let model = match conv.model.as_deref().or(opts.default_model) {
        Some(model) => model.to_string(),
        None => {
            diags.push(Diag::new(
                DiagKind::Defaulted,
                "model",
                "no model named; sent empty",
            ));
            String::new()
        }
    };
    let max_tokens = match conv.sampling.max_output_tokens {
        Some(n) => n,
        None => {
            diags.push(Diag::new(
                DiagKind::Defaulted,
                "sampling.max_output_tokens",
                format!(
                    "the format requires a limit; sent {}",
                    opts.default_max_tokens
                ),
            ));
            opts.default_max_tokens
        }
    };

    let mut messages = Vec::with_capacity(conv.turns.len());
    for (t, turn) in conv.turns.iter().enumerate() {
        let mut blocks = Vec::with_capacity(turn.blocks.len());
        for (b, block) in turn.blocks.iter().enumerate() {
            let at = format!("turns[{t}].blocks[{b}]");
            emit_block(block, turn.role, at, &mut blocks, opts, &mut diags)?;
        }
        let content = match blocks.as_slice() {
            [ContentBlock::Text { text, citations }] if citations.is_empty() => {
                Content::Text(text.clone())
            }
            _ => Content::Blocks(blocks),
        };
        let role = match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        messages.push(Message {
            role: role.to_string(),
            content,
        });
    }

    let system = match conv.system.as_slice() {
        [] => None,
        [one] => Some(System::Text(one.clone())),
        many => Some(System::Blocks(
            many.iter()
                .map(|text| ContentBlock::Text {
                    text: text.clone(),
                    citations: Vec::new(),
                })
                .collect(),
        )),
    };

    let (thinking, mut output_config) = reasoning(conv, &mut diags);
    if let Some(format) = &conv.response_format {
        let value = match format {
            ResponseFormat::JsonSchema {
                name,
                schema,
                strict,
            } => {
                if name.is_some() || strict.is_some() {
                    diags.push(Diag::new(
                        DiagKind::Dropped,
                        "response_format",
                        "the format's schema output has no name or strict flag",
                    ));
                }
                json!({ "type": "json_schema", "schema": schema })
            }
            ResponseFormat::Opaque { value } => value.clone(),
        };
        output_config
            .get_or_insert_with(OutputConfig::default)
            .format = Some(value);
    }

    let mut metadata = None;
    for (key, value) in &conv.metadata {
        if key == "user_id" {
            metadata = Some(Metadata {
                user_id: Some(value.clone()),
            });
        } else {
            diags.push(Diag::new(
                DiagKind::Dropped,
                format!("metadata.{key}"),
                "the format's metadata holds user_id only",
            ));
        }
    }

    let mut request = Request {
        model,
        max_tokens,
        messages,
        system,
        stop_sequences: conv.sampling.stop.clone(),
        temperature: conv.sampling.temperature,
        top_p: conv.sampling.top_p,
        stream: opts.stream.then_some(true),
        tools: conv
            .tools
            .iter()
            .map(|tool| Tool {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
                strict: tool.strict,
            })
            .collect(),
        tool_choice: tool_choice(conv, &mut diags),
        thinking,
        output_config,
        metadata,
        extra: Map::new(),
    };
    let known = match serde_json::to_value(&request)? {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let mut merged = known.clone();
    merge_ext(&mut merged, conv.ext.for_format(NAME), &mut diags);
    request.extra = merged
        .into_iter()
        .filter(|(key, _)| !known.contains_key(key))
        .collect();
    Ok(Emitted { request, diags })
}

fn emit_block(
    block: &Block,
    role: Role,
    at: String,
    out: &mut Vec<ContentBlock>,
    opts: &EmitOptions<'_>,
    diags: &mut Vec<Diag>,
) -> Result<(), ProtocolError> {
    let emitted = match block {
        Block::Text { text } => ContentBlock::Text {
            text: text.clone(),
            citations: Vec::new(),
        },
        Block::Citation(citation) => {
            match (out.last_mut(), &citation.raw) {
                (Some(ContentBlock::Text { citations, .. }), Some(raw)) => {
                    citations.push(raw.clone());
                }
                _ => diags.push(Diag::new(
                    DiagKind::Dropped,
                    at,
                    "a citation needs the format's own location and a text block before it",
                )),
            }
            return Ok(());
        }
        Block::Reasoning {
            text,
            signature,
            redacted,
        } => {
            if role == Role::User {
                diags.push(Diag::new(DiagKind::Dropped, at, "reasoning in a user turn"));
                return Ok(());
            }
            match (redacted, signature) {
                (Some(data), _) => ContentBlock::RedactedThinking { data: data.clone() },
                (None, Some(signature)) => ContentBlock::Thinking {
                    thinking: text.clone(),
                    signature: signature.clone(),
                },
                (None, None) => {
                    diags.push(Diag::new(
                        DiagKind::Dropped,
                        at,
                        "reasoning without a signature cannot be sent back in this format",
                    ));
                    return Ok(());
                }
            }
        }
        Block::Image { source } => ContentBlock::Image {
            source: source_of(resolve(source, opts)?),
        },
        Block::File { source, name } => ContentBlock::Document {
            source: source_of(resolve(source, opts)?),
            title: name.clone(),
        },
        Block::ToolUse { id, name, input } => ContentBlock::ToolUse {
            id: safe_id(id, format!("{at}.id"), diags),
            name: name.clone(),
            input: input.clone(),
        },
        Block::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let mut inner = Vec::with_capacity(content.len());
            for (i, part) in content.iter().enumerate() {
                let part_at = format!("{at}.content[{i}]");
                match part {
                    Block::Text { .. } | Block::Image { .. } => {
                        emit_block(part, role, part_at, &mut inner, opts, diags)?;
                    }
                    _ => diags.push(Diag::new(
                        DiagKind::Dropped,
                        part_at,
                        "tool results carry text and images only",
                    )),
                }
            }
            ContentBlock::ToolResult {
                tool_use_id: safe_id(call_id, format!("{at}.call_id"), diags),
                content: inner,
                is_error: is_error.then_some(true),
            }
        }
    };
    out.push(emitted);
    Ok(())
}

fn source_of(media: Media) -> Source {
    match media {
        Media::Url(url) => Source::Url { url },
        Media::Inline { media_type, base64 } => Source::Base64 {
            media_type,
            data: base64,
        },
    }
}

fn tool_choice(conv: &Conversation, diags: &mut Vec<Diag>) -> Option<ToolChoice> {
    if conv.parallel_tool_calls == Some(true) {
        diags.push(Diag::new(
            DiagKind::Approximated,
            "parallel_tool_calls",
            "parallel calls are the format's default; it can only turn them off",
        ));
    }
    let disable = (conv.parallel_tool_calls == Some(false)).then_some(true);
    match &conv.tool_choice {
        None => disable.map(|d| ToolChoice::Auto {
            disable_parallel_tool_use: Some(d),
        }),
        Some(Choice::Auto) => Some(ToolChoice::Auto {
            disable_parallel_tool_use: disable,
        }),
        Some(Choice::None) => Some(ToolChoice::None),
        Some(Choice::Required) => Some(ToolChoice::Any {
            disable_parallel_tool_use: disable,
        }),
        Some(Choice::Tool { name }) => Some(ToolChoice::Tool {
            name: name.clone(),
            disable_parallel_tool_use: disable,
        }),
    }
}

/// The format speaks in a token budget or in an effort level with adaptive
/// thinking; see `reasoning::EFFORT_TABLE`.
fn reasoning(
    conv: &Conversation,
    diags: &mut Vec<Diag>,
) -> (Option<Thinking>, Option<OutputConfig>) {
    let Some(config) = conv.reasoning else {
        return (None, None);
    };
    if config.effort == Some(Effort::None) {
        return (Some(Thinking::Disabled), None);
    }
    let effort = config.effort.and_then(|effort| {
        let row = effort.row();
        if !row.messages_exact {
            diags.push(Diag::new(
                DiagKind::Approximated,
                "reasoning.effort",
                format!("{:?} sent as {:?}", effort, row.messages.unwrap_or("")),
            ));
        }
        row.messages
    });
    let output_config = effort.map(|e| OutputConfig {
        effort: Some(e.to_string()),
        format: None,
    });
    let thinking = match config.budget_tokens {
        Some(budget_tokens) => Thinking::Enabled { budget_tokens },
        None if effort.is_some() => Thinking::Adaptive,
        None => return (None, output_config),
    };
    (Some(thinking), output_config)
}
