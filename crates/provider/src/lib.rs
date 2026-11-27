// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The inference provider trait and the local provider.
//!
//! Spec: docs/spec.md. Status and deviations: docs/status.md.
//!
//! One trait, N providers, OpenAI-compatible wire format end to end. This
//! repo ships the trait and the local provider. The router, remote providers
//! and the HTTP endpoint belong to the engine's model gateway.

pub mod local;
pub mod provider;
mod sse;

pub use isonomic_inference_core::{
    ChatChunk, ChatMessage, ChatRequest, ChatTool, ChatToolCall, CostEstimate, EmbedRequest,
    Embeddings, ModelInfo, ProviderHealth, ProviderId, ReasoningChoice, ToolCallDelta,
};
pub use local::{LocalLlama, ReasoningPolicy};
pub use provider::{ChatStream, GatewayError, InferenceProvider};
