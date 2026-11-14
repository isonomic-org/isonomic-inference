//! Conversation → chat-format request.

use serde_json::{json, Map, Value};

use super::types::{
    FilePart, FunctionCall, FunctionDef, ImageUrl, Message, NamedFunction, Part, Request,
    StreamOptions, Tool, ToolCall, ToolChoice, UserContent,
};
use super::NAME;
use crate::conversation::{
    Block, Conversation, Effort, ResponseFormat, Role, ToolChoice as Choice, Turn,
};
use crate::diag::{Diag, DiagKind, ProtocolError};
use crate::emit::{merge_ext, resolve, EmitOptions, Emitted, Media};

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

    let mut messages: Vec<Message> = conv
        .system
        .iter()
        .map(|text| Message::System {
            content: text.clone(),
        })
        .collect();
    for (t, turn) in conv.turns.iter().enumerate() {
        match turn.role {
            Role::User => user_turn(turn, t, &mut messages, opts, &mut diags)?,
            Role::Assistant => messages.push(assistant_turn(turn, t, opts, &mut diags)),
        }
    }

    let mut request = Request {
        model,
        messages,
        max_completion_tokens: conv.sampling.max_output_tokens,
        temperature: conv.sampling.temperature,
        top_p: conv.sampling.top_p,
        stop: conv.sampling.stop.clone(),
        stream: opts.stream.then_some(true),
        stream_options: opts.stream.then_some(StreamOptions {
            include_usage: true,
        }),
        tools: conv
            .tools
            .iter()
            .map(|tool| Tool {
                kind: "function".to_string(),
                function: FunctionDef {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.input_schema.clone(),
                    strict: tool.strict,
                },
            })
            .collect(),
        tool_choice: conv.tool_choice.as_ref().map(|choice| match choice {
            Choice::Auto => ToolChoice::Mode("auto".into()),
            Choice::None => ToolChoice::Mode("none".into()),
            Choice::Required => ToolChoice::Mode("required".into()),
            Choice::Tool { name } => ToolChoice::Named {
                kind: "function".into(),
                function: NamedFunction { name: name.clone() },
            },
        }),
        parallel_tool_calls: conv.parallel_tool_calls,
        response_format: conv
            .response_format
            .as_ref()
            .map(|f| response_format(f, &mut diags)),
        reasoning_effort: reasoning_effort(conv, &mut diags),
        metadata: conv.metadata.clone(),
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

/// A user turn becomes tool messages (results) followed by one user message.
fn user_turn(
    turn: &Turn,
    t: usize,
    out: &mut Vec<Message>,
    opts: &EmitOptions<'_>,
    diags: &mut Vec<Diag>,
) -> Result<(), ProtocolError> {
    let mut parts = Vec::new();
    let mut moved_ahead = false;
    for (b, block) in turn.blocks.iter().enumerate() {
        let at = format!("turns[{t}].blocks[{b}]");
        match block {
            Block::Text { .. } | Block::Image { .. } | Block::File { .. } => {
                parts.extend(part(block, &at, opts, diags)?);
            }
            Block::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                moved_ahead |= !parts.is_empty();
                let mut texts = Vec::new();
                for (i, item) in content.iter().enumerate() {
                    let item_at = format!("{at}.content[{i}]");
                    match item {
                        Block::Text { text } => texts.push(text.clone()),
                        Block::Image { .. } => {
                            diags.push(Diag::new(
                                DiagKind::Relocated,
                                &item_at,
                                "tool messages carry text only; image sent in the user message",
                            ));
                            parts.extend(part(item, &item_at, opts, diags)?);
                        }
                        _ => diags.push(Diag::new(
                            DiagKind::Dropped,
                            item_at,
                            "tool results carry text and images only",
                        )),
                    }
                }
                if texts.len() > 1 {
                    diags.push(Diag::new(
                        DiagKind::Approximated,
                        &at,
                        "several text blocks joined with newlines",
                    ));
                }
                if *is_error {
                    diags.push(Diag::new(
                        DiagKind::Dropped,
                        &at,
                        "the format has no error flag on tool results",
                    ));
                }
                out.push(Message::Tool {
                    tool_call_id: call_id.clone(),
                    content: texts.join("\n"),
                });
            }
            _ => diags.push(Diag::new(
                DiagKind::Dropped,
                at,
                "user messages carry text, images and files only",
            )),
        }
    }
    if moved_ahead {
        diags.push(Diag::new(
            DiagKind::Relocated,
            format!("turns[{t}]"),
            "tool results sent ahead of the turn's other content",
        ));
    }
    let has_tools = turn
        .blocks
        .iter()
        .any(|b| matches!(b, Block::ToolResult { .. }));
    if parts.is_empty() && has_tools {
        return Ok(());
    }
    let content = match parts.as_slice() {
        [] => UserContent::Text(String::new()),
        [Part::Text { text }] => UserContent::Text(text.clone()),
        _ => UserContent::Parts(parts),
    };
    out.push(Message::User { content });
    Ok(())
}

