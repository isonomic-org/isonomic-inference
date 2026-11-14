//! The worker supervisor: lazy start, state, logs and the public handle.
//! The background tasks that keep a running worker alive are in `tasks.rs`.

use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use isonomic_inference_core::{timeout, Clock, RamGate, SystemClock};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Child;
use tokio::sync::{watch, Mutex};

use crate::process::{self, Guardian};
use crate::spec::{SecretMode, WorkerError, WorkerSpec};
use crate::state::ServiceState;
use crate::tasks;

/// How many log lines are retained (last 50).
pub const LOG_RING_CAPACITY: usize = 50;

/// Where a running worker can be reached, and the secret it expects.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub addr: SocketAddr,
    /// Sent as `Authorization: Bearer …` on every request. Changes with
    /// every launch.
    pub api_key: Option<String>,
}

impl Endpoint {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = self.api_key.as_ref().map(|_| "<redacted>");
        f.debug_struct("Endpoint")
            .field("addr", &self.addr)
            .field("api_key", &key)
            .finish()
    }
}

pub(crate) struct Shared {
    pub spec: WorkerSpec,
    pub gate: Arc<dyn RamGate>,
    pub clock: Arc<dyn Clock>,
    pub state_tx: watch::Sender<ServiceState>,
    pub state_rx: watch::Receiver<ServiceState>,
    /// Pid of the worker that is currently running (0 = none). Cleared when
    /// the worker has exited and been reaped.
    pub live_tx: watch::Sender<u32>,
    pub logs: std::sync::Mutex<VecDeque<String>>,
    pub restarts: std::sync::Mutex<VecDeque<Instant>>,
    pub last_used: std::sync::Mutex<Instant>,
    pub restart_total: AtomicU32,
    /// Set during a deliberate stop (explicit stop / idle unload / drop) so the
    /// monitor doesn't count the exit as a crash.
    pub stopping: AtomicBool,
    /// Bumped on every deliberate stop; background tasks from an older
    /// generation exit instead of acting.
    pub generation: AtomicU64,
    /// Process-group id of the current worker (0 = none). Used for teardown.
    pub pgid: AtomicU32,
}

pub(crate) struct ControlState {
    pub pid: Option<u32>,
    pub endpoint: Option<Endpoint>,
}

/// One launch of the worker.
pub(crate) struct Launch {
    pub child: Child,
    pub pid: u32,
    pub endpoint: Endpoint,
    pub guardian: Guardian,
}

/// Supervises a single worker. Dropping it tears the process group down.
pub struct WorkerSupervisor {
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlState>>,
}

impl WorkerSupervisor {
    pub fn new(spec: WorkerSpec, gate: Arc<dyn RamGate>) -> Self {
        Self::with_clock(spec, gate, Arc::new(SystemClock))
    }

    /// Like [`new`](Self::new), with an injected clock. Every wait the
    /// supervisor makes goes through it.
    pub fn with_clock(spec: WorkerSpec, gate: Arc<dyn RamGate>, clock: Arc<dyn Clock>) -> Self {
        let (state_tx, state_rx) = watch::channel(ServiceState::Stopped);
        let (live_tx, _) = watch::channel(0);
        let now = clock.now();
        Self {
            shared: Arc::new(Shared {
                spec,
                gate,
                clock,
                state_tx,
                state_rx,
                live_tx,
                logs: std::sync::Mutex::new(VecDeque::with_capacity(LOG_RING_CAPACITY)),
                restarts: std::sync::Mutex::new(VecDeque::new()),
                last_used: std::sync::Mutex::new(now),
                restart_total: AtomicU32::new(0),
                stopping: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                pgid: AtomicU32::new(0),
            }),
            control: Arc::new(Mutex::new(ControlState {
                pid: None,
                endpoint: None,
            })),
        }
    }

    /// Current lifecycle state (stopped / starting / healthy / degraded).
    pub fn state(&self) -> ServiceState {
        *self.shared.state_tx.borrow()
    }

