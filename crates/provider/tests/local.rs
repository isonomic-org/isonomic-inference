// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

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
    ChatMessage, ChatRequest, ChatTool, ChatToolCall, EmbedRequest, GatewayError,
    InferenceProvider, LocalLlama, ProviderHealth, ProviderId, ReasoningChoice, ReasoningPolicy,
    ToolCallDelta,
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
        messages: vec![ChatMessage::text("user", "hi")],
        ..ChatRequest::default()
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

/// Send one chat request asking for `choice`; returns the reasoning text,
/// the answer text and the body the server received.
async fn ask(
    rig: &Rig,
    policy: ReasoningPolicy,
    choice: Option<ReasoningChoice>,
) -> (String, String, serde_json::Value) {
    let provider = rig.chat_provider().with_reasoning_policy(policy);
    let mut request = hello();
    request.reasoning = choice;
    let mut stream = provider.chat(request).await.unwrap();
    let (mut reasoning, mut answer) = (String::new(), String::new());
    while let Some(item) = stream.next().await {
        let chunk = item.unwrap();
        reasoning.push_str(chunk.reasoning.as_deref().unwrap_or_default());
        answer.push_str(&chunk.delta);
    }
    let sent = rig.server.requests_to("POST /v1/chat/completions");
    let body = serde_json::from_str(&sent.last().unwrap().body).unwrap();
    provider.supervisor().stop().await;
    (reasoning, answer, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_can_turn_reasoning_on_and_it_streams_apart_from_the_answer() {
    let rig = Rig::new().await;
    let on = Some(ReasoningChoice::On);
    let (reasoning, answer, body) = ask(&rig, ReasoningPolicy::PerRequest, on).await;
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
    assert_eq!(reasoning, "Thinking.");
    assert_eq!(answer, "Hello world");
    assert!(body.get("reasoning").is_none());

    let off = Some(ReasoningChoice::Off);
    let (reasoning, answer, body) = ask(&rig, ReasoningPolicy::PerRequest, off).await;
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!((reasoning.as_str(), answer.as_str()), ("", "Hello world"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unset_choice_keeps_the_launch_default() {
    let rig = Rig::new().await;
    let (reasoning, _, body) = ask(&rig, ReasoningPolicy::PerRequest, None).await;
    assert!(body.get("chat_template_kwargs").is_none());
    assert_eq!(reasoning, "");
}

#[tokio::test(flavor = "multi_thread")]
async fn on_intel_macs_reasoning_stays_off_whatever_the_request_asks() {
    let rig = Rig::new().await;
    for choice in [None, Some(ReasoningChoice::On)] {
        let (reasoning, answer, body) = ask(&rig, ReasoningPolicy::AlwaysOff, choice).await;
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!((reasoning.as_str(), answer.as_str()), ("", "Hello world"));
    }
    let intel = cfg!(all(target_os = "macos", target_arch = "x86_64"));
    let expected = if intel {
        ReasoningPolicy::AlwaysOff
    } else {
        ReasoningPolicy::PerRequest
    };
    assert_eq!(ReasoningPolicy::for_this_machine(), expected);
    assert_eq!(rig.chat_provider().reasoning_policy(), expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_go_out_on_the_request_and_streamed_tool_calls_come_back() {
    let rig = Rig::new().await;
    let provider = rig.chat_provider();
    let read_file = ChatTool {
        name: "read_file".to_string(),
        description: "Read a file.".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    };
    // A follow-up turn: an earlier call and its result, with tools offered.
    let request = ChatRequest {
        messages: vec![
            ChatMessage::text("user", "Read a.txt"),
            ChatMessage::assistant_tool_calls(
                "",
                vec![ChatToolCall {
                    id: "call_0".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"a.txt"}"#.to_string(),
                }],
            ),
            ChatMessage::tool_result("call_0", "not found"),
        ],
        tools: vec![read_file],
        tool_choice: Some("auto".to_string()),
        ..ChatRequest::default()
    };
    let mut stream = provider.chat(request).await.unwrap();
    let mut deltas: Vec<ToolCallDelta> = Vec::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        let chunk = item.unwrap();
        assert_eq!(chunk.delta, "");
        deltas.extend(chunk.tool_calls);
        finish = chunk.finish_reason.or(finish);
    }
    assert_eq!(finish.as_deref(), Some("tool_calls"));
    let starts: Vec<_> = deltas
        .iter()
        .filter_map(|d| Some((d.index, d.id.clone()?, d.name.clone()?)))
        .collect();
    assert_eq!(
        starts,
        [
            (0, "call_a".to_string(), "read_file".to_string()),
            (1, "call_b".to_string(), "list_dir".to_string()),
        ]
    );
    let args = |index: u32| -> String {
        deltas
            .iter()
            .filter(|d| d.index == index)
            .map(|d| d.arguments.as_str())
            .collect()
    };
    assert_eq!(args(0), r#"{"path":"a.txt"}"#);
    assert_eq!(args(1), r#"{"dir":"."}"#);

    let sent = rig.server.requests_to("POST /v1/chat/completions");
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(body["tool_choice"], "auto");
    let call = &body["messages"][1]["tool_calls"][0];
    assert_eq!(call["id"], "call_0");
    assert_eq!(call["function"]["arguments"], r#"{"path":"a.txt"}"#);
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["messages"][2]["tool_call_id"], "call_0");
}
