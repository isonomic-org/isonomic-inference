//! Conversation → responses-format request.

use serde_json::{json, Map, Value};

use super::types::{Input, ReasoningParam, Request, TextConfig, Tool};
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
    if conv.system.len() > 1 {
        diags.push(Diag::new(
            DiagKind::Approximated,
            "system",
            "several system instructions joined with blank lines",
        ));
    }
    if !conv.sampling.stop.is_empty() {
        diags.push(Diag::new(
            DiagKind::Dropped,
            "sampling.stop",
            "the format has no stop sequences",
        ));
    }

    let mut items = Vec::new();
    for (t, turn) in conv.turns.iter().enumerate() {
        match turn.role {
            Role::User => user_items(turn, t, &mut items, opts, &mut diags)?,
            Role::Assistant => assistant_items(turn, t, &mut items, &mut diags),
        }
    }

    let reasoning = conv.reasoning.and_then(|config| {
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
        Some(ReasoningParam {
            effort: Some(effort.row().chat.to_string()),
            extra: Map::new(),
        })
    });

    let text = conv.response_format.as_ref().map(|format| TextConfig {
        format: Some(match format {
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
                let mut format = json!({ "type": "json_schema", "name": name, "schema": schema });
                if let Some(strict) = strict {
                    format["strict"] = Value::Bool(*strict);
                }
                format
            }
            ResponseFormat::Opaque { value } => value.clone(),
        }),
        extra: Map::new(),
    });

    let mut request = Request {
        model,
        input: Input::Items(items),
        instructions: (!conv.system.is_empty()).then(|| conv.system.join("\n\n")),
        max_output_tokens: conv.sampling.max_output_tokens,
        temperature: conv.sampling.temperature,
        top_p: conv.sampling.top_p,
        stream: opts.stream.then_some(true),
        tools: conv
            .tools
            .iter()
            .map(|tool| Tool {
                kind: "function".to_string(),
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
                strict: tool.strict,
            })
            .collect(),
        tool_choice: conv.tool_choice.as_ref().map(|choice| match choice {
            Choice::Auto => json!("auto"),
            Choice::None => json!("none"),
            Choice::Required => json!("required"),
            Choice::Tool { name } => json!({ "type": "function", "name": name }),
        }),
        parallel_tool_calls: conv.parallel_tool_calls,
        text,
        reasoning,
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

fn media_url(media: Media) -> String {
    match media {
        Media::Url(url) => url,
        Media::Inline { media_type, base64 } => format!("data:{media_type};base64,{base64}"),
    }
}

/// Text, image and file parts for a user message or a tool output.
fn input_part(
    block: &Block,
    at: &str,
    opts: &EmitOptions<'_>,
    diags: &mut Vec<Diag>,
) -> Result<Option<Value>, ProtocolError> {
    Ok(Some(match block {
        Block::Text { text } => json!({ "type": "input_text", "text": text }),
        Block::Image { source } => {
            json!({ "type": "input_image", "image_url": media_url(resolve(source, opts)?) })
        }
        Block::File { source, name } => {
            let mut part = match resolve(source, opts)? {
                Media::Url(url) => json!({ "type": "input_file", "file_url": url }),
                inline => json!({ "type": "input_file", "file_data": media_url(inline) }),
            };
            if let Some(name) = name {
                part["filename"] = json!(name);
            }
            part
        }
        _ => {
            diags.push(Diag::new(
                DiagKind::Dropped,
                at,
                "user messages carry text, images and files only",
            ));
            return Ok(None);
        }
    }))
}

fn user_items(
    turn: &Turn,
    t: usize,
    items: &mut Vec<Value>,
    opts: &EmitOptions<'_>,
    diags: &mut Vec<Diag>,
) -> Result<(), ProtocolError> {
    let mut parts = Vec::new();
    for (b, block) in turn.blocks.iter().enumerate() {
        let at = format!("turns[{t}].blocks[{b}]");
        match block {
            Block::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                if !parts.is_empty() {
                    diags.push(Diag::new(
                        DiagKind::Relocated,
                        &at,
                        "tool output sent ahead of the turn's other content",
                    ));
                }
                if *is_error {
                    diags.push(Diag::new(
                        DiagKind::Dropped,
                        &at,
                        "the format has no error flag on tool output",
                    ));
                }
                let output = match content.as_slice() {
                    [Block::Text { text }] => json!(text),
                    _ => {
                        let mut list = Vec::new();
                        for (i, part) in content.iter().enumerate() {
                            let part_at = format!("{at}.content[{i}]");
                            list.extend(input_part(part, &part_at, opts, diags)?);
                        }
                        Value::Array(list)
                    }
                };
                items.push(
                    json!({ "type": "function_call_output", "call_id": call_id, "output": output }),
                );
            }
            _ => parts.extend(input_part(block, &at, opts, diags)?),
        }
    }
    if !parts.is_empty() || turn.blocks.is_empty() {
        items.push(json!({ "type": "message", "role": "user", "content": parts }));
    }
    Ok(())
}

