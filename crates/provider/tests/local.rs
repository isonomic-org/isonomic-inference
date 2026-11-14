//! The local provider against a fake model server and a stand-in worker.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use isonomic_inference_core::{FixedGate, ManualClock, RamGate};
use isonomic_inference_testkit::{
    process_gone, wait_for_sleepers, wait_until, write_model, write_script, FakeModelServer,
};
use isonomic_llama::{LlamaConfig, LlamaError, LlamaSupervisor, ServerMode, ServiceState};
use isonomic_provider::{
    ChatMessage, ChatRequest, EmbedRequest, GatewayError, InferenceProvider, LocalLlama,
    ProviderHealth, ProviderId,
};

const SLEEPER: &str = "#!/bin/sh\nexec /bin/sleep 3600\n";
/// Longer than any test moves the clock.
const NEVER: Duration = Duration::from_secs(100 * 24 * 3600);

struct Rig {
    dir: tempfile::TempDir,
    server: FakeModelServer,
    clock: Arc<ManualClock>,
}

impl Rig {
    async fn new() -> Self {
        let server = FakeModelServer::start().await;
        // Like the real server started with an API key: no secret, no answer.
        server.require_bearer(true);
        Self {
            dir: tempfile::tempdir().unwrap(),
            server,
            clock: Arc::new(ManualClock::new()),
        }
    }

    fn config(&self) -> LlamaConfig {
        let bin = write_script(self.dir.path(), "stand-in", SLEEPER);
        let mut config = LlamaConfig::new(bin, write_model(self.dir.path()));
        config.port = Some(self.server.port());
        config.idle_unload = None;
        config.steady_probe_interval = NEVER;
        config
    }

    fn provider(&self, config: LlamaConfig, gate: Arc<dyn RamGate>) -> LocalLlama {
        let supervisor = LlamaSupervisor::with_clock(config, gate, self.clock.clone());
        LocalLlama::new(Arc::new(supervisor), "test-model")
    }

    fn chat_provider(&self) -> LocalLlama {
        self.provider(self.config(), Arc::new(FixedGate::unlimited()))
    }
}

fn hello() -> ChatRequest {
    ChatRequest {
        model: None,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "hi".to_string(),
        }],
        temperature: None,
        max_tokens: None,
    }
}

async fn collect(provider: &LocalLlama) -> (String, Option<String>) {
    let mut stream = provider.chat(hello()).await.unwrap();
    let mut out = String::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        let chunk = item.unwrap();
        out.push_str(&chunk.delta);
        if chunk.finish_reason.is_some() {
            finish = chunk.finish_reason;
        }
    }
    (out, finish)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_chat_request_starts_the_worker_and_tokens_arrive_as_a_stream() {
    let rig = Rig::new().await;
    let provider = rig.chat_provider();

    // Nothing runs until the first inference request (lazy start).
    assert_eq!(provider.supervisor().state(), ServiceState::Stopped);
    assert_eq!(provider.health().await, ProviderHealth::Stopped);
    assert!(provider.supervisor().pid().await.is_none());

    // The server sends one event and then holds the rest back. The first
    // token must reach the caller while the response is still open, which a
    // client that buffered the whole response could not do.
    rig.server.hold_stream_after_first_event();
    let mut stream = provider.chat(hello()).await.unwrap();
    assert_eq!(provider.health().await, ProviderHealth::Healthy);
    assert!(provider.supervisor().pid().await.is_some());
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.delta, "Hello");
    assert_eq!(first.finish_reason, None);

    rig.server.release_stream();
    let mut rest = String::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        let chunk = item.unwrap();
        rest.push_str(&chunk.delta);
        finish = chunk.finish_reason.or(finish);
    }
    assert_eq!(rest, " world");
    assert_eq!(finish.as_deref(), Some("stop"));

    // The request went out as a stream request for this model.
    let sent = rig.server.requests_to("POST /v1/chat/completions");
    assert_eq!(sent.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "test-model");
}

