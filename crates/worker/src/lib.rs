//! Lifecycle of a model worker process.
//!
//! Spec: docs/spec.md. Status and deviations: docs/status.md.
//!
//! A worker is an external program that serves one model. This crate starts
//! it lazily on the first request, probes it until healthy, probes it again
//! at steady state, restarts it with backoff when it crashes, unloads it
//! when idle, and refuses to start it when the memory gate says no. The
//! worker listens on loopback only, with a port and a secret chosen at each
//! launch, an environment built from an allow-list, and a process group that
//! dies with this process.
//!
//! What differs between kinds of worker is a [`WorkerSpec`]: the binary, its
//! arguments and its health probe. `isonomic-llama` is the first user.

mod health;
mod process;
mod spec;
mod state;
mod supervisor;
mod tasks;

pub use health::probe_http;
pub use spec::{HealthProbe, HttpProbe, SecretMode, WorkerError, WorkerSpec};
pub use state::{restart_backoff_secs, should_trip_breaker, ServiceState};
pub use supervisor::{Endpoint, WorkerSupervisor, LOG_RING_CAPACITY};
pub use tasks::BREAKER_WINDOW;
