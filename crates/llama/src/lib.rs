//! Supervision of the llama.cpp server as a worker process.
//!
//! Spec: docs/spec.md. Status and deviations: docs/status.md.
//!
//! The worker is started lazily on the first request, probed until healthy,
//! probed again at steady state, restarted with backoff when it crashes,
//! unloaded when idle, and refused outright when the memory gate says no. It
//! listens on loopback only, with a port and a secret chosen at each launch,
//! an environment built from an allow-list, and a process group that dies
//! with this process.

mod config;
mod health;
mod process;
mod state;
mod supervisor;
mod tasks;

pub use config::{
    ApiKeyMode, LlamaConfig, LlamaError, ServerMode, API_KEY_ENV, ENV_ALLOW_LIST, RESERVED_ARGS,
};
pub use health::probe_http;
pub use state::{restart_backoff_secs, should_trip_breaker, ServiceState};
pub use supervisor::{Endpoint, LlamaSupervisor, LOG_RING_CAPACITY};
pub use tasks::BREAKER_WINDOW;
