//! Configuration and errors for the llama.cpp server worker.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use isonomic_inference_core::RamDenial;

use crate::state::restart_backoff_secs;

/// Environment variable the llama.cpp server reads its API key from. Passing
/// the key this way keeps it out of the process list.
pub const API_KEY_ENV: &str = "LLAMA_API_KEY";

/// The only environment variables a worker may be started with. The worker's
/// environment is built from this list; nothing is inherited from the parent
/// process. Values come from [`LlamaConfig::env`], and the API key is set by
/// the supervisor itself.
pub const ENV_ALLOW_LIST: &[&str] = &[API_KEY_ENV, "HOME", "TMPDIR", "LANG"];

/// Arguments the supervisor owns. They may not appear in `extra_args`.
pub const RESERVED_ARGS: &[&str] = &[
    "--host",
    "--port",
    "-m",
    "--model",
    "--api-key",
    "--api-key-file",
    "--embedding",
    "--embeddings",
];

/// What one server instance is started to do. The llama.cpp server restricts
/// an instance to embeddings when started with `--embeddings`, and refuses
/// embeddings otherwise, so chat and embeddings need separate instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerMode {
    #[default]
    Chat,
    Embedding,
}

/// Whether the server is started with a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApiKeyMode {
    /// A fresh random secret for every launch, sent on every request.
    #[default]
    PerLaunch,
    /// No secret. Only for a server build without an API key option.
    Disabled,
}

/// Configuration for the llama.cpp server worker. All timings are injectable
/// so the state machine is testable; the defaults are the production values.
#[derive(Debug, Clone)]
pub struct LlamaConfig {
    /// Path to the model server binary. Supplied by the caller; this crate
    /// knows nothing about where binaries are bundled.
    pub binary_path: PathBuf,
    /// Path to the GGUF model to serve.
    pub model_path: PathBuf,
    /// What this instance is for.
    pub mode: ServerMode,
    /// `None` (production): a free loopback port is chosen at every launch.
    /// `Some` is a seam for tests, where a fake server already owns a port.
    pub port: Option<u16>,
    pub api_key: ApiKeyMode,
    /// Idle unload timeout; `None` disables idle unload.
    pub idle_unload: Option<Duration>,
    /// How long to wait for the server to become healthy after spawn.
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
    /// Extra args appended to the server command line. Reserved arguments
    /// (host, port, model, API key, mode) are refused.
    pub extra_args: Vec<String>,
    /// Environment for the worker. Every name must be on [`ENV_ALLOW_LIST`].
    pub env: Vec<(String, String)>,
}

impl LlamaConfig {
    pub fn new(binary_path: PathBuf, model_path: PathBuf) -> Self {
        Self {
            binary_path,
            model_path,
            mode: ServerMode::Chat,
            port: None,
            api_key: ApiKeyMode::PerLaunch,
            idle_unload: Some(Duration::from_secs(5 * 60)),
            start_timeout: Duration::from_secs(120),
            probe_interval: Duration::from_secs(5),
            steady_probe_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(2),
            stop_grace: Duration::from_secs(5),
            backoff: default_backoff,
            extra_args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// The server command line for a launch on `addr`.
    pub fn server_args(&self, addr: SocketAddr) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "--host".into(),
            addr.ip().to_string().into(),
            "--port".into(),
            addr.port().to_string().into(),
            "-m".into(),
            self.model_path.clone().into_os_string(),
        ];
        if self.mode == ServerMode::Embedding {
            args.push("--embeddings".into());
        }
        args.extend(self.extra_args.iter().map(OsString::from));
        args
    }

    /// Refuse a configuration that would widen the worker's environment or
    /// take over an argument the supervisor owns.
    pub fn validate(&self) -> Result<(), LlamaError> {
        for (name, _) in &self.env {
            if !ENV_ALLOW_LIST.contains(&name.as_str()) || name == API_KEY_ENV {
                return Err(LlamaError::EnvNotAllowed(name.clone()));
            }
        }
        for arg in &self.extra_args {
            let name = arg.split('=').next().unwrap_or(arg);
            if RESERVED_ARGS.contains(&name) {
                return Err(LlamaError::ReservedArg(arg.clone()));
            }
        }
        Ok(())
    }
}

fn default_backoff(attempt: u32) -> Duration {
    Duration::from_secs(restart_backoff_secs(attempt))
}

#[derive(Debug, thiserror::Error)]
pub enum LlamaError {
    #[error("memory gate refused start: {0}")]
    RamDenied(RamDenial),
    #[error("llama-server is degraded (circuit breaker tripped); see last_logs()")]
    Degraded,
    #[error("llama-server exited during startup; see last_logs()")]
    StartFailed,
    #[error("llama-server failed to become healthy within {0:?}")]
    StartTimeout(Duration),
    #[error("failed to spawn llama-server: {0}")]
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

    fn config() -> LlamaConfig {
        LlamaConfig::new(
            PathBuf::from("/bin/server"),
            PathBuf::from("/models/m.gguf"),
        )
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn the_command_line_binds_the_given_loopback_address_and_model() {
        let addr: SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let args = strings(config().server_args(addr));
        assert_eq!(
            args,
            [
                "--host",
                "127.0.0.1",
                "--port",
                "41234",
                "-m",
                "/models/m.gguf"
            ]
        );
    }

    #[test]
    fn embedding_mode_adds_its_flag_and_extra_args_come_last() {
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let mut cfg = config();
        cfg.mode = ServerMode::Embedding;
        cfg.extra_args = vec!["--pooling".to_string(), "mean".to_string()];
        let args = strings(cfg.server_args(addr));
        assert_eq!(&args[6..], ["--embeddings", "--pooling", "mean"]);
    }

    #[test]
    fn the_defaults_are_the_production_values() {
        let cfg = config();
        assert_eq!(cfg.port, None);
        assert_eq!(cfg.api_key, ApiKeyMode::PerLaunch);
        assert_eq!(cfg.idle_unload, Some(Duration::from_secs(300)));
        assert_eq!(cfg.steady_probe_interval, Duration::from_secs(30));
        assert_eq!((cfg.backoff)(0), Duration::from_secs(1));
        assert_eq!((cfg.backoff)(10), Duration::from_secs(60));
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn variables_off_the_allow_list_are_refused() {
        let mut cfg = config();
        cfg.env = vec![("TMPDIR".to_string(), "/tmp".to_string())];
        assert!(cfg.validate().is_ok());
        for name in ["DYLD_INSERT_LIBRARIES", "LD_PRELOAD", "PATH", API_KEY_ENV] {
            cfg.env = vec![(name.to_string(), "x".to_string())];
            match cfg.validate() {
                Err(LlamaError::EnvNotAllowed(got)) => assert_eq!(got, name),
                other => panic!("expected EnvNotAllowed for {name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn reserved_arguments_are_refused_in_either_spelling() {
        for arg in ["--host", "--host=0.0.0.0", "--port", "--api-key=x", "-m"] {
            let mut cfg = config();
            cfg.extra_args = vec![arg.to_string()];
            assert!(
                matches!(cfg.validate(), Err(LlamaError::ReservedArg(_))),
                "{arg} must be refused"
            );
        }
    }
}
