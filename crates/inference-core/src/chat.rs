//! Chat and embedding request and response types.
//!
//! These are the data types the frozen provider trait speaks (see the
//! `provider` crate). The wire format is OpenAI-compatible end to end. Their
//! shape is carried over from the earlier build with one approved change
//! (Decision 4, #28): reasoning on streamed chunks, with a per-request
//! control. Do not change them without asking.

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
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
}

/// One streamed delta from a chat completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatChunk {
    /// Answer text.
    pub delta: String,
    /// Reasoning text, streamed apart from the answer, when the model
    /// reasons and the server reports it.
    pub reasoning: Option<String>,
    pub finish_reason: Option<String>,
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

    #[test]
    fn chat_request_omits_unset_fields_on_the_wire() {
        let req = ChatRequest {
            model: None,
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            temperature: None,
            max_tokens: Some(16),
            reasoning: Some(ReasoningChoice::On),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("model").is_none());
        assert!(json.get("reasoning").is_none(), "providers translate it");
        assert!(json.get("temperature").is_none());
        assert_eq!(json["max_tokens"], 16);
        assert_eq!(json["messages"][0]["role"], "user");
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
