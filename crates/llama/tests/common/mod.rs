// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared helpers for the llama supervisor tests.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use isonomic_inference_core::{FixedGate, ManualClock, RamGate};
use isonomic_inference_testkit::{write_model, write_script, FakeModelServer};
use isonomic_llama::{LlamaConfig, LlamaSupervisor, ServiceState};

/// A stand-in that stays alive until it is told to stop.
pub const SLEEPER: &str = "#!/bin/sh\nexec /bin/sleep 3600\n";
/// A stand-in that dies at once.
pub const CRASHER: &str = "#!/bin/sh\necho starting\nexit 1\n";

/// Production timings, on a clock the test controls. The fake server already
/// owns a port, so the port seam is used.
pub fn test_config(binary: PathBuf, model: PathBuf, port: u16) -> LlamaConfig {
    let mut config = LlamaConfig::new(binary, model);
    config.port = Some(port);
    config.idle_unload = None;
    config
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub server: FakeModelServer,
    pub clock: Arc<ManualClock>,
}

impl Rig {
    pub async fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            server: FakeModelServer::start().await,
            clock: Arc::new(ManualClock::new()),
        }
    }

    pub fn config(&self, script: &str) -> LlamaConfig {
        let bin = write_script(self.dir.path(), "stand-in", script);
        let model = write_model(self.dir.path());
        test_config(bin, model, self.server.port())
    }

    pub fn supervisor(&self, config: LlamaConfig) -> LlamaSupervisor {
        self.supervisor_with_gate(config, Arc::new(FixedGate::unlimited()))
    }

    pub fn supervisor_with_gate(
        &self,
        config: LlamaConfig,
        gate: Arc<dyn RamGate>,
    ) -> LlamaSupervisor {
        LlamaSupervisor::with_clock(config, gate, self.clock.clone())
    }
}

/// A loopback port with nothing listening on it.
pub fn dead_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Wait for the supervisor to reach `want`. The clock is not moved; the wait
/// is bounded only as a safety net.
pub async fn wait_for_state(sup: &LlamaSupervisor, want: ServiceState) {
    let mut rx = sup.subscribe();
    let limit = Duration::from_secs(30);
    let reached = tokio::time::timeout(limit, rx.wait_for(|s| *s == want))
        .await
        .is_ok_and(|seen| seen.is_ok());
    assert!(
        reached,
        "did not reach {want:?}; state is {:?}",
        sup.state()
    );
}
