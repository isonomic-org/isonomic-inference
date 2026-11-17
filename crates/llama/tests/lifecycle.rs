// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Lifecycle of the supervised worker: lazy start, the memory gate, crash
//! recovery on the backoff schedule, the breaker, idle unload, hung-server
//! detection and teardown. Time is a clock the tests move by hand.
#![cfg(unix)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{dead_port, wait_for_state, Rig, CRASHER, SLEEPER};
use isonomic_inference_core::FixedGate;
use isonomic_inference_testkit::{process_gone, pump_until, wait_for_sleepers, wait_until};
use isonomic_llama::{LlamaError, ServiceState, LOG_RING_CAPACITY};

const SECOND: Duration = Duration::from_secs(1);
/// Longer than any test moves the clock.
const NEVER: Duration = Duration::from_secs(100 * 24 * 3600);

fn kill(pid: u32) {
    let status = std::process::Command::new("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

fn crashes_seen(sup: &isonomic_llama::LlamaSupervisor) -> usize {
    let logs = sup.last_logs();
    logs.iter()
        .filter(|l| l.contains("exited unexpectedly"))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn lazy_start_reaches_healthy_and_exposes_state() {
    let rig = Rig::new().await;
    let sup = rig.supervisor(rig.config(SLEEPER));
    // Lazy: nothing runs until the first request.
    assert_eq!(sup.state(), ServiceState::Stopped);
    assert!(sup.pid().await.is_none());
    let endpoint = sup.ensure_running().await.unwrap();
    assert_eq!(endpoint.addr.port(), rig.server.port());
    assert!(endpoint.addr.ip().is_loopback());
    assert_eq!(sup.state(), ServiceState::Healthy);
    assert!(sup.pid().await.is_some());
    // A second request reuses the running worker.
    let pid = sup.pid().await;
    assert_eq!(sup.ensure_running().await.unwrap(), endpoint);
    assert_eq!(sup.pid().await, pid);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_too_large_for_the_gate_is_refused_and_nothing_starts() {
    let rig = Rig::new().await;
    // The stand-in model file is 4 bytes; the gate has room for 3.
    let sup = rig.supervisor_with_gate(rig.config(SLEEPER), Arc::new(FixedGate::with_available(3)));
    let err = sup.ensure_running().await.unwrap_err();
    match err {
        LlamaError::RamDenied(denial) => {
            assert_eq!(denial.required_bytes, 4);
            assert_eq!(denial.available_bytes, 3);
            assert!(denial.reason.contains("not enough free memory"));
        }
        other => panic!("expected RamDenied, got {other:?}"),
    }
    // Refused before any spawn: still stopped, no process, no probe.
    assert_eq!(sup.state(), ServiceState::Stopped);
    assert!(sup.pid().await.is_none());
    assert!(rig.server.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_9_restarts_on_the_backoff_schedule() {
    let rig = Rig::new().await;
    let sup = rig.supervisor(rig.config(SLEEPER));
    sup.ensure_running().await.unwrap();
    let pid = sup.pid().await.unwrap();
    kill(pid);

    // The monitor notices the crash and waits out the first backoff (1 s).
    wait_for_state(&sup, ServiceState::Starting).await;
    wait_for_sleepers(&rig.clock, 2).await; // steady probe + backoff
    assert_eq!(sup.restart_total(), 0);
    rig.clock.advance(Duration::from_millis(999));
    assert_eq!(
        sup.state(),
        ServiceState::Starting,
        "restarted before the backoff ran out"
    );
    assert_eq!(sup.restart_total(), 0);

    rig.clock.advance(Duration::from_millis(1));
    wait_for_state(&sup, ServiceState::Healthy).await;
    assert_eq!(sup.restart_total(), 1);
    let new_pid = sup.pid().await.unwrap();
    assert_ne!(new_pid, pid);
}

#[tokio::test(flavor = "multi_thread")]
async fn breaker_trips_to_degraded_with_logs_after_repeated_crashes() {
    let rig = Rig::new().await;
    let mut config = rig.config(CRASHER);
    config.backoff = |_| SECOND;
    let sup = rig.supervisor(config);
    // The fake server answers the probe, so each launch looks healthy and
    // then the stand-in is found dead.
    sup.ensure_running().await.unwrap();
    for crash in 1..=5 {
        // Wait for this crash to be recorded and its backoff to begin.
        wait_until("the crash to be recorded", || {
            crashes_seen(&sup) == crash && sup.state() == ServiceState::Starting
        })
        .await;
        wait_for_sleepers(&rig.clock, 2).await; // steady probe + backoff
        rig.clock.advance(SECOND);
    }
    // The sixth crash inside ten minutes trips the breaker.
    wait_for_state(&sup, ServiceState::Degraded).await;
    assert_eq!(sup.restart_total(), 5);
    let logs = sup.last_logs();
    assert!(!logs.is_empty() && logs.len() <= LOG_RING_CAPACITY);
    assert!(logs.iter().any(|l| l.contains("circuit breaker tripped")));
    assert!(logs.iter().any(|l| l.contains("[stdout] starting")));
    assert!(matches!(
        sup.ensure_running().await.unwrap_err(),
        LlamaError::Degraded
    ));
    // reset() clears the breaker so a lazy start can be attempted again.
    sup.reset().await;
    assert_eq!(sup.state(), ServiceState::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_unload_stops_without_counting_a_crash_and_restarts_lazily() {
    let rig = Rig::new().await;
    let mut config = rig.config(SLEEPER);
    config.idle_unload = Some(Duration::from_secs(300));
    config.steady_probe_interval = NEVER; // keep the steady probe out of the way
    let sup = rig.supervisor(config);
    sup.ensure_running().await.unwrap();
    let pid = sup.pid().await.unwrap();

    // The idle watcher checks every half timeout. Halfway: still loaded.
    wait_for_sleepers(&rig.clock, 2).await; // idle watcher (+ the parked steady probe)
    rig.clock.advance(Duration::from_secs(150));
    wait_for_sleepers(&rig.clock, 2).await;
    assert_eq!(sup.state(), ServiceState::Healthy);

    // The full timeout with no activity: unloaded.
    rig.clock.advance(Duration::from_secs(150));
    wait_for_state(&sup, ServiceState::Stopped).await;
    assert_eq!(
        sup.restart_total(),
        0,
        "idle unload must not count as a crash"
    );
    assert!(sup.pid().await.is_none());
    wait_until("the unloaded worker to exit", || process_gone(pid)).await;

    // And the next request lazily brings it back.
    sup.ensure_running().await.unwrap();
    assert_eq!(sup.state(), ServiceState::Healthy);
    assert_ne!(sup.pid().await, Some(pid));
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_keeps_the_model_loaded() {
    let rig = Rig::new().await;
    let mut config = rig.config(SLEEPER);
    config.idle_unload = Some(Duration::from_secs(300));
    config.steady_probe_interval = NEVER; // keep the steady probe out of the way
    let sup = rig.supervisor(config);
    sup.ensure_running().await.unwrap();

    wait_for_sleepers(&rig.clock, 2).await;
    rig.clock.advance(Duration::from_secs(150));
    wait_for_sleepers(&rig.clock, 2).await;
    sup.touch(); // activity at 150 s

    // 300 s after start, but only 150 s after the last activity.
    rig.clock.advance(Duration::from_secs(150));
    wait_for_sleepers(&rig.clock, 2).await;
    assert_eq!(sup.state(), ServiceState::Healthy);

    // 300 s after the last activity.
    rig.clock.advance(Duration::from_secs(150));
    wait_for_state(&sup, ServiceState::Stopped).await;
    assert_eq!(sup.restart_total(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hung_server_is_killed_and_restarted() {
    let rig = Rig::new().await;
    let sup = rig.supervisor(rig.config(SLEEPER));
    sup.ensure_running().await.unwrap();
    let pid = sup.pid().await.unwrap();

    // The process stays alive but stops answering.
    rig.server.set_hung(true);
    for _ in 0..2 {
        wait_for_sleepers(&rig.clock, 1).await; // steady probe interval
        rig.clock.advance(Duration::from_secs(30));
        wait_for_sleepers(&rig.clock, 1).await; // probe timeout
        assert_eq!(sup.state(), ServiceState::Healthy);
        rig.clock.advance(Duration::from_secs(2));
    }

    // Two failed probes: killed, then restarted through the crash path.
    wait_for_state(&sup, ServiceState::Starting).await;
    rig.server.set_hung(false);
    wait_for_sleepers(&rig.clock, 2).await; // steady probe + backoff
    rig.clock.advance(SECOND);
    wait_for_state(&sup, ServiceState::Healthy).await;
    assert_eq!(sup.restart_total(), 1);
    assert_ne!(sup.pid().await, Some(pid));
    assert!(sup
        .last_logs()
        .iter()
        .any(|l| l.contains("health probe failed twice")));
}

#[tokio::test(flavor = "multi_thread")]
async fn start_failure_is_reported_when_child_dies_before_healthy() {
    let rig = Rig::new().await;
    let mut config = rig.config("#!/bin/sh\nexit 1\n");
    // A port with nothing listening so the probe can never succeed.
    config.port = Some(dead_port());
    let sup = Arc::new(rig.supervisor(config));
    let starting = tokio::spawn({
        let sup = Arc::clone(&sup);
        async move { sup.ensure_running().await }
    });
    pump_until("the start to fail", &rig.clock, SECOND, || {
        starting.is_finished()
    })
    .await;
    let err = starting.await.unwrap().unwrap_err();
    assert!(matches!(err, LlamaError::StartFailed), "{err:?}");
    assert_eq!(sup.state(), ServiceState::Stopped);
    assert!(sup.pid().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_that_never_becomes_healthy_is_killed_at_the_start_deadline() {
    let rig = Rig::new().await;
    let mut config = rig.config(SLEEPER);
    config.port = Some(dead_port());
    config.start_timeout = Duration::from_secs(10);
    let sup = Arc::new(rig.supervisor(config));
    let starting = tokio::spawn({
        let sup = Arc::clone(&sup);
        async move { sup.ensure_running().await }
    });
    wait_until("the worker to be spawned", || {
        sup.last_logs().iter().any(|l| l.contains("spawned"))
    })
    .await;
    pump_until("the start to time out", &rig.clock, SECOND, || {
        starting.is_finished()
    })
    .await;
    let err = starting.await.unwrap().unwrap_err();
    assert!(matches!(err, LlamaError::StartTimeout(_)), "{err:?}");
    assert_eq!(sup.state(), ServiceState::Stopped);
    assert!(sup.pid().await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_the_worker_and_the_next_request_starts_it_again() {
    let rig = Rig::new().await;
    let sup = rig.supervisor(rig.config(SLEEPER));
    sup.ensure_running().await.unwrap();
    let pid = sup.pid().await.unwrap();
    sup.stop().await;
    assert_eq!(sup.state(), ServiceState::Stopped);
    assert!(
        process_gone(pid),
        "stop returned before the worker was gone"
    );
    assert_eq!(sup.restart_total(), 0);
    sup.ensure_running().await.unwrap();
    assert_eq!(sup.state(), ServiceState::Healthy);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_supervisor_tears_down_the_process_group() {
    let rig = Rig::new().await;
    let sup = rig.supervisor(rig.config(SLEEPER));
    sup.ensure_running().await.unwrap();
    let pid = sup.pid().await.unwrap();
    drop(sup);
    wait_until("the worker to die with its supervisor", || {
        process_gone(pid)
    })
    .await;
}
