// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The memory gate: the seam consulted before a model server is started.
//!
//! This repo is the gate's consumer, so the trait and the denial type live
//! here. The real, hardware-aware gate lives in the engine, which injects it.
//! Only a simple fixed-budget implementation ships in this repo, for tests
//! and tools.

use std::fmt;

/// Why the gate refused to admit a model load. Surfaced as is, so a refusal
/// always has a clear reason (refuse rather than thrash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamDenial {
    /// Bytes the load needs, including any headroom the gate adds.
    pub required_bytes: u64,
    /// Bytes available at decision time.
    pub available_bytes: u64,
    /// Human-readable reason for the refusal.
    pub reason: String,
}

impl fmt::Display for RamDenial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (required {} bytes, available {} bytes)",
            self.reason, self.required_bytes, self.available_bytes
        )
    }
}

/// The seam consulted before starting a model-hosting worker.
pub trait RamGate: Send + Sync {
    /// May a model with this on-disk size be loaded right now?
    fn admit(&self, model_bytes: u64) -> Result<(), RamDenial>;
}

/// A gate with a fixed budget: admits a model when its file fits. For tests
/// and tools only; it knows nothing about the machine.
#[derive(Debug, Clone, Copy)]
pub struct FixedGate {
    available_bytes: u64,
}

impl FixedGate {
    /// Admit any model whose file is at most `available_bytes`.
    pub fn with_available(available_bytes: u64) -> Self {
        Self { available_bytes }
    }

    /// Admit everything.
    pub fn unlimited() -> Self {
        Self {
            available_bytes: u64::MAX,
        }
    }
}

impl RamGate for FixedGate {
    fn admit(&self, model_bytes: u64) -> Result<(), RamDenial> {
        if model_bytes <= self.available_bytes {
            return Ok(());
        }
        Err(RamDenial {
            required_bytes: model_bytes,
            available_bytes: self.available_bytes,
            reason: "not enough free memory to load the model without thrashing".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_that_fits_is_admitted() {
        let gate = FixedGate::with_available(100);
        assert!(gate.admit(100).is_ok());
        assert!(FixedGate::unlimited().admit(u64::MAX).is_ok());
    }

    #[test]
    fn a_refusal_carries_required_available_and_a_readable_reason() {
        let gate = FixedGate::with_available(100);
        let denial = gate.admit(101).unwrap_err();
        assert_eq!(denial.required_bytes, 101);
        assert_eq!(denial.available_bytes, 100);
        assert!(denial.reason.contains("not enough free memory"));
        let shown = denial.to_string();
        assert!(shown.contains("required 101 bytes"));
        assert!(shown.contains("available 100 bytes"));
    }

    #[test]
    fn the_trait_is_usable_as_a_shared_object() {
        let gate: std::sync::Arc<dyn RamGate> = std::sync::Arc::new(FixedGate::with_available(1));
        assert!(gate.admit(2).is_err());
    }
}
