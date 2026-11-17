// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Starting and stopping the worker process.
//!
//! A worker is started with an environment built from scratch, in its own
//! process group, with a guardian that kills the group if this process dies.

use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::Stdio;

use tokio::process::{Child, Command};

/// A worker process that has just been started.
pub(crate) struct Spawned {
    pub child: Child,
    pub pid: u32,
    pub guardian: Guardian,
}

/// Start `binary` with exactly `env` as its environment. Nothing is inherited
/// from this process. The worker leads its own process group so teardown can
/// signal the whole tree.
pub(crate) fn spawn_worker(
    binary: &Path,
    args: &[OsString],
    env: &[(String, String)],
) -> std::io::Result<Spawned> {
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    if pid == 0 {
        return Err(std::io::Error::other("worker exited before it had a pid"));
    }
    match Guardian::watch(pid) {
        Ok(guardian) => Ok(Spawned {
            child,
            pid,
            guardian,
        }),
        Err(err) => {
            // No guardian means the worker could outlive us: refuse to run it.
            // The worker is not reaped yet, so its group id is still ours.
            kill_group_now(pid);
            Err(err)
        }
    }
}

/// Makes the worker die with its parent.
///
/// A small `/bin/sh` process holds the read end of a pipe whose only write
/// end lives in this process. If this process dies for any reason, including
/// SIGKILL, the pipe closes and the guardian kills the worker's process
/// group. The guardian is itself a member of that group, which keeps the
/// group id reserved for as long as the guardian lives, so signalling the
/// group can never hit an unrelated process. It ignores SIGTERM so that it
/// survives a graceful stop of the group and keeps that guarantee until the
/// worker is gone.
pub(crate) struct Guardian {
    #[cfg(unix)]
    child: Child,
    #[cfg(unix)]
    _alive: std::io::PipeWriter,
}

#[cfg(unix)]
// `kill -s KILL -- -<pgid>` is the POSIX spelling. Shorter forms are not
// portable: dash rejects `kill -KILL -- -<pgid>`.
const GUARDIAN_SCRIPT: &str = "trap '' TERM INT HUP; read ignored; kill -s KILL -- \"-$1\"";

impl Guardian {
    #[cfg(unix)]
    fn watch(pgid: u32) -> std::io::Result<Self> {
        let (reader, writer) = std::io::pipe()?;
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(GUARDIAN_SCRIPT)
            .arg("isonomic-worker-guardian")
            .arg(pgid.to_string())
            .env_clear()
            .stdin(Stdio::from(reader))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(pgid as i32);
        let child = cmd.spawn()?;
        Ok(Self {
            child,
            _alive: writer,
        })
    }

    #[cfg(not(unix))]
    fn watch(_pgid: u32) -> std::io::Result<Self> {
        Ok(Self {})
    }

    /// Whether the guardian is still running, which also means the worker's
    /// process group id is still reserved.
    pub(crate) fn is_alive(&mut self) -> bool {
        #[cfg(unix)]
        {
            matches!(self.child.try_wait(), Ok(None))
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

/// Kill whatever is left of the worker's process group once its leader has
/// exited. Only done while the guardian is alive, because only then is the
/// group id guaranteed to still be ours.
pub(crate) fn reap_group(guardian: &mut Guardian, pgid: u32) {
    if guardian.is_alive() {
        kill_group_now(pgid);
    }
}

#[cfg(unix)]
fn signal_group(pgid: u32, signal: libc::c_int) {
    if pgid != 0 {
        // SAFETY: kill(2) has no memory-safety preconditions. A negative pid
        // addresses the process group; zero is excluded above because it
        // would address our own group.
        unsafe {
            libc::kill(-(pgid as libc::pid_t), signal);
        }
    }
}

/// SIGKILL the whole process group.
pub(crate) fn kill_group_now(pgid: u32) {
    #[cfg(unix)]
    signal_group(pgid, libc::SIGKILL);
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Ask the whole process group to stop (SIGTERM).
pub(crate) fn stop_group(pgid: u32) {
    #[cfg(unix)]
    signal_group(pgid, libc::SIGTERM);
    #[cfg(not(unix))]
    let _ = pgid;
}

/// SIGKILL one process.
pub(crate) fn kill_pid(pid: u32) {
    #[cfg(unix)]
    if pid != 0 {
        // SAFETY: kill(2) has no memory-safety preconditions.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// The address a launch listens on: always loopback. With no port given, the
/// operating system picks a free one.
pub(crate) fn alloc_loopback_addr(port: Option<u16>) -> std::io::Result<SocketAddr> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    match port {
        Some(p) => Ok(SocketAddr::new(ip, p)),
        None => {
            // Bind port 0, record what the OS gave us. (Small race window
            // between drop and the worker binding it: accepted; the worker
            // fails fast and the crash path picks a new port.)
            let listener = std::net::TcpListener::bind((ip, 0))?;
            let addr = listener.local_addr()?;
            drop(listener);
            Ok(addr)
        }
    }
}

/// A fresh random secret: 32 bytes from the operating system, as hex.
pub(crate) fn new_secret() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| std::io::Error::other(err.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unspecified_port_is_a_free_loopback_port() {
        let addr = alloc_loopback_addr(None).unwrap();
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0);
        let fixed = alloc_loopback_addr(Some(4242)).unwrap();
        assert!(fixed.ip().is_loopback());
        assert_eq!(fixed.port(), 4242);
    }

    #[test]
    fn secrets_are_long_hex_and_never_repeat() {
        let a = new_secret().unwrap();
        let b = new_secret().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
