//! Background tasks for a running worker: the monitor that owns the process
//! and restarts it, the steady-state probe that catches a hung server, the
//! idle watcher, and the deliberate stop they share.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use isonomic_inference_core::timeout;
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::process::{self, Guardian};
use crate::state::{should_trip_breaker, ServiceState};
use crate::supervisor::{launch, probe, push_log, ControlState, Launch, Shared};

/// Restart window for the circuit breaker (more than 5 restarts in 10 minutes).
pub const BREAKER_WINDOW: Duration = Duration::from_secs(600);

/// Deliberate stop: ask the group to stop, wait out the grace period, then
/// kill it. Returns once the worker is gone. Not counted as a crash.
pub(crate) async fn stop_with(shared: &Arc<Shared>, control: &Arc<Mutex<ControlState>>) {
    shared.stopping.store(true, Ordering::SeqCst);
    shared.generation.fetch_add(1, Ordering::SeqCst);
    let pid = {
        let mut ctl = control.lock().await;
        ctl.pid.take()
    };
    if let Some(pid) = pid {
        terminate(shared, pid).await;
    }
    shared.pgid.store(0, Ordering::SeqCst);
    shared.state_tx.send_replace(ServiceState::Stopped);
}

/// SIGTERM the worker's group, give it the grace period, then SIGKILL. The
/// group is only signalled while the worker is still recorded as live, which
/// means it has not been reaped and its group id is still ours.
async fn terminate(shared: &Shared, pid: u32) {
    let mut live = shared.live_tx.subscribe();
    if *live.borrow() != pid {
        return;
    }
    process::stop_group(pid);
    let exited = live.wait_for(|current| *current != pid);
    let grace = shared.spec.stop_grace;
    if timeout(shared.clock.as_ref(), grace, exited)
        .await
        .is_some()
    {
        return;
    }
    if *live.borrow() == pid {
        process::kill_group_now(pid);
        let _ = live.wait_for(|current| *current != pid).await;
    }
}

/// Record that the worker `pid` has exited and been reaped.
async fn mark_exited(shared: &Shared, control: &Mutex<ControlState>, pid: u32) {
    {
        let mut ctl = control.lock().await;
        if ctl.pid == Some(pid) {
            ctl.pid = None;
        }
    }
    let _ = shared
        .pgid
        .compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    shared.live_tx.send_if_modified(|current| {
        let was_live = *current == pid;
        if was_live {
            *current = 0;
        }
        was_live
    });
}

fn superseded(shared: &Shared, generation: u64) -> bool {
    shared.generation.load(Ordering::SeqCst) != generation
}

