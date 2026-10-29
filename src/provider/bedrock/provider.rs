//! Bedrock provider: configuration, signing, retries, and protocol routing.

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

use super::error;
use crate::protocol::{
    Protocol as ProtocolAdapter, ProtocolInput,
    anthropic::{AnthropicOptions, Messages},
    bedrock::converse::{Converse, ConverseOptions},
    openai::{ChatCompletions, OpenAiOptions, Responses},
};
use crate::provider::http::ERROR_BODY_LIMIT;
use crate::provider::thinking::{anthropic_thinking, openai_reasoning_effort};
use crate::provider::{
    Message, Model, ModelInfo, ProviderEvent, ProviderImpl, RequestOptions, Result, StreamResponse,
    ThinkingConfig, Tool, error as provider_error,
};

use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_types::{SdkConfig, region::Region};

/// Wire protocol used for Bedrock chat requests.
#[derive(crate::Enum!, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// Bedrock's native Anthropic Messages surface.
    #[default]
    AnthropicMessages,
    /// Bedrock's `OpenAI` Chat Completions compatibility surface.
    ChatCompletions,
    /// Bedrock's `OpenAI` Responses compatibility surface.
    Responses,
    /// Bedrock's portable Converse surface with AWS `EventStream` framing.
    Converse,
}

/// Source used to discover Bedrock model IDs.
#[derive(crate::Enum!, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingMode {
    /// Return configured catalog entries without AWS requests.
    #[default]
    Catalog,
    /// Query foundation models and all inference profile pages.
    ControlPlane,
}

/// Serializable routing and policy; AWS credentials stay in runtime providers.
#[derive(crate::StructWithCustomDefault!, PartialEq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct BedrockConfig {
    /// AWS region; defaults to the SDK-resolved region.
    pub region: Option<String>,
    /// AWS shared-config profile used for credential discovery.
    pub profile: Option<String>,
    /// Optional runtime origin, including a port for local mocks.
    pub endpoint_url: Option<String>,
    /// Optional control-plane origin; independent of the runtime endpoint.
    pub control_plane_endpoint_url: Option<String>,
    /// Discovery source; defaults to the explicit catalog.
    #[builder(default)]
    pub listing_mode: ListingMode,
    /// Wire protocol for chat requests.
    #[builder(default)]
    pub protocol: Protocol,
    /// Output cap when neither request nor catalog sets one.
    #[builder(default = 8192_u32)]
    pub max_output_tokens: u32,
    /// Sign-and-send attempts for throttled or unavailable responses.
    #[builder(default = 3_u32)]
    pub max_attempts: u32,
    /// `anthropic-version` header for the native Messages surface.
    #[builder(default = "2023-06-01".to_owned())]
    pub anthropic_version: String,
    /// `anthropic-beta` features appended to Messages requests.
    #[builder(default)]
    pub anthropic_beta: Vec<String>,
    /// Explicit model-specific Converse settings; no dialect is inferred.
    pub converse_additional_model_request_fields: Option<Value>,
    /// Metadata merged by exact ID over control-plane listings.
    pub catalog: Option<crate::catalog::ProviderCatalog>,
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Bedrock provider with AWS `SigV4` request signing.
pub struct Bedrock {
    pub(super) config: BedrockConfig,
    client: reqwest::Client,
    region: Region,
    endpoint: reqwest::Url,
    pub(super) control_plane_endpoint: reqwest::Url,
    credentials: RwLock<SharedCredentialsProvider>,
    managed_credentials: bool,
}

