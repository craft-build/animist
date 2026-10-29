//! Z.AI Coding Plan authentication, endpoint routing, and model policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, ensure};

use super::credential::ApiKey;
use super::http::ensure_success;
use super::{
    Effort, Message, Model, ModelInfo, ProviderEvent, ProviderImpl, RequestOptions, Result,
    StreamResponse, ThinkingConfig, Tool, error,
};
use crate::protocol::{
    Protocol, ProtocolInput,
    openai::{ChatCompletions, ChatThinking, ChatThinkingMode, ChatTokenLimit, OpenAiOptions},
};

/// Default Z.AI Coding Plan API root.
pub const BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
/// Default environment variable holding the Coding Plan API key.
pub const API_KEY_ENV: &str = "ZHIPU_API_KEY";

/// Serializable Coding Plan settings; credentials are never serialized.
#[derive(crate::StructNoDebug!, PartialEq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
#[expect(missing_debug_implementations, reason = "credential-bearing type; Debug omitted by policy")]
pub struct ZaiCodingPlanConfig {
    /// Explicit key; `None` reads `api_key_env` at construction.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    /// Environment variable consulted when `api_key` is unset.
    #[builder(default = API_KEY_ENV.to_owned())]
    pub api_key_env: String,
    /// API root prepended to request paths.
    #[builder(default = BASE_URL.to_owned())]
    pub base_url: String,
    /// Output cap when neither request nor catalog sets one.
    #[builder(default = 8192_u32)]
    pub max_output_tokens: u32,
    /// Explicit model metadata; no undocumented discovery endpoint is queried.
    pub catalog: Option<crate::catalog::ProviderCatalog>,
}

impl Default for ZaiCodingPlanConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Z.AI Coding Plan provider over its Chat Completions surface.
#[expect(
    missing_debug_implementations,
    reason = "credential-bearing type; Debug omitted by policy"
)]
pub struct ZaiCodingPlan {
    config: ZaiCodingPlanConfig,
    client: reqwest::Client,
    key: ApiKey,
}

impl ZaiCodingPlan {
    /// Validates credentials and binds the provider to its settings.
    pub fn new(mut config: ZaiCodingPlanConfig) -> Result<Self> {
        let key = ApiKey::new(config.api_key.take(), &config.api_key_env, "Bearer ")?;
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
}

#[async_trait]
impl ProviderImpl for ZaiCodingPlan {
    fn name(&self) -> &'static str {
        "zai-coding-plan"
    }

    async fn stream_message(
        &self,
        model: &Model,
        messages: &[Message],
        system: &str,
        tools: &[Tool],
        event_tx: &Sender<ProviderEvent>,
        mut options: RequestOptions,
    ) -> Result<StreamResponse> {
        ensure!(
            model.provider.name() == self.name(),
            error::ModelProviderMismatchSnafu {
                expected: self.name(),
                actual: model.provider.name(),
            }
        );
        ensure!(
            !options.fast && model.overrides.fast != Some(true),
            error::FastModeUnsupportedSnafu {
                provider: self.name()
            }
        );
        for (control, value, min, max) in [
            ("temperature", options.temperature, 0.0, 1.0),
            ("top_p", options.top_p, 0.01, 1.0),
        ] {
            if let Some(value) = value {
                ensure!(
                    value.is_finite() && (min..=max).contains(&value),
                    error::SamplingOutOfRangeSnafu {
                        provider: self.name(),
                        control,
                        value,
                        min,
                        max,
                    }
                );
            }
        }
        for (control, explicit) in [
            ("frequency_penalty", options.frequency_penalty.is_some()),
            ("presence_penalty", options.presence_penalty.is_some()),
        ] {
            ensure!(
                !explicit,
                error::UnsupportedSamplingControlSnafu {
                    provider: self.name(),
                    control,
                }
            );
        }
        if let Some(stops) = &options.stop_sequences {
            ensure!(
                stops.len() <= 1,
                error::TooManyStopSequencesSnafu {
                    provider: self.name(),
                    count: stops.len(),
                    max: 1_usize,
                }
            );
        }
        let thinking = &model.info.features.thinking;
        let supported = model.thinking_supported();
        let required = model.thinking_required();
        let (mode, effort) = match &options.thinking {
            ThinkingConfig::Off if required => return error::ReasoningRequiredSnafu.fail(),
            ThinkingConfig::Off => (ChatThinkingMode::Disabled, None),
            ThinkingConfig::Adaptive | ThinkingConfig::Effort(Effort::Adaptive) if supported => {
                (ChatThinkingMode::Enabled, None)
            }
            ThinkingConfig::Effort(effort) if supported && thinking.efforts.contains(effort) => {
                (ChatThinkingMode::Enabled, effort.openai_wire())
            }
            ThinkingConfig::Budget(_) => return error::ReasoningBudgetUnsupportedSnafu.fail(),
            ThinkingConfig::Effort(effort) => {
                return error::UnsupportedReasoningEffortSnafu {
                    effort: effort.clone(),
                }
                .fail();
            }
            ThinkingConfig::Adaptive => {
                return error::ThinkingUnsupportedSnafu { model: &model.id }.fail();
            }
        };
        let max_tokens = options
            .max_output_tokens
            .or(model.info.config.max_output_tokens)
            .unwrap_or(self.config.max_output_tokens);
        ensure!(max_tokens > 0, error::ZeroOutputTokenLimitSnafu);
        options.max_output_tokens = Some(max_tokens);
        let protocol_options = OpenAiOptions::builder()
            .maybe_reasoning_effort(effort)
            .replay_reasoning_content(true)
            .chat_thinking(
                ChatThinking::builder()
                    .mode(mode)
                    .clear_thinking(false)
                    .build(),
            )
            .chat_token_limit(ChatTokenLimit::MaxTokens)
            .maybe_tool_stream((!tools.is_empty()).then_some(true))
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
            .client
            .post(format!(
                "{}/chat/completions",
                self.config.base_url.trim_end_matches('/')
            ))
            .header(AUTHORIZATION, self.key.header().await)
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .context(error::TransportSnafu)?;
        let response = ensure_success(response).await?;
        Ok(ChatCompletions.assemble(response, event_tx).await?)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(self
            .config
            .catalog
            .iter()
            .flat_map(|catalog| &catalog.models)
            .map(|(id, model)| {
                let mut info = model.info.clone();
                info.ids = vec![id.clone()];
                info
            })
            .collect())
    }

    async fn refresh_auth(&self) -> Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> Result<()> {
        self.key.reload().await
    }
}