fn assistant_items(turn: &Turn, t: usize, items: &mut Vec<Value>, diags: &mut Vec<Diag>) {
    // Text parts gather into one message until something else intervenes.
    let mut message: Vec<Value> = Vec::new();
    let flush = |message: &mut Vec<Value>, items: &mut Vec<Value>| {
        if !message.is_empty() {
            items.push(json!({ "type": "message", "role": "assistant",
                "content": std::mem::take(message) }));
        }
    };
    for (b, block) in turn.blocks.iter().enumerate() {
        let at = format!("turns[{t}].blocks[{b}]");
        match block {
            Block::Text { text } => message.push(json!({ "type": "output_text", "text": text })),
            Block::Citation(citation) => match (message.last_mut(), &citation.raw) {
                (Some(part), Some(raw))
                    if raw["type"] == "url_citation" && raw.get("url").is_some() =>
                {
                    let list = part
                        .as_object_mut()
                        .expect("a part is an object")
                        .entry("annotations")
                        .or_insert_with(|| json!([]));
                    if let Value::Array(list) = list {
                        list.push(raw.clone());
                    }
                }
                _ => diags.push(Diag::new(
                    DiagKind::Dropped,
                    at,
                    "a citation needs the format's own annotation and a text part before it",
                )),
            },
            Block::Reasoning {
                text, signature, ..
            } => {
                flush(&mut message, items);
                match signature.as_deref().and_then(parse_signature) {
                    Some(mut item) => {
                        if !text.is_empty() {
                            item["summary"] = json!([{ "type": "summary_text", "text": text }]);
                        }
                        items.push(item);
                    }
                    None => diags.push(Diag::new(
                        DiagKind::Dropped,
                        at,
                        "reasoning goes back only with the ID this format gave it",
                    )),
                }
            }
            Block::ToolUse { id, name, input } => {
                flush(&mut message, items);
                items.push(
                    json!({ "type": "function_call", "call_id": id, "name": name,
                    "arguments": input.to_string() }),
                );
            }
            _ => diags.push(Diag::new(
                DiagKind::Dropped,
                at,
                "assistant output carries text, reasoning and function calls only",
            )),
        }
    }
    flush(&mut message, items);
}

/// The opaque signature this format's decoder writes: the reasoning item's
/// ID and its encrypted content, as a JSON object.
pub(crate) fn parse_signature(signature: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(signature).ok()?;
    let id = value["id"].as_str()?;
    let mut item = json!({ "type": "reasoning", "id": id, "summary": [] });
    if let Some(content) = value["encrypted_content"].as_str() {
        item["encrypted_content"] = json!(content);
    }
    Some(item)
}

pub(crate) fn signature_of(item: &Value) -> Option<String> {
    let id = item["id"].as_str()?;
    let mut signature = json!({ "id": id });
    if let Some(content) = item["encrypted_content"].as_str() {
        signature["encrypted_content"] = json!(content);
    }
    Some(signature.to_string())
}
