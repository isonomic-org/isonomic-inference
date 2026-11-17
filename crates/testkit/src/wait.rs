// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Waiting helpers. The supervised code waits on an injected clock; tests
//! move that clock explicitly. The only real time used here is a safety limit
//! on how long a test waits for the operating system (a process to start or
//! die) before it gives up and fails.

use std::time::{Duration, Instant};

use isonomic_inference_core::ManualClock;

/// How long a test waits for the operating system before failing.
pub const SAFETY_LIMIT: Duration = Duration::from_secs(30);

const POLL: Duration = Duration::from_millis(2);

/// Wait until `condition` holds, without moving any clock.
pub async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let give_up = Instant::now() + SAFETY_LIMIT;
    while !condition() {
        assert!(Instant::now() < give_up, "timed out waiting for {what}");
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until at least `count` tasks are sleeping on `clock`. Call this
/// before `advance` so the sleep being released is known to be registered.
pub async fn wait_for_sleepers(clock: &ManualClock, count: usize) {
    wait_until("tasks to reach their sleep", || clock.sleepers() >= count).await;
}

/// Advance `clock` by `step` repeatedly until `condition` holds. For tests
/// that only need time to pass, not an exact amount of it.
pub async fn pump_until(
    what: &str,
    clock: &ManualClock,
    step: Duration,
    mut condition: impl FnMut() -> bool,
) {
    let give_up = Instant::now() + SAFETY_LIMIT;
    while !condition() {
        assert!(Instant::now() < give_up, "timed out waiting for {what}");
        clock.advance(step);
        tokio::time::sleep(POLL).await;
    }
}
