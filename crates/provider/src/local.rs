//! `LocalLlama` provider: supervises the llama.cpp server through
//! `isonomic-llama` (lazy start on the first inference request, idle unload,
//! memory gate) and forwards OpenAI-compatible requests to it, streaming
//! tokens back as they arrive.

use std::sync::Arc;

use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::StreamExt;
use isonomic_inference_core::{
    Capability, ChatRequest, CostEstimate, EmbedRequest, Embeddings, ModelInfo, ModelMetadata,
    ProviderHealth, ProviderId,
};
use isonomic_llama::{Endpoint, LlamaError, LlamaSupervisor, ServerMode, ServiceState};

use crate::provider::{ChatStream, GatewayError, InferenceProvider};
use crate::sse::{parse_sse_line, LineBuffer, SseEvent};

pub struct LocalLlama {
    supervisor: Arc<LlamaSupervisor>,
    model_name: String,
    context_length: Option<u32>,
    client: reqwest::Client,
}

impl LocalLlama {
    pub fn new(supervisor: Arc<LlamaSupervisor>, model_name: impl Into<String>) -> Self {
        Self {
            supervisor,
            model_name: model_name.into(),
            context_length: None,
            // Requests go to loopback only. Proxy settings in the environment
            // must never be able to route them anywhere else.
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("a client without a proxy always builds"),
        }
    }

    /// A provider for a model from the library.
    pub fn for_model(supervisor: Arc<LlamaSupervisor>, model: &ModelMetadata) -> Self {
        let mut provider = Self::new(supervisor, model.id.as_str());
        provider.context_length = model.context_length;
        provider
    }

    pub fn supervisor(&self) -> &Arc<LlamaSupervisor> {
        &self.supervisor
    }

    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// What the supervised server instance can do. One instance does one
    /// job: chat, or embeddings.
    pub fn capability(&self) -> Capability {
        match self.supervisor.config().mode {
            ServerMode::Chat => Capability::Chat,
            ServerMode::Embedding => Capability::Embeddings,
        }
    }

    fn require(&self, wanted: Capability) -> Result<(), GatewayError> {
        if self.capability() == wanted {
            return Ok(());
        }
        Err(GatewayError::Unavailable(format!(
            "model {} is served for {}, not {}",
            self.model_name,
            self.capability().as_str(),
            wanted.as_str()
        )))
    }

    async fn ensure_endpoint(&self) -> Result<Endpoint, GatewayError> {
        // Lazy start on first inference request; typed errors let a caller
        // render exactly what is wrong.
        let endpoint = self
            .supervisor
            .ensure_running()
            .await
            .map_err(|e| self.typed_error(e))?;
        self.supervisor.touch();
        Ok(endpoint)
    }

    /// Map supervisor errors to the typed gateway states.
    pub fn typed_error(&self, error: LlamaError) -> GatewayError {
        match error {
            LlamaError::RamDenied(denial) => GatewayError::InsufficientRam {
                reason: denial.to_string(),
            },
            LlamaError::Degraded | LlamaError::StartFailed => GatewayError::ServerCrashed {
                last_logs: self.supervisor.last_logs(),
            },
            LlamaError::Io(err) if err.kind() == std::io::ErrorKind::NotFound => {
                GatewayError::ModelMissing {
                    path: self.supervisor.model_path().display().to_string(),
                }
            }
            other => GatewayError::Unavailable(other.to_string()),
        }
    }

    fn request_body(
        &self,
        value: Result<serde_json::Value, serde_json::Error>,
        model: &Option<String>,
        stream: bool,
    ) -> Result<serde_json::Value, GatewayError> {
        let mut body = value.map_err(|e| GatewayError::Protocol(e.to_string()))?;
        if stream {
            body["stream"] = serde_json::Value::Bool(true);
        }
        if model.is_none() {
            body["model"] = serde_json::Value::String(self.model_name.clone());
        }
        Ok(body)
    }

    /// A POST to the running server, carrying this launch's secret.
    fn post(&self, endpoint: &Endpoint, path: &str) -> reqwest::RequestBuilder {
        let request = self.client.post(format!("{}{path}", endpoint.base_url()));
        match &endpoint.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }
}

#[async_trait]
impl InferenceProvider for LocalLlama {
    fn id(&self) -> ProviderId {
        ProviderId::LocalLlama
    }

    async fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: self.model_name.clone(),
            context_length: self.context_length,
            capabilities: vec![self.capability().as_str().to_string()],
        }]
    }

    async fn health(&self) -> ProviderHealth {
        match self.supervisor.state() {
            ServiceState::Stopped => ProviderHealth::Stopped,
            ServiceState::Starting => ProviderHealth::Starting,
            ServiceState::Healthy => ProviderHealth::Healthy,
            ServiceState::Degraded => ProviderHealth::Degraded,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, GatewayError> {
        self.require(Capability::Chat)?;
        let endpoint = self.ensure_endpoint().await?;
        let body = self.request_body(serde_json::to_value(&req), &req.model, true)?;
        let response = self
            .post(&endpoint, "/v1/chat/completions")
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        let supervisor = Arc::clone(&self.supervisor);
        let stream = try_stream! {
            let mut bytes = response.bytes_stream();
            let mut lines = LineBuffer::default();
            'outer: while let Some(chunk) = bytes.next().await {
                let chunk = chunk?;
                for line in lines.push(&chunk) {
                    match parse_sse_line(&line)? {
                        SseEvent::Chunk(c) => {
                            // Active streams count as activity for idle unload.
                            supervisor.touch();
                            yield c;
                        }
                        SseEvent::Done => break 'outer,
                        SseEvent::Ignore => {}
                    }
                }
            }
        };
        Ok(Box::pin(stream) as ChatStream)
    }

    async fn embeddings(&self, req: EmbedRequest) -> Result<Embeddings, GatewayError> {
        self.require(Capability::Embeddings)?;
        let endpoint = self.ensure_endpoint().await?;
        let body = self.request_body(serde_json::to_value(&req), &req.model, false)?;
        let response = self
            .post(&endpoint, "/v1/embeddings")
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let value: serde_json::Value = response.json().await?;
        let data = value["data"]
            .as_array()
            .ok_or_else(|| GatewayError::Protocol("missing data array".to_string()))?;
        let mut vectors = Vec::with_capacity(data.len());
        for item in data {
            let embedding = item["embedding"]
                .as_array()
                .ok_or_else(|| GatewayError::Protocol("missing embedding".to_string()))?;
            vectors.push(
                embedding
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect(),
            );
        }
        Ok(Embeddings { vectors })
    }

    fn cost(&self, _req: &ChatRequest) -> CostEstimate {
        // Local inference is free.
        CostEstimate { usdc_micros: 0 }
    }
}