/// Owns the child: reaps exits, distinguishes deliberate stops from crashes,
/// restarts on the backoff schedule, trips the breaker into Degraded.
pub(crate) async fn run_monitor(
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlState>>,
    mut child: Child,
    mut guardian: Guardian,
    mut pid: u32,
    generation: u64,
) {
    let mut attempt: u32 = 0;
    loop {
        let status = child.wait().await;
        process::reap_group(&mut guardian, pid);
        mark_exited(&shared, &control, pid).await;
        if superseded(&shared, generation) || shared.stopping.load(Ordering::SeqCst) {
            return; // deliberate stop or unload: not a crash
        }
        push_log(
            &shared,
            format!("{} exited unexpectedly ({status:?})", shared.spec.name),
        );

        let tripped = {
            let mut restarts = shared.restarts.lock().expect("restarts lock");
            let now = shared.clock.now();
            restarts.push_back(now);
            while let Some(front) = restarts.front() {
                if now.duration_since(*front) > BREAKER_WINDOW {
                    restarts.pop_front();
                } else {
                    break;
                }
            }
            should_trip_breaker(restarts.len() as u32)
        };
        if tripped {
            push_log(
                &shared,
                "circuit breaker tripped: >5 restarts in 10 min, entering degraded".to_string(),
            );
            shared.state_tx.send_replace(ServiceState::Degraded);
            return;
        }

        shared.state_tx.send_replace(ServiceState::Starting);
        shared.clock.sleep((shared.spec.backoff)(attempt)).await;
        attempt = attempt.saturating_add(1);
        shared.restart_total.fetch_add(1, Ordering::SeqCst);

        if superseded(&shared, generation) {
            return;
        }
        // A new launch: new port (unless one was injected) and a new secret.
        let endpoint = match launch(&shared) {
            Ok(Launch {
                child: new_child,
                pid: new_pid,
                endpoint,
                guardian: new_guardian,
            }) => {
                {
                    let mut ctl = control.lock().await;
                    ctl.pid = Some(new_pid);
                    ctl.endpoint = Some(endpoint.clone());
                }
                child = new_child;
                guardian = new_guardian;
                pid = new_pid;
                endpoint
            }
            Err(err) => {
                push_log(&shared, format!("respawn failed: {err}"));
                shared.state_tx.send_replace(ServiceState::Degraded);
                return;
            }
        };

        // Probe until healthy (bounded). If it never comes up, or a stop came
        // in meanwhile, kill it and let the next loop iteration handle the exit.
        let deadline = shared.clock.now() + shared.spec.start_timeout;
        let mut healthy = false;
        while shared.clock.now() < deadline {
            if superseded(&shared, generation) {
                break;
            }
            if probe(&shared, &endpoint).await {
                healthy = true;
                break;
            }
            if child.try_wait().ok().flatten().is_some() {
                break; // already dead: handled by the next wait()
            }
            shared.clock.sleep(shared.spec.probe_interval).await;
        }
        if healthy && !superseded(&shared, generation) {
            shared.state_tx.send_replace(ServiceState::Healthy);
            crate::supervisor::touch(&shared);
        } else if child.try_wait().ok().flatten().is_none() {
            // Still running and not reaped, so the group id is still ours.
            process::kill_group_now(pid);
        }
    }
}

/// Steady-state readiness probing (every 30 s). Two consecutive failures with
/// the process still alive = hung server: kill it so the monitor's normal
/// crash path restarts it.
pub(crate) async fn run_steady_probe(
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlState>>,
    generation: u64,
) {
    let mut consecutive_failures: u32 = 0;
    loop {
        shared.clock.sleep(shared.spec.steady_probe_interval).await;
        if superseded(&shared, generation) || shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        match *shared.state_tx.borrow() {
            ServiceState::Healthy => {}
            ServiceState::Starting => continue, // monitor mid-restart
            ServiceState::Stopped | ServiceState::Degraded => return,
        }
        let (endpoint, pid) = {
            let ctl = control.lock().await;
            (ctl.endpoint.clone(), ctl.pid)
        };
        let Some(endpoint) = endpoint else { continue };
        if probe(&shared, &endpoint).await {
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
            if consecutive_failures >= 2 {
                push_log(
                    &shared,
                    "health probe failed twice while process alive: killing for restart"
                        .to_string(),
                );
                if let Some(pid) = pid {
                    process::kill_pid(pid);
                }
                consecutive_failures = 0;
            }
        }
    }
}

/// Idle unload: after `idle` with no `touch()`, deliberately stop the worker.
pub(crate) async fn run_idle_watcher(
    shared: Arc<Shared>,
    control: Arc<Mutex<ControlState>>,
    idle: Duration,
    generation: u64,
) {
    let check = (idle / 2).max(Duration::from_millis(25));
    loop {
        shared.clock.sleep(check).await;
        if superseded(&shared, generation) || shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        let state = *shared.state_tx.borrow();
        if state == ServiceState::Stopped || state == ServiceState::Degraded {
            return;
        }
        let last_used = *shared.last_used.lock().expect("last_used lock");
        let idle_for = shared.clock.now().saturating_duration_since(last_used);
        if state == ServiceState::Healthy && idle_for >= idle {
            push_log(
                &shared,
                format!("idle for {idle:?}: unloading {}", shared.spec.name),
            );
            stop_with(&shared, &control).await;
            return;
        }
    }
}
