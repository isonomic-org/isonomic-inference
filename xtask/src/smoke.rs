//! `cargo xtask smoke`: one chat completion and one embedding against a real
//! model server and a real model, with a report meant to be pasted back.
//!
//! This is the one test in the repo that is not hermetic, and it is opt-in.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use isonomic_inference_core::{ChatMessage, ChatRequest, EmbedRequest, FixedGate};
use isonomic_llama::{LlamaConfig, LlamaSupervisor, ServerMode};
use isonomic_provider::{InferenceProvider, LocalLlama};

use crate::args::Args;

struct Options {
    server: PathBuf,
    model: PathBuf,
    embedding_model: Option<PathBuf>,
    max_tokens: u32,
    start_timeout: Duration,
}

pub fn run(argv: &[String]) -> Result<(), String> {
    let known = [
        "server",
        "model",
        "embedding-model",
        "max-tokens",
        "start-timeout",
    ];
    let args = Args::parse(argv, &known)?;
    let options = Options {
        server: PathBuf::from(args.require("server")?),
        model: PathBuf::from(args.require("model")?),
        embedding_model: args.get("embedding-model").map(PathBuf::from),
        max_tokens: args.number("max-tokens", 64)? as u32,
        start_timeout: Duration::from_secs(args.number("start-timeout", 300)?),
    };
    for path in [
        Some(&options.server),
        Some(&options.model),
        options.embedding_model.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if !path.is_file() {
            return Err(format!("not a file: {}", path.display()));
        }
    }
    let (report, passed) = crate::block_on(smoke(&options));
    print!("{report}");
    if passed {
        Ok(())
    } else {
        Err("smoke test failed; the report above has the last log lines".to_string())
    }
}

async fn smoke(options: &Options) -> (String, bool) {
    let mut out = String::new();
    let _ = writeln!(out, "===== isonomic-inference smoke =====");
    row(&mut out, "repo version", env!("CARGO_PKG_VERSION"));
    let host = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    row(&mut out, "host", &host);
    row(&mut out, "server", &options.server.display().to_string());
    row(&mut out, "server version", &server_version(&options.server));
    row(&mut out, "model", &describe(&options.model));

    let _ = writeln!(out, "\nchat");
    let chat_ok = match chat_phase(options, &mut out).await {
        Ok(()) => true,
        Err(failure) => {
            fail(&mut out, &failure);
            false
        }
    };

    let _ = writeln!(out, "\nembedding");
    let embed_ok = match embed_phase(options, &mut out).await {
        Ok(()) => true,
        Err(failure) => {
            fail(&mut out, &failure);
            false
        }
    };

    let passed = chat_ok && embed_ok;
    let _ = writeln!(out, "\nresult: {}", if passed { "PASS" } else { "FAIL" });
    let _ = writeln!(out, "===== end =====");
    (out, passed)
}

struct Failure {
    error: String,
    logs: Vec<String>,
}

fn row(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(out, "  {label:<18} {value}");
}

fn fail(out: &mut String, failure: &Failure) {
    row(out, "result", &format!("FAILED: {}", failure.error));
    let _ = writeln!(out, "  last log lines:");
    for line in &failure.logs {
        let _ = writeln!(out, "    {line}");
    }
}

fn describe(path: &Path) -> String {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    format!("{} ({:.2} GB)", path.display(), size as f64 / 1e9)
}

/// First lines of `<server> --version`, run with an empty environment.
fn server_version(server: &Path) -> String {
    let output = std::process::Command::new(server)
        .arg("--version")
        .env_clear()
        .output();
    match output {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stderr).to_string();
            text.push_str(&String::from_utf8_lossy(&output.stdout));
            let lines: Vec<&str> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(2)
                .collect();
            if lines.is_empty() {
                "(no output from --version)".to_string()
            } else {
                lines.join(" | ")
            }
        }
        Err(err) => format!("(could not run --version: {err})"),
    }
}

fn supervisor(
    options: &Options,
    model: &Path,
    mode: ServerMode,
    extra: &[&str],
) -> LlamaSupervisor {
    let mut config = LlamaConfig::new(options.server.clone(), model.to_path_buf());
    config.mode = mode;
    config.idle_unload = None;
    config.start_timeout = options.start_timeout;
    config.probe_interval = Duration::from_millis(250);
    config.extra_args = extra.iter().map(|s| s.to_string()).collect();
    LlamaSupervisor::new(config, Arc::new(FixedGate::unlimited()))
}

fn failure(supervisor: &LlamaSupervisor, error: impl ToString) -> Failure {
    Failure {
        error: error.to_string(),
        logs: supervisor.last_logs(),
    }
}

