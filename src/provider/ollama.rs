//! Ollama routing and Chat Completions compatibility policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};

use super::*;
use crate::protocol::{
    Protocol, ProtocolInput,
    openai::{ChatCompletions, ChatReasoningField, ChatTokenLimit, OpenAiOptions},
};

/// Serializable local Ollama settings; no credentials are required.
#[derive(Debug, Clone, PartialEq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct OllamaConfig {
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

pub struct Ollama {
    config: OllamaConfig,
    client: reqwest::Client,
}

impl Ollama {
    pub fn new(config: OllamaConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
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
impl ProviderImpl for Ollama {
    fn name(&self) -> &str {
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
        Ok(())
    }
}
