//! Bedrock native API routing with AWS credentials and SigV4 authentication.

mod error;

use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sigv4::{
    http_request::{SignableBody, SignableRequest, SigningSettings, sign},
    sign::v4,
};
use bon::Builder;
use bytes::Bytes;
use flume::Sender;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};
use tokio::sync::RwLock;

use super::{
    Effort, Message, Model, ModelInfo, ProviderEvent, ProviderImpl, RequestOptions, Result,
    StreamResponse, ThinkingConfig, ThinkingSupport, Tool, error as provider_error,
};
use crate::protocol::{
    Protocol as ProtocolAdapter, ProtocolInput,
    anthropic::{AnthropicEffort, AnthropicOptions, AnthropicThinking, Messages},
    openai::{ChatCompletions, OpenAiOptions, Responses},
};

pub use aws_credential_types::{
    Credentials,
    provider::{
        ProvideCredentials, SharedCredentialsProvider, error::CredentialsError,
        future::ProvideCredentials as CredentialsFuture,
    },
};
pub use aws_sigv4::{
    http_request::SigningError, sign::v4::signing_params::BuildError as SigningParametersError,
};
pub use aws_types::{SdkConfig, region::Region};
pub use error::BedrockError;

#[derive(crate::Enum!, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    AnthropicMessages,
    ChatCompletions,
    Responses,
}

/// Serializable routing and policy; AWS credentials stay in runtime providers.
#[derive(crate::StructNoDebug!, Debug, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct BedrockConfig {
    pub region: Option<String>,
    pub profile: Option<String>,
    /// Optional runtime origin, including a port for local mocks.
    pub endpoint_url: Option<String>,
    #[builder(default)]
    pub protocol: Protocol,
    #[builder(default = 8192_u32)]
    pub max_output_tokens: u32,
    #[builder(default = 3_u32)]
    pub max_attempts: u32,
    #[builder(default = "2023-06-01".to_owned())]
    pub anthropic_version: String,
    #[builder(default)]
    pub anthropic_beta: Vec<String>,
    /// Explicit metadata only; runtime has no OpenAI-compatible model listing.
    pub catalog: Option<crate::catalog::ProviderCatalog>,
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

pub struct Bedrock {
    config: BedrockConfig,
    client: reqwest::Client,
    region: Region,
    endpoint: reqwest::Url,
    credentials: RwLock<SharedCredentialsProvider>,
    managed_credentials: bool,
}

