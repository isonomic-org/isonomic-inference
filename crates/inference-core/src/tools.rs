// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tool types for the chat request, the chat message and the streamed chunk.
//!
//! These extend the frozen chat types in [`crate::chat`], additively: a
//! request may offer tools, an assistant message may carry the calls the
//! model made, a tool message carries one call's result, and a streamed
//! chunk carries call fragments. The wire shape is the chat format's, with
//! every tool and call tagged `"type": "function"`.

use serde::{Deserialize, Deserializer, Serialize};

/// The only tool kind the wire format defines.
const FUNCTION: &str = "function";

/// A tool the model may call. On the wire:
/// `{"type": "function", "function": {"name", "description", "parameters"}}`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(into = "ToolWire", try_from = "ToolWire")]
pub struct ChatTool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments, an object schema.
    pub parameters: serde_json::Value,
}

/// One call the model made, on an assistant message. On the wire:
/// `{"id", "type": "function", "function": {"name", "arguments"}}`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(into = "CallWire", try_from = "CallWire")]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    /// The arguments as JSON text, exactly as the model wrote them. They are
    /// not parsed here: a model can write invalid JSON, and the caller
    /// decides what to do with it.
    pub arguments: String,
}

/// A fragment of a tool call in a streamed chunk. Fragments with the same
/// `index` belong to one call. The first fragment of a call carries its `id`
/// and `name`; every fragment may carry more `arguments` text, to be
/// appended in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

#[derive(Serialize, Deserialize)]
struct ToolWire {
    #[serde(rename = "type")]
    kind: String,
    function: FunctionDefWire,
}

#[derive(Serialize, Deserialize)]
struct FunctionDefWire {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "empty_object")]
    parameters: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct CallWire {
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionCallWire,
}

#[derive(Serialize, Deserialize)]
struct FunctionCallWire {
    name: String,
    #[serde(default)]
    arguments: String,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({ "type": "object" })
}

fn function_kind(kind: &str) -> Result<(), String> {
    if kind == FUNCTION {
        Ok(())
    } else {
        Err(format!("unsupported tool type {kind:?}"))
    }
}

impl From<ChatTool> for ToolWire {
    fn from(tool: ChatTool) -> Self {
        ToolWire {
            kind: FUNCTION.to_string(),
            function: FunctionDefWire {
                name: tool.name,
                description: tool.description,
                parameters: tool.parameters,
            },
        }
    }
}

impl TryFrom<ToolWire> for ChatTool {
    type Error = String;
    fn try_from(wire: ToolWire) -> Result<Self, String> {
        function_kind(&wire.kind)?;
        Ok(ChatTool {
            name: wire.function.name,
            description: wire.function.description,
            parameters: wire.function.parameters,
        })
    }
}

impl From<ChatToolCall> for CallWire {
    fn from(call: ChatToolCall) -> Self {
        CallWire {
            id: call.id,
            kind: FUNCTION.to_string(),
            function: FunctionCallWire {
                name: call.name,
                arguments: call.arguments,
            },
        }
    }
}

impl TryFrom<CallWire> for ChatToolCall {
    type Error = String;
    fn try_from(wire: CallWire) -> Result<Self, String> {
        function_kind(&wire.kind)?;
        Ok(ChatToolCall {
            id: wire.id,
            name: wire.function.name,
            arguments: wire.function.arguments,
        })
    }
}

