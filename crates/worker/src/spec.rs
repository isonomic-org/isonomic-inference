// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! What a worker is: its binary, its arguments, its health probe, and the
//! policy numbers that govern its lifecycle.

use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use isonomic_inference_core::RamDenial;

use crate::health::probe_http;
use crate::state::restart_backoff_secs;
use crate::supervisor::Endpoint;

/// Decides whether a running worker is ready to serve.
pub trait HealthProbe: Send + Sync + 'static {
    /// One attempt. The supervisor bounds how long it may take.
    fn check<'a>(
        &'a self,
        endpoint: &'a Endpoint,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

/// Port check, then `GET <path>` expecting 200. The launch secret is sent as
/// a bearer token.
#[derive(Debug, Clone)]
pub struct HttpProbe {
    path: String,
}

impl HttpProbe {
    pub fn get(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

impl HealthProbe for HttpProbe {
    fn check<'a>(
        &'a self,
        endpoint: &'a Endpoint,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(probe_http(
            endpoint.addr,
            &self.path,
            endpoint.api_key.as_deref(),
        ))
    }
}

/// Whether the worker is started with a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretMode {
    /// A fresh random secret for every launch, handed to the worker in this
    /// environment variable (never on its command line).
    PerLaunch { env_var: String },
    /// No secret. Only for a worker that has no such option.
    Disabled,
}

type ArgsFn = dyn Fn(SocketAddr) -> Vec<OsString> + Send + Sync;

/// Everything the supervisor needs to run one kind of worker. All timings
/// are injectable so the state machine is testable; [`WorkerSpec::new`] sets
/// the production values.
#[derive(Clone)]
pub struct WorkerSpec {
    /// Name used in log lines.
    pub name: String,
    /// Path to the worker binary. Supplied by the caller; this crate knows
    /// nothing about where binaries are bundled.
    pub binary_path: PathBuf,
    /// The model file the worker loads. Its size is what the memory gate is
    /// asked about before every lazy start.
    pub model_path: PathBuf,
    /// The arguments the supervisor owns, for a launch on the given address.
    pub args: Arc<ArgsFn>,
    /// Extra arguments appended after the owned ones.
    pub extra_args: Vec<String>,
    /// Arguments that may not appear in `extra_args`.
    pub reserved_args: Vec<String>,
    pub probe: Arc<dyn HealthProbe>,
    pub secret: SecretMode,
    /// Environment for the worker. Every name must be on `env_allow_list`.
    /// Nothing is inherited from this process.
    pub env: Vec<(String, String)>,
    pub env_allow_list: Vec<String>,
    /// `None` (production): a free loopback port is chosen at every launch.
    /// `Some` is a seam for tests, where a fake server already owns a port.
    pub port: Option<u16>,
    /// Idle unload timeout; `None` disables idle unload.
    pub idle_unload: Option<Duration>,
    /// How long to wait for the worker to become healthy after spawn.
    pub start_timeout: Duration,
    /// Interval between health probes while starting.
    pub probe_interval: Duration,
    /// Interval between health probes at steady state (30 s).
    pub steady_probe_interval: Duration,
    /// Per-probe timeout.
    pub probe_timeout: Duration,
    /// Grace between asking the worker to stop and killing it.
    pub stop_grace: Duration,
    /// Backoff schedule between restarts.
    pub backoff: fn(u32) -> Duration,
}

impl WorkerSpec {
    pub fn new(
        name: impl Into<String>,
        binary_path: PathBuf,
        model_path: PathBuf,
        args: impl Fn(SocketAddr) -> Vec<OsString> + Send + Sync + 'static,
        probe: impl HealthProbe,
    ) -> Self {
        Self {
            name: name.into(),
            binary_path,
            model_path,
            args: Arc::new(args),
            extra_args: Vec::new(),
            reserved_args: Vec::new(),
            probe: Arc::new(probe),
            secret: SecretMode::Disabled,
            env: Vec::new(),
            env_allow_list: Vec::new(),
            port: None,
            idle_unload: Some(Duration::from_secs(5 * 60)),
            start_timeout: Duration::from_secs(120),
            probe_interval: Duration::from_secs(5),
            steady_probe_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(2),
            stop_grace: Duration::from_secs(5),
            backoff: default_backoff,
        }
    }

    /// The full command line for a launch on `addr`.
    pub fn command_line(&self, addr: SocketAddr) -> Vec<OsString> {
        let mut args = (self.args)(addr);
        args.extend(self.extra_args.iter().map(OsString::from));
        args
    }

