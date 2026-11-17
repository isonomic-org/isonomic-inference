// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The manifest of known models.
//!
//! The data lives in `models.json` next to this crate's manifest and is
//! compiled in. Every entry records where the file comes from, its SHA-256,
//! its size, what it can do and its license. An entry whose license does not
//! allow commercial use is rejected when the manifest is loaded.

use std::collections::HashSet;

use isonomic_inference_core::{Capability, ModelMetadata, ModelRef};
use serde::{Deserialize, Serialize};

use crate::download::ModelSpec;

/// Ceiling for the default chat model: it must fit the smallest hardware
/// profile's budget (up to 4 GB).
pub const DEFAULT_MODEL_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

const BUILTIN: &str = include_str!("../models.json");

/// One known model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    #[serde(flatten)]
    pub meta: ModelMetadata,
    /// File name under the models directory.
    pub file_name: String,
    /// Direct download URL.
    pub source_url: String,
    /// SHA-256 of the file, lowercase hex.
    pub sha256: String,
    /// True until the pinned hash has been confirmed against a real download.
    /// The hash is enforced either way; the flag only says how much the pin
    /// has been proven.
    #[serde(default)]
    pub draft: bool,
}

impl ModelEntry {
    /// What the downloader needs to fetch and verify this model.
    pub fn spec(&self) -> ModelSpec {
        ModelSpec {
            file_name: self.file_name.clone(),
            url: self.source_url.clone(),
            sha256_hex: self.sha256.clone(),
            size_bytes: self.meta.size_bytes,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    default_chat: ModelRef,
    #[serde(default)]
    default_embeddings: Option<ModelRef>,
    models: Vec<ModelEntry>,
}

/// The list of known models and which ones are the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    entries: Vec<ModelEntry>,
    default_chat: ModelRef,
    default_embeddings: Option<ModelRef>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest is not valid JSON: {0}")]
    Parse(String),
    #[error("model {0} is listed twice")]
    Duplicate(String),
    #[error("model {id}: {problem}")]
    Invalid { id: String, problem: String },
    #[error("model {0} has a license that does not allow commercial use")]
    NonCommercialLicense(String),
    #[error("default model {0} is not in the manifest")]
    UnknownDefault(String),
    #[error("default model {id} cannot do {capability}")]
    DefaultLacksCapability { id: String, capability: String },
}

impl Manifest {
    /// The manifest compiled into this crate.
    pub fn builtin() -> Self {
        Self::from_json(BUILTIN).expect("the built-in manifest is checked by this crate's tests")
    }

    /// Parse and validate a manifest.
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        let file: ManifestFile =
            serde_json::from_str(json).map_err(|e| ManifestError::Parse(e.to_string()))?;
        let mut seen = HashSet::new();
        for entry in &file.models {
            validate(entry)?;
            if !seen.insert(entry.meta.id.clone()) {
                return Err(ManifestError::Duplicate(entry.meta.id.to_string()));
            }
        }
        let manifest = Self {
            entries: file.models,
            default_chat: file.default_chat,
            default_embeddings: file.default_embeddings,
        };
        manifest.check_default(&manifest.default_chat, Capability::Chat)?;
        if let Some(id) = &manifest.default_embeddings {
            manifest.check_default(id, Capability::Embeddings)?;
        }
        Ok(manifest)
    }

    fn check_default(&self, id: &ModelRef, capability: Capability) -> Result<(), ManifestError> {
        let entry = self
            .get(id)
            .ok_or_else(|| ManifestError::UnknownDefault(id.to_string()))?;
        if !entry.meta.supports(capability) {
            return Err(ManifestError::DefaultLacksCapability {
                id: id.to_string(),
                capability: capability.as_str().to_string(),
            });
        }
        Ok(())
    }

    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    pub fn get(&self, id: &ModelRef) -> Option<&ModelEntry> {
        self.entries.iter().find(|e| &e.meta.id == id)
    }

    /// Every model that can do `capability`.
    pub fn with_capability(&self, capability: Capability) -> impl Iterator<Item = &ModelEntry> {
        self.entries
            .iter()
            .filter(move |e| e.meta.supports(capability))
    }

    /// The default chat model.
    pub fn default_chat(&self) -> &ModelEntry {
        self.get(&self.default_chat)
            .expect("validated when the manifest was loaded")
    }

    /// The default embedding model, if the manifest names one.
    pub fn default_embeddings(&self) -> Option<&ModelEntry> {
        self.default_embeddings.as_ref().and_then(|id| self.get(id))
    }
}

