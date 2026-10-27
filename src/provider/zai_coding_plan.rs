//! Z.AI Coding Plan authentication, endpoint routing, and model policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, ensure};
use tokio::sync::RwLock;

use super::*;
use crate::protocol::{
    Protocol, ProtocolInput,
    openai::{ChatCompletions, ChatThinking, ChatThinkingMode, ChatTokenLimit, OpenAiOptions},
};

pub const BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
pub const API_KEY_ENV: &str = "ZHIPU_API_KEY";

/// Serializable Coding Plan settings; credentials are never serialized.
#[derive(crate::StructNoDebug!, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct ZaiCodingPlanConfig {
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    #[builder(default = API_KEY_ENV.to_owned())]
    pub api_key_env: String,
    #[builder(default = BASE_URL.to_owned())]
    pub base_url: String,
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

pub struct ZaiCodingPlan {
    config: ZaiCodingPlanConfig,
    client: reqwest::Client,
    auth: RwLock<HeaderValue>,
    from_env: bool,
}

impl ZaiCodingPlan {
    pub fn new(mut config: ZaiCodingPlanConfig) -> Result<Self> {
        let from_env = config.api_key.is_none();
        let key = match config.api_key.take() {
            Some(key) => {
                ensure!(!key.trim().is_empty(), error::EmptyApiKeySnafu);
                key
            }
            None => {
                let key = std::env::var(&config.api_key_env).context(
                    error::CredentialEnvironmentSnafu {
                        variable: &config.api_key_env,
                    },
                )?;
                ensure!(
                    !key.trim().is_empty(),
                    error::CredentialsSnafu {
                        variable: &config.api_key_env
                    }
                );
                key
            }
        };
        let mut auth = HeaderValue::from_str(&format!("Bearer {key}"))
            .context(error::CredentialHeaderSnafu)?;
        auth.set_sensitive(true);
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
}

#[async_trait]
impl ProviderImpl for ZaiCodingPlan {
    fn name(&self) -> &str {
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
        let thinking = &model.info.features.thinking;
        let (supported, required) = match model.overrides.thinking {
            Some(ThinkingSupport::No) => (false, false),
            Some(ThinkingSupport::Yes) => (true, false),
            Some(ThinkingSupport::Required) => (true, true),
            None => (thinking.supported, thinking.required),
        };
        let (mode, effort) = match &options.thinking {
            ThinkingConfig::Off if required => return error::ReasoningRequiredSnafu.fail(),
            ThinkingConfig::Off => (ChatThinkingMode::Disabled, None),
            ThinkingConfig::Adaptive | ThinkingConfig::Effort(Effort::Adaptive) if supported => {
                (ChatThinkingMode::Enabled, None)
            }
            ThinkingConfig::Effort(effort) if supported && thinking.efforts.contains(effort) => {
                let effort = match effort {
                    Effort::Minimal => "minimal",
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High => "high",
                    Effort::XHigh => "xhigh",
                    Effort::Max => "max",
                    Effort::Adaptive => unreachable!(),
                };
                (ChatThinkingMode::Enabled, Some(effort))
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
            .header(AUTHORIZATION, self.auth.read().await.clone())
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .context(error::TransportSnafu)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.context(error::TransportSnafu)?;
            return error::HttpSnafu { status, body }.fail();
        }
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
        if self.from_env {
            let key = std::env::var(&self.config.api_key_env).context(
                error::CredentialEnvironmentSnafu {
                    variable: &self.config.api_key_env,
                },
            )?;
            ensure!(
                !key.trim().is_empty(),
                error::CredentialsSnafu {
                    variable: &self.config.api_key_env
                }
            );
            let mut auth = HeaderValue::from_str(&format!("Bearer {key}"))
                .context(error::CredentialHeaderSnafu)?;
            auth.set_sensitive(true);
            *self.auth.write().await = auth;
        }
        Ok(())
    }
}
