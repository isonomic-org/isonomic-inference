//! Chat-format request → conversation.

use serde_json::Value;

use super::types::{Message, Part, Request, TextContent, ToolChoice, UserContent};
use super::NAME;
use crate::accept::{parse_data_url, Accepted, Inliner};
use crate::conversation::{
    Block, Conversation, Effort, ReasoningConfig, ResponseFormat, Role, Sampling,
    ToolChoice as Choice, ToolSpec, Turn,
};
use crate::diag::{Diag, DiagKind, ProtocolError};
use crate::limits::Limits;

pub(super) fn accept(req: Request, limits: &Limits) -> Result<Accepted, ProtocolError> {
    let mut diags = Vec::new();
    let mut media = Inliner::new(limits);
    let mut conv = Conversation {
        model: (!req.model.is_empty()).then_some(req.model),
        sampling: Sampling {
            max_output_tokens: req.max_completion_tokens,
            temperature: req.temperature,
            top_p: req.top_p,
            stop: req.stop,
        },
        parallel_tool_calls: req.parallel_tool_calls,
        metadata: req.metadata,
        ..Conversation::default()
    };

    // Tool messages and the user message after them form one user turn.
    let mut user: Option<Turn> = None;
    for (m, message) in req.messages.into_iter().enumerate() {
        let at = format!("messages[{m}]");
        match message {
            Message::System { content } | Message::Developer { content } => {
                if !conv.turns.is_empty() || user.is_some() {
                    diags.push(Diag::new(
                        DiagKind::Relocated,
                        &at,
                        "system text after the first turn joins the system instructions",
                    ));
                }
                conv.system.push(text_of(content, &at, &mut diags));
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                let turn = user.get_or_insert_with(|| empty(Role::User));
                // A tool result after the user's own content starts a new turn.
                if turn
                    .blocks
                    .iter()
                    .any(|b| !matches!(b, Block::ToolResult { .. }))
                {
                    conv.turns.extend(user.take());
                }
                let text = text_of(content, &at, &mut diags);
                user.get_or_insert_with(|| empty(Role::User))
                    .blocks
                    .push(Block::ToolResult {
                        call_id: tool_call_id,
                        content: vec![Block::Text { text }],
                        is_error: false,
                    });
            }
            Message::User { content } => {
                let turn = user.get_or_insert_with(|| empty(Role::User));
                if turn
                    .blocks
                    .iter()
                    .any(|b| !matches!(b, Block::ToolResult { .. }))
                {
                    conv.turns.extend(user.take());
                }
                let blocks = &mut user.get_or_insert_with(|| empty(Role::User)).blocks;
                match content {
                    UserContent::Text(text) => blocks.push(Block::Text { text }),
                    UserContent::Parts(parts) => {
                        for (p, part) in parts.into_iter().enumerate() {
                            let part_at = format!("{at}.content[{p}]");
                            blocks.extend(user_part(part, &part_at, &mut media, &mut diags)?);
                        }
                    }
                }
            }
            Message::Assistant {
                content,
                tool_calls,
                reasoning_content,
            } => {
                conv.turns.extend(user.take());
                let mut blocks = Vec::new();
                if let Some(text) = reasoning_content {
                    blocks.push(Block::Reasoning {
                        text,
                        signature: None,
                        redacted: None,
                    });
                }
                if let Some(content) = content {
                    blocks.push(Block::Text {
                        text: text_of(content, &at, &mut diags),
                    });
                }
                for (c, call) in tool_calls.into_iter().enumerate() {
                    let input =
                        serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| {
                            diags.push(Diag::new(
                                DiagKind::Approximated,
                                format!("{at}.tool_calls[{c}]"),
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
                conv.turns.push(Turn {
                    role: Role::Assistant,
                    blocks,
                });
            }
        }
    }
    conv.turns.extend(user);

    for (i, tool) in req.tools.into_iter().enumerate() {
        if tool.kind != "function" {
            diags.push(Diag::new(
                DiagKind::Dropped,
                format!("tools[{i}]"),
                format!("tool type {:?} is not a function", tool.kind),
            ));
            continue;
        }
        conv.tools.push(ToolSpec {
            name: tool.function.name,
            description: tool.function.description,
            input_schema: tool.function.parameters,
            strict: tool.function.strict,
        });
    }
    conv.tool_choice = match req.tool_choice {
        None => None,
        Some(ToolChoice::Mode(mode)) => match mode.as_str() {
            "auto" => Some(Choice::Auto),
            "none" => Some(Choice::None),
            "required" => Some(Choice::Required),
            other => {
                return Err(ProtocolError::Invalid(format!(
                    "tool_choice {other:?} is not known"
                )))
            }
        },
        Some(ToolChoice::Named { function, .. }) => Some(Choice::Tool {
            name: function.name,
        }),
    };
    conv.response_format = req.response_format.map(|format| {
        let schema = &format["json_schema"];
        let plain = format["type"] == "json_schema"
            && format.as_object().is_some_and(|m| m.len() == 2)
            && schema["schema"].is_object()
            && schema["name"].is_string()
            && schema.as_object().is_some_and(|m| {
                m.keys()
                    .all(|k| ["name", "schema", "strict"].contains(&k.as_str()))
            });
        if plain {
            ResponseFormat::JsonSchema {
                name: schema["name"].as_str().map(str::to_string),
                schema: schema["schema"].clone(),
                strict: schema["strict"].as_bool(),
            }
        } else {
            ResponseFormat::Opaque { value: format }
        }
    });
    if let Some(effort) = req.reasoning_effort {
        match Effort::from_chat(&effort) {
            Some(effort) => {
                conv.reasoning = Some(ReasoningConfig {
                    effort: Some(effort),
                    budget_tokens: None,
                })
            }
            None => conv
                .ext
                .insert(NAME, "reasoning_effort", Value::String(effort)),
        }
    }
    let stream_usage = req.stream_options.is_some_and(|o| o.include_usage);
    for (key, value) in req.extra {
        conv.ext.insert(NAME, key, value);
    }
    limits.check_conversation(&conv)?;
    Ok(Accepted {
        conversation: conv,
        stream: req.stream == Some(true),
        stream_usage,
        blobs: media.blobs,
        diags,
    })
}

fn empty(role: Role) -> Turn {
    Turn {
        role,
        blocks: Vec::new(),
    }
}

/// Text content; parts other than text are reported and left out.
fn text_of(content: TextContent, at: &str, diags: &mut Vec<Diag>) -> String {
    match content {
        TextContent::Text(text) => text,
        TextContent::Parts(parts) => {
            let mut out = String::new();
            for part in parts {
                match part {
                    Part::Text { text } | Part::Refusal { refusal: text } => out.push_str(&text),
                    _ => diags.push(Diag::new(
                        DiagKind::Dropped,
                        at,
                        "only text parts are kept here",
                    )),
                }
            }
            out
        }
    }
}

fn user_part(
    part: Part,
    at: &str,
    media: &mut Inliner<'_>,
    diags: &mut Vec<Diag>,
) -> Result<Option<Block>, ProtocolError> {
    Ok(Some(match part {
        Part::Text { text } => Block::Text { text },
        Part::ImageUrl { image_url } => Block::Image {
            source: media.url(&image_url.url)?,
        },
        Part::File { file } => match file.file_data {
            // Inline data, as a data URL or as bare base64.
            Some(data) => Block::File {
                source: match parse_data_url(&data) {
                    Some((media_type, base64)) => media.inline(media_type, base64)?,
                    None => media.inline("application/octet-stream", &data)?,
                },
                name: file.filename,
            },
            None => {
                diags.push(Diag::new(
                    DiagKind::Dropped,
                    at,
                    "a file by ID; only inline files are kept",
                ));
                return Ok(None);
            }
        },
        Part::Refusal { refusal } => Block::Text { text: refusal },
    }))
}