fn part(
    block: &Block,
    at: &str,
    opts: &EmitOptions<'_>,
    diags: &mut Vec<Diag>,
) -> Result<Option<Part>, ProtocolError> {
    Ok(Some(match block {
        Block::Text { text } => Part::Text { text: text.clone() },
        Block::Image { source } => Part::ImageUrl {
            image_url: ImageUrl {
                url: match resolve(source, opts)? {
                    Media::Url(url) => url,
                    Media::Inline { media_type, base64 } => {
                        format!("data:{media_type};base64,{base64}")
                    }
                },
            },
        },
        Block::File { source, name } => match resolve(source, opts)? {
            Media::Inline { media_type, base64 } => Part::File {
                file: FilePart {
                    file_data: Some(format!("data:{media_type};base64,{base64}")),
                    filename: name.clone(),
                },
            },
            Media::Url(_) => {
                diags.push(Diag::new(
                    DiagKind::Dropped,
                    at,
                    "the format takes files inline only, not by URL",
                ));
                return Ok(None);
            }
        },
        _ => return Ok(None),
    }))
}

fn assistant_turn(turn: &Turn, t: usize, opts: &EmitOptions<'_>, diags: &mut Vec<Diag>) -> Message {
    let mut text = Vec::new();
    let mut reasoning = Vec::new();
    let mut tool_calls = Vec::new();
    for (b, block) in turn.blocks.iter().enumerate() {
        let at = format!("turns[{t}].blocks[{b}]");
        match block {
            Block::Text { text: t } => text.push(t.clone()),
            Block::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                id: id.clone(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: name.clone(),
                    arguments: input.to_string(),
                },
            }),
            Block::Reasoning {
                text: r,
                signature,
                redacted,
            } if opts.chat_reasoning_history => {
                if signature.is_some() || redacted.is_some() {
                    diags.push(Diag::new(
                        DiagKind::Dropped,
                        &at,
                        "the format has no place for a reasoning signature",
                    ));
                }
                reasoning.push(r.clone());
            }
            _ => diags.push(Diag::new(
                DiagKind::Dropped,
                at,
                "assistant messages carry text and tool calls only",
            )),
        }
    }
    if text.len() > 1 {
        diags.push(Diag::new(
            DiagKind::Approximated,
            format!("turns[{t}]"),
            "several text blocks joined into one",
        ));
    }
    Message::Assistant {
        content: (!text.is_empty() || tool_calls.is_empty()).then(|| text.concat()),
        tool_calls,
        reasoning_content: (!reasoning.is_empty()).then(|| reasoning.concat()),
    }
}

fn response_format(format: &ResponseFormat, diags: &mut Vec<Diag>) -> Value {
    match format {
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let name = name.clone().unwrap_or_else(|| {
                diags.push(Diag::new(
                    DiagKind::Defaulted,
                    "response_format.name",
                    "the format requires a schema name; sent \"response\"",
                ));
                "response".to_string()
            });
            let mut inner = json!({ "name": name, "schema": schema });
            if let Some(strict) = strict {
                inner["strict"] = Value::Bool(*strict);
            }
            json!({ "type": "json_schema", "json_schema": inner })
        }
        ResponseFormat::Opaque { value } => value.clone(),
    }
}

/// The format speaks in effort levels; see `reasoning::EFFORT_TABLE`.
fn reasoning_effort(conv: &Conversation, diags: &mut Vec<Diag>) -> Option<String> {
    let config = conv.reasoning?;
    let effort = match (config.effort, config.budget_tokens) {
        (Some(effort), budget) => {
            if budget.is_some() {
                diags.push(Diag::new(
                    DiagKind::Dropped,
                    "reasoning.budget_tokens",
                    "the format takes an effort level, not a budget",
                ));
            }
            effort
        }
        (None, Some(budget)) => {
            let effort = Effort::from_budget(budget);
            diags.push(Diag::new(
                DiagKind::Approximated,
                "reasoning.budget_tokens",
                format!("budget {budget} sent as effort {:?}", effort.row().chat),
            ));
            effort
        }
        (None, None) => return None,
    };
    Some(effort.row().chat.to_string())
}
