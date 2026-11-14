//! Repository tasks. Run with `cargo xtask <task>`.

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  help    print this message
";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        None | Some("help") | Some("--help") | Some("-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown task: {other}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
