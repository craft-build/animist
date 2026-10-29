//! llama.cpp single-model and router-server inference policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{ResultExt, ensure};

use super::credential::ApiKey;
use super::http::ensure_success;
use super::openai::ModelList;
use super::{
    DeciderModel, DeciderProviderImpl, DecisionRequest, DecisionResponse, Effort, EmbeddingModel,
    EmbeddingProviderImpl, EmbeddingRequest, EmbeddingResponse, Message, Model, ModelInfo,
    ProviderEvent, ProviderImpl, RequestOptions, Result, StreamResponse, ThinkingConfig, Tool,
    error,
};
use crate::protocol::{
    DeciderProtocol, EmbeddingProtocol, Protocol, ProtocolInput,
    openai::{ChatCompletions, ChatTokenLimit, OpenAiOptions},
};

/// Serializable llama.cpp settings; credentials are never serialized.
#[derive(crate::StructNoDebug!, PartialEq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
#[expect(missing_debug_implementations, reason = "credential-bearing type; Debug omitted by policy")]
pub struct LlamaCppConfig {
    /// API root prepended to request paths.
    #[builder(default = "http://localhost:9931/v1".to_owned())]
    pub base_url: String,
    /// Explicit key; `None` reads `api_key_env` or runs unauthenticated.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    /// Only read when explicitly configured; missing or empty values are errors.
    pub api_key_env: Option<String>,
    /// Optional metadata for models returned by the server.
    pub catalog: Option<crate::catalog::ProviderCatalog>,
}

impl Default for LlamaCppConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// llama.cpp single-model and router-server provider.
#[expect(
    missing_debug_implementations,
    reason = "credential-bearing type; Debug omitted by policy"
)]
pub struct LlamaCpp {
    config: LlamaCppConfig,
    client: reqwest::Client,
    key: Option<ApiKey>,
}

impl LlamaCpp {
    /// Resolves credentials and binds the provider to its settings.
    pub fn new(mut config: LlamaCppConfig) -> Result<Self> {
        let key = ApiKey::optional(
            config.api_key.take(),
            config.api_key_env.as_deref(),
            "Bearer ",
        )?;
        Ok(Self {
            config,
            client: reqwest::Client::new(),
            key,
        })
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
        if let Some(key) = &self.key {
            request = request.header(AUTHORIZATION, key.header().await);
        }
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
impl ProviderImpl for LlamaCpp {
    fn name(&self) -> &'static str {
        "llama-cpp"
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
        let mut budget = None;
        let effort = match &options.thinking {
            ThinkingConfig::Off if required => return error::ReasoningRequiredSnafu.fail(),
            ThinkingConfig::Off => Some("none"),
            ThinkingConfig::Adaptive => None,
            ThinkingConfig::Effort(Effort::Adaptive) if supported => None,
            ThinkingConfig::Effort(effort) if supported && thinking.efforts.contains(effort) => {
                effort.openai_wire()
            }
            ThinkingConfig::Effort(effort) => {
                return error::UnsupportedReasoningEffortSnafu {
                    effort: effort.clone(),
                }
                .fail();
            }
            ThinkingConfig::Budget(tokens) => {
                ensure!(
                    supported,
                    error::ThinkingUnsupportedSnafu { model: &model.id }
                );
                ensure!(
                    *tokens <= i32::MAX.unsigned_abs(),
                    error::LlamaCppThinkingBudgetOutOfRangeSnafu { budget: *tokens }
                );
                budget = Some(*tokens);
                None
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
            .maybe_chat_reasoning_budget(budget)
            .replay_reasoning_content(true)
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
        if let Some(key) = &self.key {
            key.reload().await?;
        }
        Ok(())
    }
}

#[async_trait]
impl EmbeddingProviderImpl for LlamaCpp {
    fn name(&self) -> &'static str {
        "llama-cpp"
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
        ensure!(
            request.dimensions.is_none(),
            error::EmbeddingDimensionsUnsupportedSnafu {
                provider: EmbeddingProviderImpl::name(self),
            }
        );
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

#[async_trait]
impl DeciderProviderImpl for LlamaCpp {
    fn name(&self) -> &'static str {
        "llama-cpp"
    }

    async fn decide(
        &self,
        model: &DeciderModel,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse> {
        ensure!(
            model.provider == DeciderProviderImpl::name(self),
            error::ModelProviderMismatchSnafu {
                expected: DeciderProviderImpl::name(self),
                actual: &model.provider,
            }
        );
        let protocol = crate::protocol::system_one::SystemOne;
        let body = protocol.request(model, request)?;
        let response = self
            .send(reqwest::Method::POST, "systemone", Some(body))
            .await?;
        Ok(protocol.assemble(response, request).await?)
    }

    async fn refresh_auth(&self) -> Result<()> {
        ProviderImpl::refresh_auth(self).await
    }

    async fn reload_auth(&self) -> Result<()> {
        ProviderImpl::reload_auth(self).await
    }
}