    /// Subscribe to state transitions.
    pub fn subscribe(&self) -> watch::Receiver<ServiceState> {
        self.shared.state_rx.clone()
    }

    pub fn spec(&self) -> &WorkerSpec {
        &self.shared.spec
    }

    /// Path to the model this supervisor serves.
    pub fn model_path(&self) -> &std::path::Path {
        &self.shared.spec.model_path
    }

    /// Last ≤50 captured log lines (kept when the breaker trips).
    pub fn last_logs(&self) -> Vec<String> {
        let logs = self.shared.logs.lock().expect("logs lock");
        logs.iter().cloned().collect()
    }

    /// Total unexpected-exit restarts since creation (not reset by deliberate stops).
    pub fn restart_total(&self) -> u32 {
        self.shared.restart_total.load(Ordering::SeqCst)
    }

    /// Where the current (or most recent) launch listens.
    pub async fn endpoint(&self) -> Option<Endpoint> {
        self.control.lock().await.endpoint.clone()
    }

    /// The current worker pid, if one is running.
    pub async fn pid(&self) -> Option<u32> {
        self.control.lock().await.pid
    }

    /// Record activity so the idle-unload timer restarts.
    pub fn touch(&self) {
        touch(&self.shared);
    }

    /// Lazy start: ensure the worker is running and healthy, starting it on
    /// demand. Consults the memory gate before any spawn (refuses with a
    /// clear reason rather than thrashing). Returns where to reach it.
    pub async fn ensure_running(&self) -> Result<Endpoint, WorkerError> {
        self.touch();
        match self.state() {
            ServiceState::Degraded => return Err(WorkerError::Degraded),
            ServiceState::Healthy => {
                if let Some(endpoint) = self.endpoint().await {
                    return Ok(endpoint);
                }
            }
            _ => {}
        }

        let mut ctl = self.control.lock().await;
        // Re-check under the lock: another caller may have finished starting it.
        match *self.shared.state_tx.borrow() {
            ServiceState::Degraded => return Err(WorkerError::Degraded),
            ServiceState::Healthy => {
                if let Some(endpoint) = ctl.endpoint.clone() {
                    return Ok(endpoint);
                }
            }
            _ => {}
        }

        self.shared.spec.validate()?;
        let model_bytes = tokio::fs::metadata(&self.shared.spec.model_path)
            .await?
            .len();
        self.shared
            .gate
            .admit(model_bytes)
            .map_err(WorkerError::RamDenied)?;

        self.shared.stopping.store(false, Ordering::SeqCst);
        let generation = self.shared.generation.load(Ordering::SeqCst);
        self.shared.state_tx.send_replace(ServiceState::Starting);

        let shared = &self.shared;
        let Launch {
            mut child,
            pid,
            endpoint,
            mut guardian,
        } = match launch(shared) {
            Ok(launch) => launch,
            Err(err) => {
                shared.state_tx.send_replace(ServiceState::Stopped);
                return Err(err);
            }
        };
        ctl.pid = Some(pid);
        ctl.endpoint = Some(endpoint.clone());

        let deadline = shared.clock.now() + shared.spec.start_timeout;
        let failure = loop {
            if probe(shared, &endpoint).await {
                break None;
            }
            if child.try_wait().ok().flatten().is_some() {
                break Some(WorkerError::StartFailed);
            }
            if shared.clock.now() >= deadline {
                // The worker is not reaped yet, so its group id is still ours.
                process::kill_group_now(pid);
                let _ = child.wait().await;
                break Some(WorkerError::StartTimeout(shared.spec.start_timeout));
            }
            shared.clock.sleep(shared.spec.probe_interval).await;
        };
        if let Some(err) = failure {
            process::reap_group(&mut guardian, pid);
            ctl.pid = None;
            shared.pgid.store(0, Ordering::SeqCst);
            shared.live_tx.send_replace(0);
            shared.state_tx.send_replace(ServiceState::Stopped);
            return Err(err);
        }

        shared.state_tx.send_replace(ServiceState::Healthy);
        self.touch();
        tokio::spawn(tasks::run_monitor(
            Arc::clone(shared),
            Arc::clone(&self.control),
            child,
            guardian,
            pid,
            generation,
        ));
        tokio::spawn(tasks::run_steady_probe(
            Arc::clone(shared),
            Arc::clone(&self.control),
            generation,
        ));
        if let Some(idle) = shared.spec.idle_unload {
            tokio::spawn(tasks::run_idle_watcher(
                Arc::clone(shared),
                Arc::clone(&self.control),
                idle,
                generation,
            ));
        }
        Ok(endpoint)
    }

