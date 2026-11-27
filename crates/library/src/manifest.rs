// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The manifest of known models.
//!
//! The data lives in `models.json` next to this crate's manifest, with more
//! entries in `models/*.json` (split to keep each file small), and is
//! compiled in. Every entry records where the file comes from, its SHA-256,
//! its size, what it can do and its license. An entry whose license does not
//! allow commercial use is rejected when the manifest is loaded.

use std::collections::{BTreeMap, HashSet};

use isonomic_inference_core::{Capability, ModelMetadata, ModelRef};
use serde::{Deserialize, Serialize};

use crate::download::ModelSpec;

/// Ceiling for the default chat model: it must fit the smallest hardware
/// profile's budget (up to 4 GB).
pub const DEFAULT_MODEL_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

const BUILTIN: &str = include_str!("../models.json");

/// The other built-in files: `models` lists only, no defaults.
const BUILTIN_MORE: [&str; 4] = [
    include_str!("../models/google.json"),
    include_str!("../models/mistral.json"),
    include_str!("../models/more.json"),
    include_str!("../models/qwen.json"),
];

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
    /// Who publishes the weights, for a list row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// One line about the model, for a list row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// The least memory, in GB, of a machine it is recommended on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ram_gb: Option<u32>,
    /// What it is good at: `tools`, `vision`, `code`, `reasoning`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
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
    /// The default chat model by the machine's memory in GB, as text keys.
    #[serde(default)]
    default_chat_by_ram: BTreeMap<String, ModelRef>,
    models: Vec<ModelEntry>,
}

/// A further file of entries.
#[derive(Debug, Deserialize)]
struct MoreFile {
    models: Vec<ModelEntry>,
}

/// The list of known models and which ones are the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    entries: Vec<ModelEntry>,
    default_chat: ModelRef,
    default_embeddings: Option<ModelRef>,
    /// (memory in GB, model), smallest memory first.
    default_chat_by_ram: Vec<(u32, ModelRef)>,
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
        Self::from_parts(BUILTIN, &BUILTIN_MORE)
            .expect("the built-in manifest is checked by this crate's tests")
    }

    /// Parse and validate a manifest.
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        Self::from_parts(json, &[])
    }

    /// Parse and validate a manifest and further files of entries, which
    /// hold `models` only. An id listed twice anywhere is refused.
    pub fn from_parts(json: &str, more: &[&str]) -> Result<Self, ManifestError> {
        let parse = |e: serde_json::Error| ManifestError::Parse(e.to_string());
        let mut file: ManifestFile = serde_json::from_str(json).map_err(parse)?;
        for part in more {
            let extra: MoreFile = serde_json::from_str(part).map_err(parse)?;
            file.models.extend(extra.models);
        }
        let mut seen = HashSet::new();
        for entry in &file.models {
            validate(entry)?;
            if !seen.insert(entry.meta.id.clone()) {
                return Err(ManifestError::Duplicate(entry.meta.id.to_string()));
            }
        }
        let mut by_ram = Vec::new();
        for (gb, id) in file.default_chat_by_ram {
            let gb = gb.parse::<u32>().map_err(|_| ManifestError::Invalid {
                id: id.to_string(),
                problem: format!("default_chat_by_ram key {gb:?} is not a whole number of GB"),
            })?;
            by_ram.push((gb, id));
        }
        by_ram.sort_by_key(|(gb, _)| *gb);
        let manifest = Self {
            entries: file.models,
            default_chat: file.default_chat,
            default_embeddings: file.default_embeddings,
            default_chat_by_ram: by_ram,
        };
        manifest.check_default(&manifest.default_chat, Capability::Chat)?;
        for (_, id) in &manifest.default_chat_by_ram {
            manifest.check_default(id, Capability::Chat)?;
        }
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

    /// The default chat model for a machine with `ram_gb` of memory: the
    /// entry for the largest memory at or below it, else
    /// [`default_chat`](Self::default_chat).
    pub fn default_chat_for_ram(&self, ram_gb: u32) -> &ModelEntry {
        self.default_chat_by_ram
            .iter()
            .rev()
            .find(|(gb, _)| *gb <= ram_gb)
            .and_then(|(_, id)| self.get(id))
            .unwrap_or_else(|| self.default_chat())
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

    #[test]
    fn the_builtin_catalogue_loads_every_file_with_its_row_fields() {
        let m = Manifest::builtin();
        let chat = m.with_capability(Capability::Chat).count();
        assert!(chat >= 30, "{chat} chat models");
        assert!(m
            .entries()
            .iter()
            .all(|e| e.publisher.is_some() && e.about.is_some()));
        assert!(m.entries().iter().all(|e| e.min_ram_gb.is_some()));
    }

    #[test]
    fn the_default_follows_memory_and_falls_back() {
        let m = Manifest::builtin();
        assert_eq!(m.default_chat_for_ram(4).meta.id, m.default_chat().meta.id);
        let at = |gb| m.default_chat_for_ram(gb).meta.id.to_string();
        assert_eq!(at(8), "qwen3.5-4b-q4-k-m");
        assert_eq!(at(12), "qwen3.5-4b-q4-k-m");
        assert_eq!(at(16), "qwen3.5-9b-q4-k-m");
        assert_eq!(at(128), "qwen3.6-35b-a3b-ud-q4-k-xl");
    }

    #[test]
    fn an_id_in_two_files_is_refused() {
        let base = one("", true, &good_sha(), "https://example.invalid/m.gguf");
        let models = &base[base.find(r#""models""#).unwrap()..];
        let more = format!("{{{models}");
        assert!(matches!(
            Manifest::from_parts(&base, &[&more]),
            Err(ManifestError::Duplicate(_))
        ));
    }
}
