// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Model references and metadata.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::chat::ModelInfo;

/// Stable identifier of a model, for example `phi-3-mini-4k-instruct-q4`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelRef(String);

impl ModelRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a model can be asked to do. One running model server serves exactly
/// one of these (see docs/spec.md, "One server, one job").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Chat,
    Embeddings,
}

impl Capability {
    /// Stable string form, as used in [`ModelInfo::capabilities`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::Chat => "chat",
            Capability::Embeddings => "embeddings",
        }
    }
}

/// The license a model is distributed under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    /// SPDX identifier where one exists, otherwise the license's own name.
    pub name: String,
    /// Whether the license allows commercial use. The model library lists
    /// only models for which this is true.
    pub commercial_use: bool,
    /// Where the license text can be read.
    pub url: String,
}

/// What is known about a model before it is loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub id: ModelRef,
    /// Name shown to a person.
    pub name: String,
    /// Size of the model file on disk.
    pub size_bytes: u64,
    pub context_length: Option<u32>,
    pub capabilities: Vec<Capability>,
    pub license: License,
}

impl ModelMetadata {
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// The provider-facing listing entry for this model.
    pub fn to_info(&self) -> ModelInfo {
        ModelInfo {
            id: self.id.as_str().to_string(),
            context_length: self.context_length,
            capabilities: self
                .capabilities
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ModelMetadata {
        ModelMetadata {
            id: ModelRef::new("tiny-chat"),
            name: "Tiny Chat".to_string(),
            size_bytes: 1024,
            context_length: Some(4096),
            capabilities: vec![Capability::Chat],
            license: License {
                name: "MIT".to_string(),
                commercial_use: true,
                url: "https://example.invalid/license".to_string(),
            },
        }
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let meta = sample();
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"id\":\"tiny-chat\""));
        assert!(json.contains("\"capabilities\":[\"chat\"]"));
        let back: ModelMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn capabilities_answer_supports_and_map_to_the_listing() {
        let meta = sample();
        assert!(meta.supports(Capability::Chat));
        assert!(!meta.supports(Capability::Embeddings));
        let info = meta.to_info();
        assert_eq!(info.id, "tiny-chat");
        assert_eq!(info.context_length, Some(4096));
        assert_eq!(info.capabilities, vec!["chat".to_string()]);
        assert_eq!(Capability::Embeddings.as_str(), "embeddings");
        assert_eq!(meta.id.to_string(), "tiny-chat");
    }
}
