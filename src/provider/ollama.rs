//! Ollama routing and Chat Completions compatibility policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{ResultExt, ensure};

use super::http::ensure_success;
use super::openai::ModelList;
use super::{
    Effort, EmbeddingModel, EmbeddingProviderImpl, EmbeddingRequest, EmbeddingResponse, Message,
    Model, ModelInfo, ProviderEvent, ProviderImpl, RequestOptions, Result, StreamResponse,
    ThinkingConfig, Tool, error,
};
use crate::protocol::{
    EmbeddingProtocol, Protocol, ProtocolInput,
    openai::{ChatCompletions, ChatReasoningField, ChatTokenLimit, OpenAiOptions},
};

/// Serializable local Ollama settings; no credentials are required.
#[derive(crate::StructWithCustomDefault!, PartialEq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct OllamaConfig {
    /// API root prepended to request paths.
    #[builder(default = "http://localhost:11434/v1".to_owned())]
    pub base_url: String,
    /// Optional metadata for models returned by the server.
    pub catalog: Option<crate::catalog::ProviderCatalog>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Local Ollama provider over its Chat Completions compatibility surface.
#[derive(Debug)]
pub struct Ollama {
    config: OllamaConfig,
    client: reqwest::Client,
}

impl Ollama {
    /// Binds the provider to its settings without probing the server.
    pub fn new(config: OllamaConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Replaces the runtime HTTP client without changing serializable settings.
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response> {
        let mut request = self.client.request(
            method,
            format!("{}/{}", self.config.base_url.trim_end_matches('/'), path),
        );
        if let Some(body) = body {
            request = request
                .header(
                    "Accept",
                    if path == "chat/completions" {
                        "text/event-stream"
                    } else {
                        "application/json"
                    },
                )
                .json(&body);
        }
        ensure_success(request.send().await.context(error::TransportSnafu)?).await
    }
}

#[async_trait]
impl ProviderImpl for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    async fn stream_message(
        &self,
        model: &Model,
        messages: &[Message],
        system: &str,
        tools: &[Tool],
        event_tx: &Sender<ProviderEvent>,
        options: RequestOptions,
    ) -> Result<StreamResponse> {
        ensure!(
            model.provider.name() == ProviderImpl::name(self),
            error::ModelProviderMismatchSnafu {
                expected: ProviderImpl::name(self),
                actual: model.provider.name(),
            }
        );
        ensure!(
            !options.fast && model.overrides.fast != Some(true),
            error::FastModeUnsupportedSnafu {
                provider: ProviderImpl::name(self)
            }
        );
        let thinking = &model.info.features.thinking;
        let supported = model.thinking_supported();
        let required = model.thinking_required();
        let effort = match &options.thinking {
            ThinkingConfig::Off if required => return error::ReasoningRequiredSnafu.fail(),
            ThinkingConfig::Off => Some("none"),
            ThinkingConfig::Adaptive => None,
            ThinkingConfig::Effort(Effort::Adaptive) if supported => None,
            ThinkingConfig::Effort(effort) if supported && thinking.efforts.contains(effort) => {
                effort.openai_wire()
            }
            ThinkingConfig::Budget(_) => return error::ReasoningBudgetUnsupportedSnafu.fail(),
            ThinkingConfig::Effort(effort) => {
                return error::UnsupportedReasoningEffortSnafu {
                    effort: effort.clone(),
                }
                .fail();
            }
        };
        if let Some(limit) = options
            .max_output_tokens
            .or(model.info.config.max_output_tokens)
        {
            ensure!(limit > 0, error::ZeroOutputTokenLimitSnafu);
        }
        let protocol_options = OpenAiOptions::builder()
            .maybe_reasoning_effort(effort)
            .replay_reasoning_content(true)
            .chat_reasoning_field(ChatReasoningField::Reasoning)
            .chat_token_limit(ChatTokenLimit::MaxTokens)
            .build();
        let input = ProtocolInput::builder()
            .model(model)
            .messages(messages)
            .system(system)
            .tools(tools)
            .options(&options)
            .build();
        let body = ChatCompletions.request(input, &protocol_options)?;
        let response = self
            .send(reqwest::Method::POST, "chat/completions", Some(body))
            .await?;
        Ok(ChatCompletions.assemble(response, event_tx).await?)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let response = self.send(reqwest::Method::GET, "models", None).await?;
        let bytes = crate::protocol::body::read_body(response).await?;
        let models: ModelList = serde_json::from_slice(&bytes).context(error::DecodeModelsSnafu)?;
        Ok(models
            .data
            .into_iter()
            .map(|model| {
                let mut info = self
                    .config
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.models.get(&model.id))
                    .map(|model| model.info.clone())
                    .unwrap_or_default();
                info.ids = vec![model.id];
                info
            })
            .collect())
    }

    async fn refresh_auth(&self) -> Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl EmbeddingProviderImpl for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }

    async fn embed(
        &self,
        model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse> {
        ensure!(
            model.provider == EmbeddingProviderImpl::name(self),
            error::ModelProviderMismatchSnafu {
                expected: EmbeddingProviderImpl::name(self),
                actual: &model.provider,
            }
        );
        let protocol = crate::protocol::openai::Embeddings;
        let body = protocol.request(model, request)?;
        let response = self
            .send(reqwest::Method::POST, "embeddings", Some(body))
            .await?;
        Ok(protocol.assemble(response, model, request).await?)
    }

    async fn refresh_auth(&self) -> Result<()> {
        ProviderImpl::refresh_auth(self).await
    }

    async fn reload_auth(&self) -> Result<()> {
        ProviderImpl::reload_auth(self).await
    }
}