impl Bedrock {
    pub async fn new(config: BedrockConfig) -> Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let Some(profile) = &config.profile {
            loader = loader.profile_name(profile);
        }
        if let Some(region) = &config.region {
            loader = loader.region(Region::new(region.clone()));
        }
        let sdk = loader.load().await;
        let mut provider = Self::from_sdk_config(config, &sdk)?;
        provider.managed_credentials = true;
        Ok(provider)
    }

    /// Uses caller-owned AWS configuration without running credential discovery.
    pub fn from_sdk_config(config: BedrockConfig, sdk: &SdkConfig) -> Result<Self> {
        ensure!(
            (1..=10).contains(&config.max_attempts),
            error::InvalidMaxAttemptsSnafu {
                attempts: config.max_attempts
            }
        );
        let region = config
            .region
            .as_ref()
            .map(|region| Region::new(region.clone()))
            .or_else(|| sdk.region().cloned())
            .context(error::MissingRegionSnafu)?;
        ensure!(
            !region.as_ref().is_empty()
                && region
                    .as_ref()
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            error::InvalidRegionSnafu {
                region: region.as_ref()
            }
        );
        let credentials = sdk
            .credentials_provider()
            .context(error::MissingCredentialsProviderSnafu)?;
        let endpoint = config
            .endpoint_url
            .clone()
            .or_else(|| sdk.endpoint_url().map(str::to_owned))
            .unwrap_or_else(|| format!("https://bedrock-runtime.{}.amazonaws.com", region.as_ref()))
            .parse::<reqwest::Url>()
            .context(error::EndpointSnafu)?;
        let loopback = match endpoint.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            _ => false,
        };
        ensure!(
            (endpoint.scheme() == "https" || (endpoint.scheme() == "http" && loopback))
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.path() == "/",
            error::InvalidEndpointSnafu
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context(error::ClientSnafu)?;
        Ok(Self {
            config,
            client,
            region,
            endpoint,
            credentials: RwLock::new(credentials),
            managed_credentials: false,
        })
    }

    async fn send(&self, path: &str, body: Value) -> Result<reqwest::Response> {
        let body = Bytes::from(serde_json::to_vec(&body).context(error::EncodeRequestSnafu)?);
        let mut url = self.endpoint.clone();
        url.set_path(path);
        for attempt in 0..self.config.max_attempts {
            let mut builder = http::Request::builder()
                .method("POST")
                .uri(url.as_str())
                .header("content-type", "application/json")
                .header("accept", "text/event-stream");
            if self.config.protocol == Protocol::AnthropicMessages {
                builder = builder.header("anthropic-version", &self.config.anthropic_version);
                if !self.config.anthropic_beta.is_empty() {
                    builder =
                        builder.header("anthropic-beta", self.config.anthropic_beta.join(","));
                }
            }
            let mut request = builder.body(body.clone()).context(error::RequestSnafu)?;
            let provider = self.credentials.read().await.clone();
            let identity = provider
                .provide_credentials()
                .await
                .context(error::CredentialsSnafu)?
                .into();
            let signing_params = v4::SigningParams::builder()
                .identity(&identity)
                .region(self.region.as_ref())
                .name("bedrock")
                .time(SystemTime::now())
                .settings(SigningSettings::default())
                .build()
                .context(error::SigningParametersSnafu)?
                .into();
            let headers = request
                .headers()
                .iter()
                .map(|(name, value)| value.to_str().map(|value| (name.as_str(), value)))
                .collect::<std::result::Result<Vec<_>, _>>()
                .context(error::SigningHeaderSnafu)?;
            let signable = SignableRequest::new(
                "POST",
                url.as_str(),
                headers.into_iter(),
                SignableBody::Bytes(&body),
            )
            .context(error::SigningSnafu)?;
            let (instructions, _) = sign(signable, &signing_params)
                .context(error::SigningSnafu)?
                .into_parts();
            for (name, value) in instructions.headers() {
                http::HeaderValue::from_str(value)
                    .context(error::SignedHeaderValueSnafu { header: name })?;
            }
            instructions.apply_to_request_http1x(&mut request);
            let request = reqwest::Request::try_from(request).context(error::TransportSnafu)?;
            let mut response = self
                .client
                .execute(request)
                .await
                .context(error::TransportSnafu)?;
            if response.status().is_success() {
                return Ok(response);
            }
            let status = response.status().as_u16();
            let request_id = response
                .headers()
                .get("x-amzn-requestid")
                .or_else(|| response.headers().get("x-amzn-request-id"))
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let error_type = response
                .headers()
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.context(error::TransportSnafu)? {
                let remaining = (64 * 1024_usize).saturating_sub(body.len());
                body.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
                if chunk.len() > remaining {
                    return Err(BedrockError::ErrorBodyTooLarge {
                        status,
                        request_id,
                        error_type,
                        body_prefix: body,
                        limit: 64 * 1024,
                    }
                    .into());
                }
            }
            if matches!(status, 429 | 503) && attempt + 1 < self.config.max_attempts {
                let delay = retry_after
                    .map(|seconds| Duration::from_secs(seconds.min(30)))
                    .unwrap_or_else(|| {
                        Duration::from_millis((200 * (1_u64 << attempt)).min(30_000))
                    });
                tokio::time::sleep(delay).await;
                continue;
            }
            return Err(BedrockError::Http {
                status,
                request_id,
                error_type,
                body,
            }
            .into());
        }
        unreachable!("validated max_attempts is positive")
    }
}

