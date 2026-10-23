//! llama.cpp single-model and router-server inference policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};
use tokio::sync::RwLock;

use super::*;
use crate::protocol::{
    Protocol, ProtocolInput,
    openai::{ChatCompletions, ChatTokenLimit, OpenAiOptions},
};

/// Serializable llama.cpp settings; credentials are never serialized.
#[derive(crate::StructNoDebug!, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct LlamaCppConfig {
    #[builder(default = "http://localhost:9931/v1".to_owned())]
    pub base_url: String,
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

pub struct LlamaCpp {
    config: LlamaCppConfig,
    client: reqwest::Client,
    auth: RwLock<Option<HeaderValue>>,
    from_env: bool,
}

impl LlamaCpp {
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
            request = request.header("Accept", "text/event-stream").json(&body);
        }
        let response = request.send().await.context(error::TransportSnafu)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.context(error::TransportSnafu)?;
            return error::HttpSnafu { status, body }.fail();
        }
        Ok(response)
    }
}

#[async_trait]
impl ProviderImpl for LlamaCpp {
    fn name(&self) -> &str {
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
            model.provider.as_ref() == self.name(),
            error::ModelProviderMismatchSnafu {
                expected: self.name(),
                actual: model.provider.as_ref(),
            }
        );
        ensure!(
            !options.fast && model.overrides.fast != Some(true),
            error::FastModeUnsupportedSnafu {
                provider: self.name()
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
                Some(match effort {
                    Effort::Minimal => "minimal",
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High => "high",
                    Effort::XHigh => "xhigh",
                    Effort::Max => "max",
                    Effort::Adaptive => unreachable!(),
                })
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
                    *tokens <= i32::MAX as u32,
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
