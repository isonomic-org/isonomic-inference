//! The memory gate is asked before every start: the first lazy start and
//! every crash restart.
#![cfg(unix)]

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use isonomic_inference_core::{ManualClock, RamDenial, RamGate};
use isonomic_inference_testkit::{wait_for_sleepers, write_model, write_script};
use isonomic_worker::{
    Endpoint, HealthProbe, ServiceState, WorkerError, WorkerSpec, WorkerSupervisor,
};

/// A gate the test can open and close, which counts how often it was asked.
#[derive(Default)]
struct SwitchGate {
    closed: AtomicBool,
    asked: AtomicUsize,
}

impl RamGate for SwitchGate {
    fn admit(&self, model_bytes: u64) -> Result<(), RamDenial> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if !self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        Err(RamDenial {
            required_bytes: model_bytes,
            available_bytes: 0,
            reason: "free memory dropped since the model was loaded".to_string(),
        })
    }
}

/// Always ready.
struct Ready;

impl HealthProbe for Ready {
    fn check<'a>(
        &'a self,
        _endpoint: &'a Endpoint,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
}

async fn wait_for_state(sup: &WorkerSupervisor, want: ServiceState) {
    let mut rx = sup.subscribe();
    let reached = tokio::time::timeout(Duration::from_secs(30), rx.wait_for(|s| *s == want))
        .await
        .is_ok_and(|seen| seen.is_ok());
    assert!(
        reached,
        "did not reach {want:?}; state is {:?}",
        sup.state()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_restart_asks_the_gate_and_a_refusal_stops_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(dir.path(), "stand-in", "#!/bin/sh\nexec /bin/sleep 3600\n");
    let mut spec = WorkerSpec::new(
        "gated-worker",
        bin,
        write_model(dir.path()),
        |_| vec![],
        Ready,
    );
    spec.idle_unload = None;
    let gate = Arc::new(SwitchGate::default());
    let clock = Arc::new(ManualClock::new());
    let sup = WorkerSupervisor::with_clock(spec, gate.clone(), clock.clone());

    sup.ensure_running().await.unwrap();
    assert_eq!(
        gate.asked.load(Ordering::SeqCst),
        1,
        "asked on the lazy start"
    );
    let pid = sup.pid().await.unwrap();

    // Memory is gone by the time the worker crashes.
    gate.closed.store(true, Ordering::SeqCst);
    let killed = std::process::Command::new("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());

    wait_for_state(&sup, ServiceState::Starting).await;
    wait_for_sleepers(&clock, 2).await; // steady probe + backoff
    clock.advance(Duration::from_secs(1));

    // Asked again before the restart, refused, and nothing was started.
    wait_for_state(&sup, ServiceState::Stopped).await;
    assert_eq!(gate.asked.load(Ordering::SeqCst), 2, "asked on the restart");
    assert_eq!(sup.restart_total(), 0, "a refused restart is not a restart");
    assert!(sup.pid().await.is_none());
    assert!(sup
        .last_logs()
        .iter()
        .any(|l| l.contains("restart refused") && l.contains("free memory dropped")));

    // The next request asks again and gets the refusal with its numbers.
    match sup.ensure_running().await {
        Err(WorkerError::RamDenied(denial)) => {
            assert_eq!(denial.required_bytes, 4);
            assert_eq!(denial.available_bytes, 0);
        }
        other => panic!("expected RamDenied, got {other:?}"),
    }
    assert_eq!(sup.state(), ServiceState::Stopped);

    // Once memory is back, the next request starts it.
    gate.closed.store(false, Ordering::SeqCst);
    sup.ensure_running().await.unwrap();
    assert_eq!(sup.state(), ServiceState::Healthy);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_restart_with_the_gate_open_is_still_counted() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_script(dir.path(), "stand-in", "#!/bin/sh\nexec /bin/sleep 3600\n");
    let mut spec = WorkerSpec::new(
        "gated-worker",
        bin,
        write_model(dir.path()),
        |_| vec![],
        Ready,
    );
    spec.idle_unload = None;
    let gate = Arc::new(SwitchGate::default());
    let clock = Arc::new(ManualClock::new());
    let sup = WorkerSupervisor::with_clock(spec, gate.clone(), clock.clone());

    sup.ensure_running().await.unwrap();
    let pid = sup.pid().await.unwrap();
    let killed = std::process::Command::new("/bin/kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());

    wait_for_state(&sup, ServiceState::Starting).await;
    wait_for_sleepers(&clock, 2).await;
    clock.advance(Duration::from_secs(1));
    wait_for_state(&sup, ServiceState::Healthy).await;
    assert_eq!(gate.asked.load(Ordering::SeqCst), 2);
    assert_eq!(sup.restart_total(), 1);
    assert_ne!(sup.pid().await, Some(pid));
}
