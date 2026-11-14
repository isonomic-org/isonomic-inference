//! What a worker is started with and what happens to it when its parent
//! dies: an environment built from the allow-list, loopback only, a secret
//! per launch, and a process group that does not outlive this process.
#![cfg(unix)]

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Rig, SLEEPER};
use isonomic_inference_testkit::{process_gone, read_lines, wait_until, RECORDING_STANDIN};
use isonomic_llama::{ApiKeyMode, LlamaError, ServiceState, API_KEY_ENV, ENV_ALLOW_LIST};

/// Variables a POSIX shell adds to its own environment. The stand-in is a
/// shell script, so these appear in what it records; the supervisor did not
/// pass them.
const SHELL_OWN: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

async fn recorded(dir: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let args_file = dir.join("args.txt");
    wait_until("the stand-in to record its start", || args_file.exists()).await;
    let env = read_lines(&dir.join("env.txt"))
        .into_iter()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            Some((name.to_string(), value.to_string()))
        })
        .filter(|(name, _)| !SHELL_OWN.contains(&name.as_str()))
        .collect();
    (env, read_lines(&args_file))
}

fn value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_worker_environment_contains_only_allow_listed_variables() {
    // This process has plenty in its environment that must not leak.
    assert!(std::env::var_os("PATH").is_some());
    assert!(std::env::vars_os().count() > 3);

    let rig = Rig::new().await;
    let mut config = rig.config(RECORDING_STANDIN);
    config.env = vec![("TMPDIR".to_string(), "/tmp/isonomic-test".to_string())];
    let sup = rig.supervisor(config);
    sup.ensure_running().await.unwrap();

    let (env, _) = recorded(rig.dir.path()).await;
    for (name, _) in &env {
        assert!(
            ENV_ALLOW_LIST.contains(&name.as_str()),
            "{name} reached the worker but is not on the allow-list"
        );
    }
    let mut names: Vec<&str> = env.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, [API_KEY_ENV, "TMPDIR"]);
    assert_eq!(value(&env, "TMPDIR"), Some("/tmp/isonomic-test"));
    assert!(value(&env, "PATH").is_none());
    assert!(value(&env, "HOME").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_variable_off_the_allow_list_is_refused_before_anything_starts() {
    let rig = Rig::new().await;
    let mut config = rig.config(RECORDING_STANDIN);
    config.env = vec![("DYLD_INSERT_LIBRARIES".to_string(), "/x".to_string())];
    let sup = rig.supervisor(config);
    let err = sup.ensure_running().await.unwrap_err();
    assert!(matches!(err, LlamaError::EnvNotAllowed(ref n) if n == "DYLD_INSERT_LIBRARIES"));
    assert_eq!(sup.state(), ServiceState::Stopped);
    assert!(sup.pid().await.is_none());
    assert!(!rig.dir.path().join("args.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_worker_listens_on_loopback_with_a_new_secret_for_every_launch() {
    let rig = Rig::new().await;
    let sup = rig.supervisor(rig.config(RECORDING_STANDIN));

    let first = sup.ensure_running().await.unwrap();
    let (env, args) = recorded(rig.dir.path()).await;
    let port = rig.server.port().to_string();
    assert_eq!(
        &args[..5],
        ["--host", "127.0.0.1", "--port", port.as_str(), "-m"]
    );
    assert!(first.addr.ip().is_loopback());

    // The secret reaches the server through its environment, never its
    // command line, and is sent on every request, the health probe included.
    let key = first.api_key.clone().expect("a secret per launch");
    assert_eq!(key.len(), 64);
    assert_eq!(value(&env, API_KEY_ENV), Some(key.as_str()));
    assert!(args.iter().all(|a| !a.contains(&key)));
    let probes = rig.server.requests_to("GET /health");
    assert!(!probes.is_empty());
    let expected = format!("Bearer {key}");
    assert!(probes
        .iter()
        .all(|p| p.authorization.as_deref() == Some(expected.as_str())));
    assert!(
        !format!("{first:?}").contains(&key),
        "Debug must redact the secret"
    );

    // A second launch gets a different secret.
    sup.stop().await;
    std::fs::remove_file(rig.dir.path().join("args.txt")).unwrap();
    let second = sup.ensure_running().await.unwrap();
    let (env, _) = recorded(rig.dir.path()).await;
    let second_key = second.api_key.clone().unwrap();
    assert_ne!(second_key, key);
    assert_eq!(value(&env, API_KEY_ENV), Some(second_key.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_an_api_key_option_can_run_without_a_secret() {
    let rig = Rig::new().await;
    let mut config = rig.config(RECORDING_STANDIN);
    config.api_key = ApiKeyMode::Disabled;
    let sup = rig.supervisor(config);
    let endpoint = sup.ensure_running().await.unwrap();
    assert_eq!(endpoint.api_key, None);
    let (env, _) = recorded(rig.dir.path()).await;
    assert!(env.is_empty(), "nothing at all is passed: {env:?}");
    let probes = rig.server.requests_to("GET /health");
    assert!(probes.iter().all(|p| p.authorization.is_none()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reserved_argument_is_refused_before_anything_starts() {
    let rig = Rig::new().await;
    let mut config = rig.config(RECORDING_STANDIN);
    config.extra_args = vec!["--host=0.0.0.0".to_string()];
    let sup = rig.supervisor(config);
    let err = sup.ensure_running().await.unwrap_err();
    assert!(matches!(err, LlamaError::ReservedArg(_)), "{err:?}");
    assert!(!rig.dir.path().join("args.txt").exists());
}

const HELPER_DIR: &str = "ISONOMIC_TEST_HELPER_DIR";

/// Not a test of its own: the body of the stand-in parent process that
/// `the_worker_dies_with_its_parent` starts and then kills.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "helper process for the_worker_dies_with_its_parent"]
async fn helper_parent_process() {
    let Some(dir) = std::env::var_os(HELPER_DIR) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let rig = Rig::new().await;
    let sup = rig.supervisor(rig.config(SLEEPER));
    sup.ensure_running().await.unwrap();
    let pid = sup.pid().await.unwrap();
    std::fs::write(dir.join("worker.pid.tmp"), pid.to_string()).unwrap();
    std::fs::rename(dir.join("worker.pid.tmp"), dir.join("worker.pid")).unwrap();
    // Stay alive, supervising, until killed from outside.
    tokio::time::sleep(Duration::from_secs(3600)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_worker_dies_with_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let mut parent = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "helper_parent_process",
            "--ignored",
            "--nocapture",
        ])
        .env(HELPER_DIR, dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let pid_file = dir.path().join("worker.pid");
    wait_until("the parent to start its worker", || pid_file.exists()).await;
    let worker: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    assert!(!process_gone(worker), "the worker should be running");

    // Kill the parent outright: no destructor runs, no signal is forwarded.
    parent.kill().unwrap();
    parent.wait().unwrap();

    wait_until("the orphaned worker to be killed", || process_gone(worker)).await;
}
