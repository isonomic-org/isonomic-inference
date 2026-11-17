// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! The llama.cpp server as a supervised model worker.
//!
//! Spec: docs/spec.md. Status and deviations: docs/status.md.
//!
//! The lifecycle (lazy start, probes, restart with backoff, the breaker, idle
//! unload, the memory gate, the allow-listed environment, the process group)
//! is `isonomic-worker`. This crate supplies what is specific to the
//! llama.cpp server: its command line, its health endpoint, its API key
//! variable and its one-job-per-instance rule.

mod config;

use std::ops::Deref;
use std::sync::Arc;

use isonomic_inference_core::{Clock, RamGate, SystemClock};
use isonomic_worker::WorkerSupervisor;

pub use config::{
    ApiKeyMode, LlamaConfig, Reasoning, ServerMode, API_KEY_ENV, DEFAULT_CONTEXT_SIZE,
    ENV_ALLOW_LIST, HEALTH_PATH, RESERVED_ARGS, WORKER_NAME,
};
pub use isonomic_worker::{
    probe_http, restart_backoff_secs, should_trip_breaker, Endpoint, ServiceState, BREAKER_WINDOW,
    LOG_RING_CAPACITY,
};

/// Errors from starting or running the server. The same type every worker
/// uses.
pub type LlamaError = isonomic_worker::WorkerError;

/// Supervises a single llama.cpp server worker. Dropping it tears the
/// process group down. Everything the worker supervisor offers is available
/// on it directly.
pub struct LlamaSupervisor {
    config: LlamaConfig,
    worker: WorkerSupervisor,
}

impl LlamaSupervisor {
    pub fn new(config: LlamaConfig, gate: Arc<dyn RamGate>) -> Self {
        Self::with_clock(config, gate, Arc::new(SystemClock))
    }

    /// Like [`new`](Self::new), with an injected clock. Every wait the
    /// supervisor makes goes through it.
    pub fn with_clock(config: LlamaConfig, gate: Arc<dyn RamGate>, clock: Arc<dyn Clock>) -> Self {
        let worker = WorkerSupervisor::with_clock(config.to_spec(), gate, clock);
        Self { config, worker }
    }

    pub fn config(&self) -> &LlamaConfig {
        &self.config
    }
}

impl Deref for LlamaSupervisor {
    type Target = WorkerSupervisor;

    fn deref(&self) -> &WorkerSupervisor {
        &self.worker
    }
}