    /// Deliberate stop (also used by idle unload): SIGTERM to the process
    /// group, a grace period, then SIGKILL. Not counted as a crash.
    pub async fn stop(&self) {
        tasks::stop_with(&self.shared, &self.control).await;
    }

    /// "Report and retry": clear the breaker window and return to Stopped so
    /// the next request can lazily start again.
    pub async fn reset(&self) {
        tasks::stop_with(&self.shared, &self.control).await;
        self.shared.restarts.lock().expect("restarts lock").clear();
    }
}

impl Drop for WorkerSupervisor {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        // Non-zero only while the worker is running and not yet reaped, so
        // the group id is still ours.
        let pgid = self.shared.pgid.swap(0, Ordering::SeqCst);
        process::kill_group_now(pgid);
    }
}

pub(crate) fn touch(shared: &Shared) {
    *shared.last_used.lock().expect("last_used lock") = shared.clock.now();
}

/// Start the worker: a loopback address and a secret chosen for this launch,
/// an environment built from the allow-list, its own process group.
pub(crate) fn launch(shared: &Arc<Shared>) -> Result<Launch, WorkerError> {
    let spec = &shared.spec;
    spec.validate()?;
    let addr = process::alloc_loopback_addr(spec.port)?;
    let mut env = spec.env.clone();
    let api_key = match &spec.secret {
        SecretMode::PerLaunch { env_var } => {
            let key = process::new_secret()?;
            env.push((env_var.clone(), key.clone()));
            Some(key)
        }
        SecretMode::Disabled => None,
    };
    let args = spec.command_line(addr);
    let mut spawned =
        process::spawn_worker(&spec.binary_path, &args, &env).map_err(WorkerError::Spawn)?;
    let pid = spawned.pid;
    if let Some(stdout) = spawned.child.stdout.take() {
        spawn_log_pump(Arc::clone(shared), stdout, "stdout");
    }
    if let Some(stderr) = spawned.child.stderr.take() {
        spawn_log_pump(Arc::clone(shared), stderr, "stderr");
    }
    push_log(
        shared,
        format!("spawned {} (pid {pid}) on {addr}", spec.name),
    );
    shared.pgid.store(pid, Ordering::SeqCst);
    shared.live_tx.send_replace(pid);
    Ok(Launch {
        child: spawned.child,
        pid,
        endpoint: Endpoint { addr, api_key },
        guardian: spawned.guardian,
    })
}

/// One bounded health probe against `endpoint`.
pub(crate) async fn probe(shared: &Shared, endpoint: &Endpoint) -> bool {
    let attempt = shared.spec.probe.check(endpoint);
    timeout(shared.clock.as_ref(), shared.spec.probe_timeout, attempt)
        .await
        .unwrap_or(false)
}

fn spawn_log_pump<R>(shared: Arc<Shared>, reader: R, label: &'static str)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            push_log(&shared, format!("[{label}] {line}"));
        }
    });
}

pub(crate) fn push_log(shared: &Shared, line: String) {
    let mut logs = shared.logs.lock().expect("logs lock");
    if logs.len() == LOG_RING_CAPACITY {
        logs.pop_front();
    }
    logs.push_back(line);
}
