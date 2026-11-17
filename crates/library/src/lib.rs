// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Isonomic model library.
//!
//! Spec: docs/spec.md. Status and deviations: docs/status.md.
//!
//! Two parts: a manifest of known models ([`Manifest`]) and a download that
//! verifies what it fetched before anyone can use it ([`ensure_model`]).

pub mod download;
pub mod manifest;

pub use download::{ensure_model, ensure_model_with, DownloadProgress, ModelError, ModelSpec};
pub use manifest::{Manifest, ManifestError, ModelEntry};
