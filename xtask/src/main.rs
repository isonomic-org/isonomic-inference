//! Repository tasks. Run with `cargo xtask <task>`.

mod args;
mod models;
mod smoke;

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  smoke --server <path> --model <gguf> [--embedding-model <gguf>]
        [--max-tokens <n>] [--start-timeout <secs>]
      Run one chat completion and one embedding against a real model server
      and print a report to paste back.

  models
      List the models in the library manifest.

  fetch <model-id> [--dir <dir>]
      Download a model from the manifest and verify its hash.

  help
      Print this message.
";

fn main() -> ExitCode {
    let mut argv = std::env::args().skip(1);
    let task = argv.next();
    let rest: Vec<String> = argv.collect();
    let outcome = match task.as_deref() {
        None | Some("help") | Some("--help") | Some("-h") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("smoke") => smoke::run(&rest),
        Some("models") => models::list(),
        Some("fetch") => models::fetch(&rest),
        Some(other) => Err(format!("unknown task: {other}\n\n{USAGE}")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// A runtime for the tasks that need one.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(future)
}
