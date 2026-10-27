//! Synthetic API-key authentication, model discovery, and Chat Completions policy.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{ResultExt, ensure};
use tokio::sync::RwLock;

use super::*;
use crate::{
    catalog::CatalogModel,
    protocol::{
        EmbeddingProtocol, Protocol, ProtocolInput,
        openai::{ChatCompletions, OpenAiOptions},
    },
};

pub const BASE_URL: &str = "https://api.synthetic.new/openai/v1";
pub const API_KEY_ENV: &str = "SYNTHETIC_API_KEY";

/// Serializable Synthetic settings; API keys are never serialized.
#[derive(crate::StructNoDebug!, PartialEq, Eq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct SyntheticConfig {
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    #[builder(default = API_KEY_ENV.to_owned())]
    pub api_key_env: String,
    /// OpenAI-compatible API base, including `/openai/v1`.
    #[builder(default = BASE_URL.to_owned())]
    pub base_url: String,
    #[builder(default = 8192_u32)]
    pub max_output_tokens: u32,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

pub struct Synthetic {
    config: SyntheticConfig,
    client: reqwest::Client,
    auth: RwLock<HeaderValue>,
    from_env: bool,
}

impl Synthetic {
    pub fn new(mut config: SyntheticConfig) -> Result<Self> {
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

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .request(
                method,
                format!("{}/{}", self.config.base_url.trim_end_matches('/'), path),
            )
            .header(AUTHORIZATION, self.auth.read().await.clone());
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
            let body = response.text().await.context(error::TransportSnafu)?;
            return error::HttpSnafu { status, body }.fail();
        }
        Ok(response)
    }

    /// Discovers text models and preserves their complete Synthetic metadata.
    /// Prices are USD per million tokens; absent rates are not known-free.
    pub async fn model_catalog(&self) -> Result<Vec<CatalogModel>> {
        let response = self.send(reqwest::Method::GET, "models", None).await?;
        let bytes = response.bytes().await.context(error::TransportSnafu)?;
        let models: Models =
            serde_json::from_slice(&bytes).context(error::DecodeSyntheticModelsSnafu)?;
        let mut catalog = Vec::new();
        for metadata in models.data {
            let source: SourceModel = serde_json::from_value(metadata.clone())
                .context(error::DecodeSyntheticModelsSnafu)?;
            if !source.output_modalities.iter().any(|m| m == "text") {
                continue;
            }
            catalog.push(
                CatalogModel::builder()
                    .name(
                        source
                            .display_name
                            .clone()
                            .unwrap_or_else(|| source.id.clone()),
                    )
                    .info(source.into_info()?)
                    .metadata(metadata)
                    .build(),
            );
        }
        Ok(catalog)
    }
}

#[async_trait]
impl ProviderImpl for Synthetic {
    fn name(&self) -> &str {
        "synthetic"
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
        let effort = match &options.thinking {
            ThinkingConfig::Off if required => return error::ReasoningRequiredSnafu.fail(),
            ThinkingConfig::Off => supported.then_some("none"),
            ThinkingConfig::Adaptive | ThinkingConfig::Effort(Effort::Adaptive) if supported => {
                None
            }
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
        Ok(self
            .model_catalog()
            .await?
            .into_iter()
            .map(|model| model.info)
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

#[async_trait]
impl EmbeddingProviderImpl for Synthetic {
    fn name(&self) -> &str {
        "synthetic"
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

#[derive(crate::Struct!, Deserialize)]
struct Models {
    data: Vec<Value>,
}

#[derive(crate::Struct!, Deserialize)]
struct SourceModel {
    id: String,
    display_name: Option<String>,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
    #[serde(default)]
    supported_features: Vec<String>,
    #[serde(default)]
    reasoning_parameters: ReasoningParameters,
    context_length: Option<u32>,
    max_output_length: Option<u32>,
    #[serde(default)]
    pricing: SourcePricing,
}

#[derive(crate::Struct!, Deserialize)]
#[serde(default)]
struct ReasoningParameters {
    efforts: Vec<String>,
}

#[derive(crate::Struct!, Deserialize)]
#[serde(default)]
struct SourcePricing {
    prompt: Price,
    completion: Price,
    input_cache_reads: Price,
    input_cache_writes: Price,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(untagged)]
enum Price {
    Number(f64),
    Text(String),
    #[default]
    Missing,
}

impl Price {
    fn per_million(&self, field: &'static str) -> Result<f64> {
        let (rate, value) = match self {
            Self::Missing => return Ok(0.0),
            Self::Number(value) => (Some(*value), Value::from(*value)),
            Self::Text(value) => (
                value.trim_start_matches('$').parse().ok(),
                Value::from(value.clone()),
            ),
        };
        match rate {
            Some(rate) if rate.is_finite() && rate >= 0.0 && (rate * 1_000_000.0).is_finite() => {
                Ok(rate * 1_000_000.0)
            }
            _ => error::InvalidSyntheticPriceSnafu { field, value }.fail(),
        }
    }
}

impl SourceModel {
    fn into_info(self) -> Result<ModelInfo> {
        let efforts = self
            .reasoning_parameters
            .efforts
            .iter()
            .filter_map(|value| match value.as_str() {
                "low" => Some(Effort::Low),
                "medium" => Some(Effort::Medium),
                "high" => Some(Effort::High),
                "xhigh" => Some(Effort::XHigh),
                "max" => Some(Effort::Max),
                "minimal" => Some(Effort::Minimal),
                _ => None,
            })
            .collect::<Vec<_>>();
        Ok(ModelInfo::builder()
            .ids(vec![self.id])
            .tier(Tier::default())
            .features(
                Features::builder()
                    .tool_examples(false)
                    .thinking(
                        Thinking::builder()
                            .supported(self.supported_features.iter().any(|f| f == "reasoning"))
                            .required(false)
                            .efforts(efforts)
                            .build(),
                    )
                    .vision(self.input_modalities.iter().any(|m| m == "image"))
                    .deferred_tools(false)
                    .build(),
            )
            .pricing(
                Pricing::builder()
                    .input(self.pricing.prompt.per_million("prompt")?)
                    .output(self.pricing.completion.per_million("completion")?)
                    .cache_read(
                        self.pricing
                            .input_cache_reads
                            .per_million("input_cache_reads")?,
                    )
                    .cache_write(
                        self.pricing
                            .input_cache_writes
                            .per_million("input_cache_writes")?,
                    )
                    .build(),
            )
            .config(
                Config::builder()
                    .maybe_context_window(self.context_length)
                    .maybe_max_output_tokens(self.max_output_length)
                    .build(),
            )
            .build())
    }
}
