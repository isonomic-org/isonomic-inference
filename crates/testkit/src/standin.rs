//! Stand-in binaries: small shell scripts that take the place of the real
//! model server. Scripts use absolute paths only, because the worker's
//! environment has no PATH.

use std::path::{Path, PathBuf};

/// A stand-in that records how it was started, then stays alive. `{dir}` is
/// replaced with the directory to write `env.txt` and `args.txt` into;
/// `args.txt` is written last, so its presence means both are complete.
pub const RECORDING_STANDIN: &str = "#!/bin/sh\n\
/usr/bin/env > \"{dir}/env.txt\"\n\
printf '%s\\n' \"$@\" > \"{dir}/args.txt\"\n\
exec /bin/sleep 3600\n";

/// Write an executable script.
#[cfg(unix)]
pub fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let body = body.replace("{dir}", &dir.display().to_string());
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Write a small file standing in for a model.
pub fn write_model(dir: &Path) -> PathBuf {
    let path = dir.join("model.gguf");
    std::fs::write(&path, b"gguf").unwrap();
    path
}

/// The non-empty lines of a file, or nothing if it does not exist yet.
pub fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Whether process `pid` is gone: no such process, or exited and only
/// waiting to be reaped by whoever inherited it.
#[cfg(unix)]
pub fn process_gone(pid: u32) -> bool {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output();
    match out {
        Ok(out) => {
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            stat.is_empty() || stat.starts_with('Z')
        }
        Err(_) => false,
    }
}
