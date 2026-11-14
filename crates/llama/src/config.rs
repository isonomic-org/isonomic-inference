//! Configuration for the llama.cpp server worker, and its translation into
//! a worker spec.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use isonomic_worker::{restart_backoff_secs, HttpProbe, SecretMode, WorkerError, WorkerSpec};

/// Name of the worker in log lines.
pub const WORKER_NAME: &str = "llama-server";

/// The server's health endpoint. It answers 503 while the model is loading
/// and 200 once it is ready, and is public even when an API key is set.
pub const HEALTH_PATH: &str = "/health";

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
    "-c",
    "--ctx-size",
    "-np",
    "--parallel",
    "-rea",
    "--reasoning",
];

/// Default context window per launch, in tokens. The server's own default is
/// the model's full trained context, which for recent models (262K for
/// Qwen3.5) costs minutes of load time and gigabytes of memory on a laptop.
/// The engine sets the real value per hardware profile.
pub const DEFAULT_CONTEXT_SIZE: u32 = 8192;

/// Whether the model may think before it answers.
///
/// The frozen chat types carry no separate reasoning channel, and the server
/// streams a reasoning model's thoughts outside the answer text. With
/// reasoning on, a caller sees nothing until thinking ends, so it is off by
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reasoning {
    #[default]
    Off,
    On,
    /// Let the server decide from the model's chat template.
    Auto,
}

impl Reasoning {
    fn as_arg(self) -> &'static str {
        match self {
            Reasoning::Off => "off",
            Reasoning::On => "on",
            Reasoning::Auto => "auto",
        }
    }
}

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
    /// Context window per launch, in tokens (`-c`).
    pub context_size: u32,
    /// Request slots (`-np`). One person uses a desktop at a time, and every
    /// slot reserves its own share of memory.
    pub parallel: u32,
    /// Thinking before answering, for chat launches (`-rea`).
    pub reasoning: Reasoning,
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
            context_size: DEFAULT_CONTEXT_SIZE,
            parallel: 1,
            reasoning: Reasoning::Off,
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
        let mut args = owned_args(addr, &self.model_path, &self.server_options());
        args.extend(self.extra_args.iter().map(OsString::from));
        args
    }

    /// This configuration as a worker spec.
    pub fn to_spec(&self) -> WorkerSpec {
        let model = self.model_path.clone();
        let options = self.server_options();
        let mut spec = WorkerSpec::new(
            WORKER_NAME,
            self.binary_path.clone(),
            self.model_path.clone(),
            move |addr| owned_args(addr, &model, &options),
            HttpProbe::get(HEALTH_PATH),
        );
        spec.extra_args = self.extra_args.clone();
        spec.reserved_args = RESERVED_ARGS.iter().map(|a| a.to_string()).collect();
        spec.secret = match self.api_key {
            ApiKeyMode::PerLaunch => SecretMode::PerLaunch {
                env_var: API_KEY_ENV.to_string(),
            },
            ApiKeyMode::Disabled => SecretMode::Disabled,
        };
        spec.env = self.env.clone();
        spec.env_allow_list = ENV_ALLOW_LIST.iter().map(|n| n.to_string()).collect();
        spec.port = self.port;
        spec.idle_unload = self.idle_unload;
        spec.start_timeout = self.start_timeout;
        spec.probe_interval = self.probe_interval;
        spec.steady_probe_interval = self.steady_probe_interval;
        spec.probe_timeout = self.probe_timeout;
        spec.stop_grace = self.stop_grace;
        spec.backoff = self.backoff;
        spec
    }

    /// Refuse a configuration that would widen the worker's environment or
    /// take over an argument the supervisor owns.
    pub fn validate(&self) -> Result<(), WorkerError> {
        self.to_spec().validate()
    }
}

/// The settings the supervisor owns besides where to listen and what to load.
#[derive(Debug, Clone, Copy)]
struct ServerOptions {
    mode: ServerMode,
    context_size: u32,
    parallel: u32,
    reasoning: Reasoning,
}