#[async_trait]
impl ProviderImpl for Bedrock {
    fn name(&self) -> &str {
        "amazon-bedrock"
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
            model.provider.as_ref() == self.name(),
            provider_error::ModelProviderMismatchSnafu {
                expected: self.name(),
                actual: model.provider.as_ref(),
            }
        );
        ensure!(
            !options.fast && model.overrides.fast != Some(true),
            provider_error::FastModeUnsupportedSnafu {
                provider: self.name()
            }
        );
        let max_tokens = options
            .max_output_tokens
            .or(model.info.config.max_output_tokens)
            .unwrap_or(self.config.max_output_tokens);
        ensure!(max_tokens > 0, provider_error::ZeroOutputTokenLimitSnafu);
        options.max_output_tokens = Some(max_tokens);
        let (supported, required) = match model.overrides.thinking {
            Some(ThinkingSupport::No) => (false, false),
            Some(ThinkingSupport::Yes) => (true, false),
            Some(ThinkingSupport::Required) => (true, true),
            None => (
                model.info.features.thinking.supported,
                model.info.features.thinking.required,
            ),
        };
        ensure!(
            !required || options.thinking != ThinkingConfig::Off,
            provider_error::ReasoningRequiredSnafu
        );
        let input = ProtocolInput::builder()
            .model(model)
            .messages(messages)
            .system(system)
            .tools(tools)
            .options(&options)
            .build();
        match self.config.protocol {
            Protocol::AnthropicMessages => {
                let mut effort = None;
                let thinking = match &options.thinking {
                    ThinkingConfig::Off => AnthropicThinking::Disabled,
                    ThinkingConfig::Adaptive => {
                        ensure!(
                            supported,
                            provider_error::ThinkingUnsupportedSnafu { model: &model.id }
                        );
                        AnthropicThinking::Adaptive
                    }
                    ThinkingConfig::Budget(budget) => {
                        ensure!(
                            supported,
                            provider_error::ThinkingUnsupportedSnafu { model: &model.id }
                        );
                        ensure!(
                            *budget >= 1024 && *budget < max_tokens,
                            provider_error::InvalidThinkingBudgetSnafu {
                                budget: *budget,
                                max_tokens
                            }
                        );
                        AnthropicThinking::Enabled {
                            budget_tokens: *budget,
                        }
                    }
                    ThinkingConfig::Effort(requested) => {
                        ensure!(
                            supported && model.info.features.thinking.efforts.contains(requested),
                            provider_error::UnsupportedReasoningEffortSnafu {
                                effort: requested.clone()
                            }
                        );
                        effort = Some(match requested {
                            Effort::Low => AnthropicEffort::Low,
                            Effort::Medium => AnthropicEffort::Medium,
                            Effort::High => AnthropicEffort::High,
                            Effort::XHigh => AnthropicEffort::XHigh,
                            Effort::Max => AnthropicEffort::Max,
                            Effort::Minimal | Effort::Adaptive => {
                                return provider_error::UnsupportedReasoningEffortSnafu {
                                    effort: requested.clone(),
                                }
                                .fail();
                            }
                        });
                        AnthropicThinking::Adaptive
                    }
                };
                let protocol_options = AnthropicOptions::builder()
                    .max_tokens(max_tokens)
                    .thinking(thinking)
                    .maybe_effort(effort)
                    .fast(false)
                    .build();
                let body = Messages.request(input, &protocol_options)?;
                let response = self.send("/anthropic/v1/messages", body).await?;
                Ok(Messages.assemble(response, event_tx).await?)
            }
            Protocol::ChatCompletions | Protocol::Responses => {
                let effort = match &options.thinking {
                    ThinkingConfig::Off => supported.then_some("none"),
                    ThinkingConfig::Adaptive => {
                        ensure!(
                            supported,
                            provider_error::ThinkingUnsupportedSnafu { model: &model.id }
                        );
                        None
                    }
                    ThinkingConfig::Budget(_) => {
                        return provider_error::ReasoningBudgetUnsupportedSnafu.fail();
                    }
                    ThinkingConfig::Effort(requested) => {
                        ensure!(
                            supported && model.info.features.thinking.efforts.contains(requested),
                            provider_error::UnsupportedReasoningEffortSnafu {
                                effort: requested.clone()
                            }
                        );
                        Some(match requested {
                            Effort::Minimal => "minimal",
                            Effort::Low => "low",
                            Effort::Medium => "medium",
                            Effort::High => "high",
                            Effort::XHigh => "xhigh",
                            Effort::Max => "max",
                            Effort::Adaptive => {
                                return provider_error::UnsupportedReasoningEffortSnafu {
                                    effort: requested.clone(),
                                }
                                .fail();
                            }
                        })
                    }
                };
                let protocol_options = OpenAiOptions::builder()
                    .maybe_reasoning_effort(effort)
                    .build();
                match self.config.protocol {
                    Protocol::ChatCompletions => {
                        let body = ChatCompletions.request(input, &protocol_options)?;
                        let response = self.send("/openai/v1/chat/completions", body).await?;
                        Ok(ChatCompletions.assemble(response, event_tx).await?)
                    }
                    Protocol::Responses => {
                        let body = Responses.request(input, &protocol_options)?;
                        let response = self.send("/openai/v1/responses", body).await?;
                        Ok(Responses.assemble(response, event_tx).await?)
                    }
                    Protocol::AnthropicMessages => unreachable!(),
                }
            }
        }
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
        let provider = self.credentials.read().await.clone();
        provider
            .provide_credentials()
            .await
            .context(error::CredentialsSnafu)?;
        Ok(())
    }

    async fn reload_auth(&self) -> Result<()> {
        if self.managed_credentials {
            let mut loader =
                aws_config::defaults(BehaviorVersion::latest()).region(self.region.clone());
            if let Some(profile) = &self.config.profile {
                loader = loader.profile_name(profile);
            }
            let sdk = loader.load().await;
            let provider = sdk
                .credentials_provider()
                .context(error::MissingCredentialsProviderSnafu)?;
            provider
                .provide_credentials()
                .await
                .context(error::CredentialsSnafu)?;
            *self.credentials.write().await = provider;
        } else {
            let provider = self.credentials.read().await.clone();
            provider
                .provide_credentials()
                .await
                .context(error::CredentialsSnafu)?;
        }
        Ok(())
    }
}