impl std::fmt::Debug for Bedrock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bedrock")
            .field("config", &self.config)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl Bedrock {
    /// Replaces the runtime HTTP client; callers must disable redirects.
    /// Client timeouts apply per attempt; `max_attempts` still limits HTTP retries.
    /// Transport errors, including timeouts, are never retried.
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Runs AWS credential discovery, then binds the provider to its settings.
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
        validate_origin(&endpoint)?;
        let control_plane_endpoint = config
            .control_plane_endpoint_url
            .clone()
            .unwrap_or_else(|| format!("https://bedrock.{}.amazonaws.com", region.as_ref()))
            .parse::<reqwest::Url>()
            .context(error::EndpointSnafu)?;
        validate_origin(&control_plane_endpoint)?;
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
            control_plane_endpoint,
            credentials: RwLock::new(credentials),
            managed_credentials: false,
        })
    }

    async fn send(&self, path: &str, body: Value) -> Result<reqwest::Response> {
        let body = Bytes::from(serde_json::to_vec(&body).context(error::EncodeRequestSnafu)?);
        let mut url = self.endpoint.clone();
        url.set_path(path);
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(url.as_str())
            .header("content-type", "application/json")
            .header(
                "accept",
                if self.config.protocol == Protocol::Converse {
                    "application/vnd.amazon.eventstream"
                } else {
                    "text/event-stream"
                },
            );
        if self.config.protocol == Protocol::AnthropicMessages {
            builder = builder.header("anthropic-version", &self.config.anthropic_version);
            if !self.config.anthropic_beta.is_empty() {
                builder = builder.header("anthropic-beta", self.config.anthropic_beta.join(","));
            }
        }
        self.send_signed(builder.body(body).context(error::RequestSnafu)?)
            .await
    }

    pub(super) async fn send_signed(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<reqwest::Response> {
        for attempt in 0..self.config.max_attempts {
            let mut request = request.clone();
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
                request.method().as_str(),
                request.uri().to_string(),
                headers.into_iter(),
                SignableBody::Bytes(request.body()),
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
            let mut truncated = false;
            while let Some(chunk) = response.chunk().await.context(error::TransportSnafu)? {
                let remaining = ERROR_BODY_LIMIT.saturating_sub(body.len());
                body.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
                if chunk.len() > remaining {
                    truncated = true;
                    break;
                }
            }
            if matches!(status, 429 | 503) && attempt + 1 < self.config.max_attempts {
                let delay = retry_after.map_or_else(
                    || Duration::from_millis((200 * (1_u64 << attempt)).min(30_000)),
                    |seconds| Duration::from_secs(seconds.min(30)),
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            if truncated {
                return Err(error::ErrorBodyTooLargeSnafu {
                    status,
                    request_id,
                    error_type,
                    body_prefix: body,
                    limit: ERROR_BODY_LIMIT,
                }
                .build()
                .into());
            }
            return Err(error::HttpSnafu {
                status,
                request_id,
                error_type,
                body,
            }
            .build()
            .into());
        }
        unreachable!("validated max_attempts is positive")
    }
}

#[async_trait]
impl ProviderImpl for Bedrock {
    fn name(&self) -> &'static str {
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
            model.provider.name() == self.name(),
            provider_error::ModelProviderMismatchSnafu {
                expected: self.name(),
                actual: model.provider.name(),
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
        ensure!(
            !model.thinking_required() || options.thinking != ThinkingConfig::Off,
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
            Protocol::Converse => {
                ensure!(
                    !matches!(model.id.as_str(), "" | "." | ".."),
                    error::InvalidConverseModelIdSnafu { id: &model.id }
                );
                let protocol_options = ConverseOptions::builder()
                    .maybe_additional_model_request_fields(
                        self.config.converse_additional_model_request_fields.clone(),
                    )
                    .build();
                let body = Converse.request(input, &protocol_options)?;
                let mut url = self.endpoint.clone();
                url.path_segments_mut()
                    .expect("validated endpoint is an HTTP origin")
                    .push("model")
                    .push(&model.id)
                    .push("converse-stream");
                let response = self.send(url.path(), body).await?;
                Ok(Converse.assemble(response, event_tx).await?)
            }
            Protocol::AnthropicMessages => {
                let (thinking, effort) = anthropic_thinking(model, &options.thinking, max_tokens)?;
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
                let effort = openai_reasoning_effort(model, &options.thinking)?;
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
                    Protocol::AnthropicMessages | Protocol::Converse => {
                        unreachable!("native protocol matched by the outer arm")
                    }
                }
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        if self.config.listing_mode == ListingMode::ControlPlane {
            return self.list_control_plane_models().await;
        }
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

fn validate_origin(endpoint: &reqwest::Url) -> Result<()> {
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
    Ok(())
}