fn validate(entry: &ModelEntry) -> Result<(), ManifestError> {
    let id = entry.meta.id.to_string();
    let invalid = |problem: &str| ManifestError::Invalid {
        id: id.clone(),
        problem: problem.to_string(),
    };
    if !entry.meta.license.commercial_use {
        return Err(ManifestError::NonCommercialLicense(id));
    }
    if entry.meta.license.name.trim().is_empty() {
        return Err(invalid("the license has no name"));
    }
    let hex_ok = entry.sha256.len() == 64
        && entry
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex_ok {
        return Err(invalid("sha256 must be 64 lowercase hex characters"));
    }
    if !entry.source_url.starts_with("https://") {
        return Err(invalid("the source URL must use https"));
    }
    if entry.meta.size_bytes == 0 {
        return Err(invalid("the size is zero"));
    }
    if entry.meta.capabilities.is_empty() {
        return Err(invalid("no capabilities are listed"));
    }
    let name = &entry.file_name;
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err(invalid("the file name must be a plain file name"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(extra: &str, commercial: bool, sha: &str, url: &str) -> String {
        format!(
            r#"{{"default_chat":"m","models":[{{"id":"m","name":"M","file_name":"m.gguf",
            "source_url":"{url}","sha256":"{sha}","size_bytes":10,"context_length":null,
            "capabilities":["chat"],"license":{{"name":"MIT","commercial_use":{commercial},
            "url":"https://example.invalid/l"}}{extra}}}]}}"#
        )
    }

    fn good_sha() -> String {
        "a".repeat(64)
    }

    #[test]
    fn the_builtin_manifest_is_valid_and_records_every_license() {
        let manifest = Manifest::builtin();
        assert!(!manifest.entries().is_empty());
        for entry in manifest.entries() {
            assert!(entry.meta.license.commercial_use, "{}", entry.meta.id);
            assert!(!entry.meta.license.name.is_empty());
            assert!(entry.meta.license.url.starts_with("https://"));
            assert_eq!(entry.spec().sha256_hex.len(), 64);
            assert_eq!(entry.spec().size_bytes, entry.meta.size_bytes);
        }
    }

    #[test]
    fn the_defaults_exist_can_do_their_job_and_fit_the_smallest_profile() {
        let manifest = Manifest::builtin();
        let chat = manifest.default_chat();
        assert!(chat.meta.supports(Capability::Chat));
        assert!(chat.meta.size_bytes <= DEFAULT_MODEL_MAX_BYTES);
        let embed = manifest.default_embeddings().expect("an embedding default");
        assert!(embed.meta.supports(Capability::Embeddings));
        assert!(manifest.with_capability(Capability::Chat).count() >= 1);
        assert!(manifest.get(&ModelRef::new("no-such-model")).is_none());
    }

    #[test]
    fn a_license_without_commercial_use_is_rejected() {
        let json = one("", false, &good_sha(), "https://example.invalid/m.gguf");
        assert_eq!(
            Manifest::from_json(&json).unwrap_err(),
            ManifestError::NonCommercialLicense("m".to_string())
        );
    }

    #[test]
    fn a_bad_hash_or_a_plain_http_source_is_rejected() {
        let bad_hash = one("", true, "ABC", "https://example.invalid/m.gguf");
        assert!(matches!(
            Manifest::from_json(&bad_hash).unwrap_err(),
            ManifestError::Invalid { .. }
        ));
        let plain = one("", true, &good_sha(), "http://example.invalid/m.gguf");
        assert!(matches!(
            Manifest::from_json(&plain).unwrap_err(),
            ManifestError::Invalid { .. }
        ));
    }

    #[test]
    fn draft_defaults_to_false_and_is_read_when_present() {
        let plain = one("", true, &good_sha(), "https://example.invalid/m.gguf");
        assert!(!Manifest::from_json(&plain).unwrap().default_chat().draft);
        let draft = one(
            r#","draft":true"#,
            true,
            &good_sha(),
            "https://example.invalid/m.gguf",
        );
        assert!(Manifest::from_json(&draft).unwrap().default_chat().draft);
    }

    #[test]
    fn duplicates_and_unusable_defaults_are_rejected() {
        let entry = r#"{"id":"m","name":"M","file_name":"m.gguf",
            "source_url":"https://example.invalid/m.gguf","sha256":"SHA","size_bytes":10,
            "context_length":null,"capabilities":["embeddings"],
            "license":{"name":"MIT","commercial_use":true,"url":"https://example.invalid/l"}}"#
            .replace("SHA", &good_sha());
        let twice = format!(r#"{{"default_chat":"m","models":[{entry},{entry}]}}"#);
        assert_eq!(
            Manifest::from_json(&twice).unwrap_err(),
            ManifestError::Duplicate("m".to_string())
        );
        let wrong_job = format!(r#"{{"default_chat":"m","models":[{entry}]}}"#);
        assert!(matches!(
            Manifest::from_json(&wrong_job).unwrap_err(),
            ManifestError::DefaultLacksCapability { .. }
        ));
        let missing = format!(r#"{{"default_chat":"other","models":[{entry}]}}"#);
        assert_eq!(
            Manifest::from_json(&missing).unwrap_err(),
            ManifestError::UnknownDefault("other".to_string())
        );
        assert!(matches!(
            Manifest::from_json("not json").unwrap_err(),
            ManifestError::Parse(_)
        ));
    }
}