#[tokio::test(flavor = "multi_thread")]
async fn after_the_idle_timeout_the_worker_stops_and_the_next_request_starts_it_again() {
    let rig = Rig::new().await;
    let mut config = rig.config();
    config.idle_unload = Some(Duration::from_secs(300));
    let provider = rig.provider(config, Arc::new(FixedGate::unlimited()));

    assert_eq!(collect(&provider).await.0, "Hello world");
    let first_pid = provider.supervisor().pid().await.unwrap();

    // Two half-timeout checks with no activity in between.
    for _ in 0..2 {
        wait_for_sleepers(&rig.clock, 2).await; // idle watcher (+ parked steady probe)
        rig.clock.advance(Duration::from_secs(150));
    }
    let mut state = provider.supervisor().subscribe();
    state
        .wait_for(|s| *s == ServiceState::Stopped)
        .await
        .unwrap();
    wait_until("the idle worker to exit", || process_gone(first_pid)).await;
    assert_eq!(provider.health().await, ProviderHealth::Stopped);
    assert_eq!(provider.supervisor().restart_total(), 0);

    // The next request starts it again.
    assert_eq!(collect(&provider).await.0, "Hello world");
    assert_eq!(provider.health().await, ProviderHealth::Healthy);
    assert_ne!(provider.supervisor().pid().await, Some(first_pid));
}

#[tokio::test(flavor = "multi_thread")]
async fn embeddings_come_from_a_server_started_for_embeddings() {
    let rig = Rig::new().await;
    let mut config = rig.config();
    config.mode = ServerMode::Embedding;
    let provider = rig.provider(config, Arc::new(FixedGate::unlimited()));

    let emb = provider
        .embeddings(EmbedRequest {
            model: None,
            input: vec!["x".to_string()],
        })
        .await
        .unwrap();
    assert_eq!(emb.vectors, vec![vec![0.1, 0.2]]);
    assert_eq!(provider.health().await, ProviderHealth::Healthy);

    let models = provider.models().await;
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "test-model");
    assert_eq!(models[0].capabilities, ["embeddings"]);

    // One instance does one job: this one does not chat.
    match provider.chat(hello()).await {
        Err(GatewayError::Unavailable(why)) => assert!(why.contains("embeddings"), "{why}"),
        other => panic!("expected Unavailable, got {:?}", other.map(|_| ())),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_server_lists_chat_costs_nothing_and_refuses_embeddings() {
    let rig = Rig::new().await;
    let provider = rig.chat_provider();
    assert_eq!(provider.id(), ProviderId::LocalLlama);
    assert_eq!(provider.models().await[0].capabilities, ["chat"]);
    assert_eq!(provider.cost(&hello()).usdc_micros, 0);
    let refused = provider
        .embeddings(EmbedRequest {
            model: None,
            input: vec!["x".to_string()],
        })
        .await;
    assert!(matches!(refused, Err(GatewayError::Unavailable(_))));
    // Refusing costs nothing: the worker was never started.
    assert_eq!(provider.health().await, ProviderHealth::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_carries_the_secret_of_the_current_launch() {
    let rig = Rig::new().await;
    let provider = rig.chat_provider();
    assert_eq!(collect(&provider).await.0, "Hello world");
    let key = provider
        .supervisor()
        .endpoint()
        .await
        .unwrap()
        .api_key
        .unwrap();
    let expected = format!("Bearer {key}");
    let requests = rig.server.requests();
    assert!(requests.len() >= 2, "a probe and a chat request");
    for request in &requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some(expected.as_str()),
            "{}",
            request.request
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_too_large_for_the_gate_is_refused_with_numbers_and_a_reason() {
    let rig = Rig::new().await;
    let provider = rig.provider(rig.config(), Arc::new(FixedGate::with_available(3)));
    match provider.chat(hello()).await {
        Err(GatewayError::InsufficientRam { reason }) => {
            assert!(reason.contains("not enough free memory"), "{reason}");
            assert!(reason.contains("required 4 bytes"), "{reason}");
            assert!(reason.contains("available 3 bytes"), "{reason}");
        }
        other => panic!("expected InsufficientRam, got {:?}", other.map(|_| ())),
    }
    // The worker was not started and the server never heard from us.
    assert_eq!(provider.health().await, ProviderHealth::Stopped);
    assert!(provider.supervisor().pid().await.is_none());
    assert!(rig.server.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_model_file_and_a_tripped_breaker_are_typed_errors() {
    let rig = Rig::new().await;
    let mut config = rig.config();
    config.model_path = rig.dir.path().join("not-downloaded.gguf");
    let provider = rig.provider(config, Arc::new(FixedGate::unlimited()));
    match provider.chat(hello()).await {
        Err(GatewayError::ModelMissing { path }) => assert!(path.ends_with("not-downloaded.gguf")),
        other => panic!("expected ModelMissing, got {:?}", other.map(|_| ())),
    }
    assert!(matches!(
        provider.typed_error(LlamaError::Degraded),
        GatewayError::ServerCrashed { .. }
    ));
}