async fn chat_phase(options: &Options, out: &mut String) -> Result<(), Failure> {
    let supervisor = Arc::new(supervisor(options, &options.model, ServerMode::Chat, &[]));
    let provider = LocalLlama::new(Arc::clone(&supervisor), "smoke");
    let result = chat(options, &supervisor, &provider, out).await;
    supervisor.stop().await;
    result.map_err(|error| failure(&supervisor, error))
}

async fn chat(
    options: &Options,
    supervisor: &LlamaSupervisor,
    provider: &LocalLlama,
    out: &mut String,
) -> Result<(), String> {
    let started = Instant::now();
    let endpoint = supervisor
        .ensure_running()
        .await
        .map_err(|e| e.to_string())?;
    row(
        out,
        "load time",
        &format!("{:.2} s", started.elapsed().as_secs_f64()),
    );
    row(out, "listening on", &endpoint.addr.to_string());

    // The server must refuse a request that does not carry the secret.
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let bare = client
        .get(format!("{}/v1/models", endpoint.base_url()))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .status();
    let enforced = bare == reqwest::StatusCode::UNAUTHORIZED;
    let verdict = if enforced { "yes" } else { "NO" };
    row(
        out,
        "secret enforced",
        &format!("{verdict} (request without it: {})", bare.as_u16()),
    );
    if !enforced {
        return Err("the server answered a request that carried no secret".to_string());
    }

    let request = ChatRequest {
        model: None,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "Say hello in one short sentence.".to_string(),
        }],
        temperature: Some(0.0),
        max_tokens: Some(options.max_tokens),
        reasoning: None,
    };
    let asked = Instant::now();
    let mut stream = provider.chat(request).await.map_err(|e| e.to_string())?;
    let mut text = String::new();
    let mut tokens = 0u32;
    let mut first_token = None;
    let mut finish = None;
    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| e.to_string())?;
        if !chunk.delta.is_empty() {
            tokens += 1;
            first_token.get_or_insert_with(Instant::now);
            text.push_str(&chunk.delta);
        }
        finish = chunk.finish_reason.or(finish);
    }
    let done = Instant::now();
    let Some(first) = first_token else {
        return Err("the stream ended without a single token".to_string());
    };
    let generating = done.duration_since(first).as_secs_f64();
    let rate = if tokens > 1 && generating > 0.0 {
        format!("{:.1} (stream chunks)", f64::from(tokens - 1) / generating)
    } else {
        "n/a (one token)".to_string()
    };
    row(
        out,
        "first token",
        &format!("{:.2} s", first.duration_since(asked).as_secs_f64()),
    );
    row(
        out,
        "tokens",
        &format!(
            "{tokens} in {:.2} s",
            done.duration_since(asked).as_secs_f64()
        ),
    );
    row(out, "tokens per second", &rate);
    row(out, "finish reason", finish.as_deref().unwrap_or("(none)"));
    let shown: String = text.chars().take(120).collect();
    row(out, "text", &format!("{shown:?}"));
    row(out, "result", "ok");
    Ok(())
}

async fn embed_phase(options: &Options, out: &mut String) -> Result<(), Failure> {
    // A server instance does one job, so embeddings get their own launch.
    // Without a dedicated embedding model, the chat model is reused with
    // mean pooling.
    let (model, extra, label): (&Path, &[&str], String) = match &options.embedding_model {
        Some(path) => (path, &[], describe(path)),
        None => (
            &options.model,
            &["--pooling", "mean"],
            "the chat model, mean pooling".to_string(),
        ),
    };
    row(out, "model", &label);
    let supervisor = Arc::new(supervisor(options, model, ServerMode::Embedding, extra));
    let provider = LocalLlama::new(Arc::clone(&supervisor), "smoke-embed");
    let result = embed(&supervisor, &provider, out).await;
    supervisor.stop().await;
    result.map_err(|error| failure(&supervisor, error))
}

async fn embed(
    supervisor: &LlamaSupervisor,
    provider: &LocalLlama,
    out: &mut String,
) -> Result<(), String> {
    let started = Instant::now();
    supervisor
        .ensure_running()
        .await
        .map_err(|e| e.to_string())?;
    row(
        out,
        "load time",
        &format!("{:.2} s", started.elapsed().as_secs_f64()),
    );
    let asked = Instant::now();
    let embeddings = provider
        .embeddings(EmbedRequest {
            model: None,
            input: vec!["hello world".to_string()],
        })
        .await
        .map_err(|e| e.to_string())?;
    let dimensions = embeddings.vectors.first().map(Vec::len).unwrap_or(0);
    if dimensions == 0 {
        return Err("the server returned no embedding".to_string());
    }
    row(
        out,
        "vectors",
        &format!("{} × {dimensions}", embeddings.vectors.len()),
    );
    row(
        out,
        "time",
        &format!("{:.2} s", asked.elapsed().as_secs_f64()),
    );
    row(out, "result", "ok");
    Ok(())
}
