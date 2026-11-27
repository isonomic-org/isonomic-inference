// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Chat and embedding request and response types.
//!
//! These are the data types the frozen provider trait speaks (see the
//! `provider` crate). The wire format is OpenAI-compatible end to end. Their
//! shape is carried over from the earlier build with two additive changes:
//! reasoning on streamed chunks, with a per-request control (Decision 4,
//! #28), and tools: tools offered on the request, tool calls and tool
//! results on messages, and tool-call fragments on streamed chunks (see
//! [`crate::tools`] and docs/status.md). Do not change them without asking.

use serde::{Deserialize, Serialize};

use crate::tools::null_as_empty;
pub use crate::tools::{ChatTool, ChatToolCall, ToolCallDelta};

/// Which provider answered. Remote providers are not built in this repo; the
/// identifier is kept so the engine's router can use one type for all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderId {
    LocalLlama,
    RemoteOpenAI,
}

/// A model a provider can serve, including context and capability hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub context_length: Option<u32>,
    pub capabilities: Vec<String>,
}

/// Provider lifecycle state (stopped / starting / healthy / degraded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHealth {
    Stopped,
    Starting,
    Healthy,
    Degraded,
}

/// One message of a conversation.
///
/// An assistant message that only calls tools has empty `content` and its
/// calls in `tool_calls`. A tool result is a message with role `"tool"`, the
/// result text in `content` and the call it answers in `tool_call_id`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// Always sent, as text. An empty string is accepted by the local
    /// server for an assistant message with tool calls; `null` is read back
    /// as empty.
    #[serde(default, deserialize_with = "null_as_empty")]
    pub content: String,
    /// Calls the model made, on an assistant message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,
    /// The call a tool message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// A plain text message: `"system"`, `"user"` or `"assistant"`.
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            ..Self::default()
        }
    }

    /// An assistant message carrying the calls the model made, with any
    /// text it wrote alongside them.
    pub fn assistant_tool_calls(content: impl Into<String>, calls: Vec<ChatToolCall>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            tool_calls: calls,
            tool_call_id: None,
        }
    }

    /// The result of one tool call, answering the call with `call_id`.
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// Whether a request wants the model to reason before it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningChoice {
    Off,
    On,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Per-request reasoning, layered on the server's launch default.
    /// `None` keeps the launch default. A provider may clamp it: the local
    /// provider keeps reasoning off on Intel Macs. Not sent as is; each
    /// provider translates it for its server.
    #[serde(skip)]
    pub reasoning: Option<ReasoningChoice>,
    /// Tools the model may call. Empty sends no `tools` field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ChatTool>,
    /// `"auto"`, `"none"` or `"required"`. `None` sends nothing, which the
    /// server takes as `"auto"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
}

/// One streamed delta from a chat completion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatChunk {
    /// Answer text.
    pub delta: String,
    /// Reasoning text, streamed apart from the answer, when the model
    /// reasons and the server reports it.
    pub reasoning: Option<String>,
    /// `"stop"`, `"length"`, or `"tool_calls"` when the model stopped to
    /// call tools.
    pub finish_reason: Option<String>,
    /// Tool-call fragments; see [`ToolCallDelta`].
    pub tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub input: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Embeddings {
    pub vectors: Vec<Vec<f32>>,
}

/// Cost estimate for a request, in micro-units (6 decimals) of the platform's
/// one stable-dollar asset. Always 0 for local inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostEstimate {
    pub usdc_micros: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_request_omits_unset_fields_on_the_wire() {
        let req = ChatRequest {
            model: None,
            messages: vec![ChatMessage::text("user", "hi")],
            temperature: None,
            max_tokens: Some(16),
            reasoning: Some(ReasoningChoice::On),
            ..ChatRequest::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("model").is_none());
        assert!(json.get("reasoning").is_none(), "providers translate it");
        assert!(json.get("temperature").is_none());
        assert!(json.get("tools").is_none());
        assert!(json.get("tool_choice").is_none());
        assert_eq!(json["max_tokens"], 16);
        assert_eq!(
            json["messages"][0],
            json!({"role": "user", "content": "hi"})
        );
    }

    fn read_file_tool() -> ChatTool {
        ChatTool {
            name: "read_file".to_string(),
            description: "Read a file from the workspace.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        }
    }

    #[test]
    fn a_request_with_tools_sends_them_as_functions_with_the_choice() {
        let req = ChatRequest {
            messages: vec![ChatMessage::text("user", "Open a.txt")],
            tools: vec![read_file_tool()],
            tool_choice: Some("auto".to_string()),
            ..ChatRequest::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["tools"],
            json!([{"type": "function", "function": {
                "name": "read_file",
                "description": "Read a file from the workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                },
            }}])
        );
        assert_eq!(json["tool_choice"], "auto");
    }

    #[test]
    fn an_assistant_message_that_only_calls_tools_keeps_empty_text() {
        let message = ChatMessage::assistant_tool_calls(
            "",
            vec![
                ChatToolCall {
                    id: "call_a".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"a.txt"}"#.to_string(),
                },
                ChatToolCall {
                    id: "call_b".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"b.txt"}"#.to_string(),
                },
            ],
        );
        let json = serde_json::to_value(&message).unwrap();
        // The local server accepts a string, empty included, or null for an
        // assistant message with tool calls; text is always sent.
        assert_eq!(
            json,
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {"id": "call_a", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}},
                    {"id": "call_b", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"path\":\"b.txt\"}"}},
                ],
            })
        );
        assert_eq!(
            serde_json::from_value::<ChatMessage>(json).unwrap(),
            message
        );
    }

    #[test]
    fn a_tool_result_names_the_call_it_answers() {
        let message = ChatMessage::tool_result("call_a", "hello from a.txt");
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json,
            json!({"role": "tool", "content": "hello from a.txt", "tool_call_id": "call_a"})
        );
        assert_eq!(
            serde_json::from_value::<ChatMessage>(json).unwrap(),
            message
        );
    }

    #[test]
    fn null_or_missing_content_reads_as_empty() {
        for json in [
            json!({"role": "assistant", "content": null, "tool_calls": []}),
            json!({"role": "assistant"}),
        ] {
            let message: ChatMessage = serde_json::from_value(json).unwrap();
            assert_eq!(message.content, "");
            assert!(message.tool_calls.is_empty());
        }
    }

    #[test]
    fn embed_request_serializes_input_as_a_list() {
        let req = EmbedRequest {
            model: Some("m".to_string()),
            input: vec!["a".to_string(), "b".to_string()],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "m");
        assert_eq!(json["input"].as_array().unwrap().len(), 2);
    }
}
