// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The frozen `InferenceProvider` trait: one trait, N providers,
//! OpenAI-compatible wire format end to end.
//! Do not change the trait surface. Providers plug in behind it.

use std::pin::Pin;

use futures_core::Stream;
use isonomic_inference_core::{
    ChatChunk, ChatRequest, CostEstimate, EmbedRequest, Embeddings, ModelInfo, ProviderHealth,
    ProviderId,
};

/// Token stream: the wire format is OpenAI SSE, parsed into `ChatChunk`s.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatChunk, GatewayError>> + Send>>;

/// Typed gateway errors: a caller can render each state directly.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("model file missing at {path}; download it first")]
    ModelMissing { path: String },
    #[error("model still downloading: {received_bytes}/{total_bytes} bytes")]
    ModelDownloading {
        received_bytes: u64,
        total_bytes: u64,
    },
    #[error("insufficient RAM: {reason}")]
    InsufficientRam { reason: String },
    #[error("llama-server crashed; last log lines attached")]
    ServerCrashed { last_logs: Vec<String> },
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

#[async_trait::async_trait]
pub trait InferenceProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn models(&self) -> Vec<ModelInfo>;
    async fn health(&self) -> ProviderHealth;
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, GatewayError>;
    async fn embeddings(&self, req: EmbedRequest) -> Result<Embeddings, GatewayError>;
    fn cost(&self, req: &ChatRequest) -> CostEstimate;
}
