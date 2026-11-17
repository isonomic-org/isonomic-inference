// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared types for Isonomic Inference.
//!
//! Spec: docs/spec.md. Status and deviations: docs/status.md.
//!
//! This crate has no process, network or file-system code. It holds the types
//! every other crate in the workspace agrees on: model references and
//! metadata, the chat and embedding wire types, the memory gate trait and its
//! denial type, and injectable clocks.

pub mod chat;
pub mod clock;
pub mod gate;
pub mod model;

pub use chat::{
    ChatChunk, ChatMessage, ChatRequest, CostEstimate, EmbedRequest, Embeddings, ModelInfo,
    ProviderHealth, ProviderId, ReasoningChoice,
};
pub use clock::{timeout, Clock, ManualClock, Sleep, SystemClock};
pub use gate::{FixedGate, RamDenial, RamGate};
pub use model::{Capability, License, ModelMetadata, ModelRef};
