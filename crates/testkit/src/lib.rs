//! Test support for Isonomic Inference.
//!
//! Hermetic tests are the default in this workspace: stand-in binaries
//! instead of a real model server, a fake server that speaks the same HTTP
//! surface, and a clock the test moves by hand. Nothing here touches the
//! network beyond loopback, the machine's memory, or a GPU.

pub mod fake_server;
pub mod standin;
pub mod wait;

pub use fake_server::{FakeModelServer, Recorded};
pub use standin::{process_gone, read_lines, write_model, write_script, RECORDING_STANDIN};
pub use wait::{pump_until, wait_for_sleepers, wait_until};