/// Reads a message's `content` as text, taking `null` (an assistant message
/// that only calls tools) as empty.
pub(crate) fn null_as_empty<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(de)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tool_is_a_tagged_function_on_the_wire_and_reads_back() {
        let tool = ChatTool {
            name: "read_file".to_string(),
            description: "Read a file.".to_string(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        };
        let wire = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            wire,
            json!({"type": "function", "function": {
                "name": "read_file",
                "description": "Read a file.",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
            }})
        );
        assert_eq!(serde_json::from_value::<ChatTool>(wire).unwrap(), tool);
    }

    #[test]
    fn a_call_is_a_tagged_function_with_text_arguments() {
        let call = ChatToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path":"a.txt"}"#.to_string(),
        };
        let wire = serde_json::to_value(&call).unwrap();
        assert_eq!(
            wire,
            json!({"id": "call_1", "type": "function", "function": {
                "name": "read_file", "arguments": "{\"path\":\"a.txt\"}",
            }})
        );
        assert_eq!(serde_json::from_value::<ChatToolCall>(wire).unwrap(), call);
    }

    #[test]
    fn other_tool_kinds_are_refused() {
        let tool = json!({"type": "web_search", "function": {"name": "x"}});
        assert!(serde_json::from_value::<ChatTool>(tool).is_err());
        let call = json!({"id": "c", "type": "custom", "function": {"name": "x"}});
        assert!(serde_json::from_value::<ChatToolCall>(call).is_err());
    }
}

/// Joins streamed [`ToolCallDelta`] fragments into whole calls, in index
/// order. A call takes its id and name from the first fragment that has
/// them; a later fragment of the same index with a different id or name is
/// refused, as it would splice two calls into one. A call that never got a
/// name is refused at the end; one that never got an id is given
/// `call_<index>`.
#[derive(Debug, Clone, Default)]
pub struct ToolCallAssembler {
    calls: std::collections::BTreeMap<u32, ChatToolCall>,
}

impl ToolCallAssembler {
    /// Add one chunk's fragments, in order.
    pub fn push(&mut self, deltas: &[ToolCallDelta]) -> Result<(), String> {
        for d in deltas {
            let call = self.calls.entry(d.index).or_default();
            for (have, new, what) in [
                (&mut call.id, &d.id, "id"),
                (&mut call.name, &d.name, "name"),
            ] {
                match new {
                    Some(new) if have.is_empty() => *have = new.clone(),
                    Some(new) if have != new => {
                        return Err(format!("tool call {} got a second {what}", d.index))
                    }
                    _ => {}
                }
            }
            call.arguments.push_str(&d.arguments);
        }
        Ok(())
    }

    /// Whether any fragment has arrived.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// The whole calls, in index order.
    pub fn finish(self) -> Result<Vec<ChatToolCall>, String> {
        self.calls
            .into_iter()
            .map(|(index, mut call)| {
                if call.name.is_empty() {
                    return Err(format!("tool call {index} has no name"));
                }
                if call.id.is_empty() {
                    call.id = format!("call_{index}");
                }
                Ok(call)
            })
            .collect()
    }
}

#[cfg(test)]
mod assembler_tests {
    use super::*;

    fn d(index: u32, id: Option<&str>, name: Option<&str>, args: &str) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            arguments: args.to_string(),
        }
    }

    #[test]
    fn interleaved_fragments_become_whole_calls_in_order() {
        let mut a = ToolCallAssembler::default();
        a.push(&[d(1, Some("b"), Some("g"), "{\"x")]).unwrap();
        a.push(&[d(0, Some("a"), Some("f"), "{}")]).unwrap();
        a.push(&[d(1, None, None, "\":1}")]).unwrap();
        let calls = a.finish().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[0].id.as_str(), calls[0].name.as_str()), ("a", "f"));
        assert_eq!(calls[1].arguments, "{\"x\":1}");
    }

    #[test]
    fn a_conflicting_id_or_name_is_refused() {
        let mut a = ToolCallAssembler::default();
        a.push(&[d(0, Some("a"), Some("f"), "")]).unwrap();
        assert!(a.clone().push(&[d(0, Some("b"), None, "")]).is_err());
        assert!(a.push(&[d(0, None, Some("g"), "")]).is_err());
    }

    #[test]
    fn a_call_without_a_name_is_refused_and_one_without_an_id_is_named() {
        let mut a = ToolCallAssembler::default();
        a.push(&[d(0, None, Some("f"), "{}")]).unwrap();
        assert_eq!(a.clone().finish().unwrap()[0].id, "call_0");
        a.push(&[d(1, Some("x"), None, "")]).unwrap();
        assert!(a.finish().is_err());
    }
}