impl LlamaConfig {
    fn server_options(&self) -> ServerOptions {
        ServerOptions {
            mode: self.mode,
            context_size: self.context_size,
            parallel: self.parallel,
            reasoning: self.reasoning,
        }
    }
}

/// The arguments the supervisor owns: where to listen, what to load, what
/// the instance is for, and how much memory it may reserve.
fn owned_args(addr: SocketAddr, model: &Path, options: &ServerOptions) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--host".into(),
        addr.ip().to_string().into(),
        "--port".into(),
        addr.port().to_string().into(),
        "-m".into(),
        model.as_os_str().to_owned(),
    ];
    if options.mode == ServerMode::Embedding {
        args.push("--embeddings".into());
    }
    args.push("-c".into());
    args.push(options.context_size.to_string().into());
    args.push("-np".into());
    args.push(options.parallel.to_string().into());
    if options.mode == ServerMode::Chat {
        args.push("-rea".into());
        args.push(options.reasoning.as_arg().into());
    }
    args
}

fn default_backoff(attempt: u32) -> Duration {
    Duration::from_secs(restart_backoff_secs(attempt))
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
                "/models/m.gguf",
                "-c",
                "8192",
                "-np",
                "1",
                "-rea",
                "off"
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
        assert_eq!(
            &args[6..],
            [
                "--embeddings",
                "-c",
                "8192",
                "-np",
                "1",
                "--pooling",
                "mean"
            ]
        );
    }

    #[test]
    fn the_defaults_are_the_production_values() {
        let cfg = config();
        assert_eq!(cfg.port, None);
        assert_eq!(cfg.context_size, DEFAULT_CONTEXT_SIZE);
        assert_eq!(cfg.parallel, 1);
        assert_eq!(cfg.reasoning, Reasoning::Off);
        assert_eq!(cfg.api_key, ApiKeyMode::PerLaunch);
        assert_eq!(cfg.idle_unload, Some(Duration::from_secs(300)));
        assert_eq!(cfg.steady_probe_interval, Duration::from_secs(30));
        assert_eq!((cfg.backoff)(0), Duration::from_secs(1));
        assert_eq!((cfg.backoff)(10), Duration::from_secs(60));
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn context_slots_and_reasoning_are_set_by_the_config() {
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let mut cfg = config();
        cfg.context_size = 32768;
        cfg.parallel = 2;
        cfg.reasoning = Reasoning::On;
        let args = strings(cfg.server_args(addr));
        assert_eq!(&args[6..], ["-c", "32768", "-np", "2", "-rea", "on"]);
        cfg.reasoning = Reasoning::Auto;
        assert_eq!(strings(cfg.server_args(addr)).last().unwrap(), "auto");
    }

    #[test]
    fn variables_off_the_allow_list_are_refused() {
        let mut cfg = config();
        cfg.env = vec![("TMPDIR".to_string(), "/tmp".to_string())];
        assert!(cfg.validate().is_ok());
        for name in ["DYLD_INSERT_LIBRARIES", "LD_PRELOAD", "PATH", API_KEY_ENV] {
            cfg.env = vec![(name.to_string(), "x".to_string())];
            match cfg.validate() {
                Err(WorkerError::EnvNotAllowed(got)) => assert_eq!(got, name),
                other => panic!("expected EnvNotAllowed for {name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn reserved_arguments_are_refused_in_either_spelling() {
        for arg in [
            "--host",
            "--host=0.0.0.0",
            "--port",
            "--api-key=x",
            "-m",
            "-c",
            "--ctx-size=262144",
            "-np",
            "--reasoning=on",
        ] {
            let mut cfg = config();
            cfg.extra_args = vec![arg.to_string()];
            assert!(
                matches!(cfg.validate(), Err(WorkerError::ReservedArg(_))),
                "{arg} must be refused"
            );
        }
    }
}