    /// Refuse a spec that would widen the worker's environment or take over
    /// an argument the supervisor owns.
    pub fn validate(&self) -> Result<(), WorkerError> {
        let secret_var = match &self.secret {
            SecretMode::PerLaunch { env_var } => Some(env_var.as_str()),
            SecretMode::Disabled => None,
        };
        for (name, _) in &self.env {
            if !self.env_allow_list.contains(name) || Some(name.as_str()) == secret_var {
                return Err(WorkerError::EnvNotAllowed(name.clone()));
            }
        }
        for arg in &self.extra_args {
            let name = arg.split('=').next().unwrap_or(arg);
            if self.reserved_args.iter().any(|reserved| reserved == name) {
                return Err(WorkerError::ReservedArg(arg.clone()));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for WorkerSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerSpec")
            .field("name", &self.name)
            .field("binary_path", &self.binary_path)
            .field("model_path", &self.model_path)
            .field("extra_args", &self.extra_args)
            .field("secret", &self.secret)
            .field("port", &self.port)
            .field("idle_unload", &self.idle_unload)
            .finish_non_exhaustive()
    }
}

fn default_backoff(attempt: u32) -> Duration {
    Duration::from_secs(restart_backoff_secs(attempt))
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("memory gate refused start: {0}")]
    RamDenied(RamDenial),
    #[error("worker is degraded (circuit breaker tripped); see last_logs()")]
    Degraded,
    #[error("worker exited during startup; see last_logs()")]
    StartFailed,
    #[error("worker failed to become healthy within {0:?}")]
    StartTimeout(Duration),
    #[error("failed to spawn worker: {0}")]
    Spawn(std::io::Error),
    #[error("environment variable {0} is not on the worker allow-list")]
    EnvNotAllowed(String),
    #[error("argument {0} is set by the supervisor and cannot be overridden")]
    ReservedArg(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> WorkerSpec {
        WorkerSpec::new(
            "speech-worker",
            PathBuf::from("/bin/worker"),
            PathBuf::from("/models/m.bin"),
            |addr| vec!["--listen".into(), addr.to_string().into()],
            HttpProbe::get("/ready"),
        )
    }

    #[test]
    fn the_defaults_are_the_production_values() {
        let spec = spec();
        assert_eq!(spec.port, None);
        assert_eq!(spec.secret, SecretMode::Disabled);
        assert_eq!(spec.idle_unload, Some(Duration::from_secs(300)));
        assert_eq!(spec.steady_probe_interval, Duration::from_secs(30));
        assert_eq!((spec.backoff)(0), Duration::from_secs(1));
        assert_eq!((spec.backoff)(10), Duration::from_secs(60));
        assert!(spec.validate().is_ok());
        assert!(format!("{spec:?}").contains("speech-worker"));
    }

    #[test]
    fn owned_arguments_come_first_and_extra_arguments_last() {
        let mut spec = spec();
        spec.extra_args = vec!["--threads".to_string(), "4".to_string()];
        let addr: SocketAddr = "127.0.0.1:4100".parse().unwrap();
        let line: Vec<String> = spec
            .command_line(addr)
            .into_iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(line, ["--listen", "127.0.0.1:4100", "--threads", "4"]);
    }

    #[test]
    fn the_environment_is_limited_to_the_allow_list() {
        let mut spec = spec();
        spec.secret = SecretMode::PerLaunch {
            env_var: "WORKER_KEY".to_string(),
        };
        spec.env_allow_list = vec!["TMPDIR".to_string(), "WORKER_KEY".to_string()];
        spec.env = vec![("TMPDIR".to_string(), "/tmp".to_string())];
        assert!(spec.validate().is_ok());
        // Off the list, or the secret's own variable: refused.
        for name in ["LD_PRELOAD", "PATH", "WORKER_KEY"] {
            spec.env = vec![(name.to_string(), "x".to_string())];
            match spec.validate() {
                Err(WorkerError::EnvNotAllowed(got)) => assert_eq!(got, name),
                other => panic!("expected EnvNotAllowed for {name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn reserved_arguments_are_refused_in_either_spelling() {
        for arg in ["--listen", "--listen=0.0.0.0:1"] {
            let mut spec = spec();
            spec.reserved_args = vec!["--listen".to_string()];
            spec.extra_args = vec![arg.to_string()];
            assert!(matches!(spec.validate(), Err(WorkerError::ReservedArg(_))));
        }
    }
}
