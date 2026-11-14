//! The worker lifecycle is not tied to one kind of worker: a different
//! binary, different arguments and a different health probe run through the
//! same supervisor.
#![cfg(unix)]

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use isonomic_inference_core::{FixedGate, ManualClock};
use isonomic_inference_testkit::{
    process_gone, read_lines, wait_for_sleepers, wait_until, write_model, write_script,
    RECORDING_STANDIN,
};
use isonomic_worker::{
    Endpoint, HealthProbe, SecretMode, ServiceState, WorkerError, WorkerSpec, WorkerSupervisor,
};

/// A probe that is not HTTP at all: ready once it has been asked `after`
/// times. It counts how often the supervisor asked.
struct CountingProbe {
    asked: Arc<AtomicUsize>,
    after: usize,
}

impl HealthProbe for CountingProbe {
    fn check<'a>(
        &'a self,
        _endpoint: &'a Endpoint,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let asked = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { asked >= self.after })
    }
}

fn spec(dir: &Path, asked: &Arc<AtomicUsize>, after: usize) -> WorkerSpec {
    let bin = write_script(dir, "speech-stand-in", RECORDING_STANDIN);
    let probe = CountingProbe {
        asked: Arc::clone(asked),
        after,
    };
    let mut spec = WorkerSpec::new(
        "speech-worker",
        bin,
        write_model(dir),
        |addr| vec!["serve".into(), "--listen".into(), addr.to_string().into()],
        probe,
    );
    spec.secret = SecretMode::PerLaunch {
        env_var: "SPEECH_KEY".to_string(),
    };
    spec.env_allow_list = vec!["SPEECH_KEY".to_string()];
    spec.idle_unload = None;
    spec
}

#[tokio::test(flavor = "multi_thread")]
async fn another_kind_of_worker_runs_through_the_same_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(ManualClock::new());
    let sup = WorkerSupervisor::with_clock(
        spec(dir.path(), &asked, 1),
        Arc::new(FixedGate::unlimited()),
        clock.clone(),
    );
    assert_eq!(sup.state(), ServiceState::Stopped);

    // No port was given: one is chosen at launch, on loopback.
    let endpoint = sup.ensure_running().await.unwrap();
    assert_eq!(sup.state(), ServiceState::Healthy);
    assert!(endpoint.addr.ip().is_loopback());
    assert_ne!(endpoint.addr.port(), 0);
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    // Its own arguments, its own secret variable, and nothing else.
    let args_file = dir.path().join("args.txt");
    wait_until("the stand-in to record its start", || args_file.exists()).await;
    assert_eq!(
        read_lines(&args_file),
        ["serve", "--listen", &endpoint.addr.to_string()]
    );
    let key = endpoint.api_key.clone().unwrap();
    let env = read_lines(&dir.path().join("env.txt"));
    assert!(env.contains(&format!("SPEECH_KEY={key}")));
    assert!(env.iter().all(|line| !line.starts_with("PATH=")));
    assert!(sup
        .last_logs()
        .iter()
        .any(|l| l.contains("spawned speech-worker")));

    let pid = sup.pid().await.unwrap();
    sup.stop().await;
    assert_eq!(sup.state(), ServiceState::Stopped);
    assert!(process_gone(pid));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_is_probed_at_the_start_interval_until_its_probe_passes() {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(ManualClock::new());
    // Ready on the third probe.
    let sup = Arc::new(WorkerSupervisor::with_clock(
        spec(dir.path(), &asked, 3),
        Arc::new(FixedGate::unlimited()),
        clock.clone(),
    ));
    let starting = tokio::spawn({
        let sup = Arc::clone(&sup);
        async move { sup.ensure_running().await.map(|_| ()) }
    });
    for probes_so_far in 1..=2 {
        wait_for_sleepers(&clock, 1).await; // the wait between start probes
        assert_eq!(asked.load(Ordering::SeqCst), probes_so_far);
        assert_eq!(sup.state(), ServiceState::Starting);
        clock.advance(Duration::from_secs(5));
    }
    starting.await.unwrap().unwrap();
    assert_eq!(asked.load(Ordering::SeqCst), 3);
    assert_eq!(sup.state(), ServiceState::Healthy);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_model_file_is_reported_before_anything_starts() {
    let dir = tempfile::tempdir().unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let mut spec = spec(dir.path(), &asked, 1);
    spec.model_path = dir.path().join("absent.bin");
    let sup = WorkerSupervisor::new(spec, Arc::new(FixedGate::unlimited()));
    match sup.ensure_running().await {
        Err(WorkerError::Io(err)) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected a missing-file error, got {other:?}"),
    }
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(!dir.path().join("args.txt").exists());
}
