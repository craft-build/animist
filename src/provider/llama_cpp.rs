//! llama.cpp single-model and router-server inference policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};
use tokio::sync::RwLock;

use super::{
    DeciderModel, DeciderProviderImpl, DecisionRequest, DecisionResponse, Effort, EmbeddingModel,
    EmbeddingProviderImpl, EmbeddingRequest, EmbeddingResponse, Message, Model, ModelInfo,
    ProviderEvent, ProviderImpl, RequestOptions, Result, StreamResponse, ThinkingConfig,
    ThinkingSupport, Tool, clamp_error_body, error,
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
    auth: RwLock<Option<HeaderValue>>,
    from_env: bool,
}

impl LlamaCpp {
    /// Resolves credentials and binds the provider to its settings.
    pub fn new(mut config: LlamaCppConfig) -> Result<Self> {
        let from_env = config.api_key.is_none();
        let key = match config.api_key.take() {
            Some(key) => {
                ensure!(!key.trim().is_empty(), error::EmptyApiKeySnafu);
                Some(key)
            }
            None => match &config.api_key_env {
                Some(variable) => {
                    let key = std::env::var(variable)
                        .context(error::CredentialEnvironmentSnafu { variable })?;
                    ensure!(!key.trim().is_empty(), error::CredentialsSnafu { variable });
                    Some(key)
                }
                None => None,
            },
        };
        let auth = match key {
            Some(key) => {
                let mut header = HeaderValue::from_str(&format!("Bearer {key}"))
                    .context(error::CredentialHeaderSnafu)?;
                header.set_sensitive(true);
                Some(header)
            }
            None => None,
        };
        Ok(Self {
            config,
            client: reqwest::Client::new(),
            auth: RwLock::new(auth),
            from_env,
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
        if let Some(auth) = self.auth.read().await.clone() {
            request = request.header(AUTHORIZATION, auth);
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
        let response = request.send().await.context(error::TransportSnafu)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = clamp_error_body(response.text().await.context(error::TransportSnafu)?);
            return error::HttpSnafu { status, body }.fail();
        }
        Ok(response)
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
        let (supported, required) = match model.overrides.thinking {
            Some(ThinkingSupport::No) => (false, false),
            Some(ThinkingSupport::Yes) => (true, false),
            Some(ThinkingSupport::Required) => (true, true),
            None => (thinking.supported, thinking.required),
        };
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
        let value: Value = response.json().await.context(error::TransportSnafu)?;
        let data = value["data"]
            .as_array()
            .context(error::MissingModelDataSnafu)?;
        data.iter()
            .map(|entry| {
                let id = entry["id"].as_str().context(error::MissingModelIdSnafu)?;
                let mut info = self
                    .config
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.models.get(id))
                    .map(|model| model.info.clone())
                    .unwrap_or_default();
                info.ids = vec![id.into()];
                Ok(info)
            })
            .collect()
    }

    async fn refresh_auth(&self) -> Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> Result<()> {
        if self.from_env
            && let Some(variable) = &self.config.api_key_env
        {
            let key =
                std::env::var(variable).context(error::CredentialEnvironmentSnafu { variable })?;
            ensure!(!key.trim().is_empty(), error::CredentialsSnafu { variable });
            let mut auth = HeaderValue::from_str(&format!("Bearer {key}"))
                .context(error::CredentialHeaderSnafu)?;
            auth.set_sensitive(true);
            *self.auth.write().await = Some(auth);
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
