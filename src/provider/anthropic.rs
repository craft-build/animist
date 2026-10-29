//! Anthropic API-key authentication, routing, and model policy.

use std::collections::BTreeSet;

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};

use super::credential::ApiKey;
use super::http::ensure_success;
use super::thinking::anthropic_thinking;
use super::{
    Effort, Message, Model, ModelInfo, ProviderEvent, ProviderImpl, RequestOptions, Result,
    StreamResponse, Tool, error,
};
use crate::protocol::{
    Protocol, ProtocolInput,
    anthropic::{AnthropicOptions, Messages},
};

/// Serializable Anthropic settings; API keys are never serialized.
#[derive(crate::StructNoDebug!, PartialEq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
#[expect(missing_debug_implementations, reason = "credential-bearing type; Debug omitted by policy")]
pub struct AnthropicConfig {
    /// Explicit key; `None` reads `api_key_env` at construction.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    /// Environment variable consulted when `api_key` is unset.
    #[builder(default = "ANTHROPIC_API_KEY".to_owned())]
    pub api_key_env: String,
    /// API root prepended to request paths.
    #[builder(default = "https://api.anthropic.com/v1".to_owned())]
    pub base_url: String,
    /// `anthropic-version` header sent on every request.
    #[builder(default = "2023-06-01".to_owned())]
    pub version: String,
    /// `anthropic-beta` features appended to every request.
    #[builder(default)]
    pub beta: Vec<String>,
    /// Output cap when neither request nor catalog sets one.
    #[builder(default = 4096_u32)]
    pub max_output_tokens: u32,
    /// Optional metadata; only models returned by Anthropic are listed.
    pub catalog: Option<crate::catalog::ProviderCatalog>,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Anthropic Messages provider over an API-key transport.
#[expect(
    missing_debug_implementations,
    reason = "credential-bearing type; Debug omitted by policy"
)]
pub struct Anthropic {
    config: AnthropicConfig,
    client: reqwest::Client,
    key: ApiKey,
}

impl Anthropic {
    /// Validates credentials and binds the provider to its settings.
    pub fn new(mut config: AnthropicConfig) -> Result<Self> {
        let key = ApiKey::new(config.api_key.take(), &config.api_key_env, "")?;
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
        after_id: Option<&str>,
        fast: bool,
    ) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .request(
                method,
                format!("{}/{}", self.config.base_url.trim_end_matches('/'), path),
            )
            .header("x-api-key", self.key.header().await)
            .header("anthropic-version", &self.config.version);
        let mut beta = self.config.beta.clone();
        if fast && !beta.iter().any(|beta| beta == "fast-mode-2026-02-01") {
            beta.push("fast-mode-2026-02-01".into());
        }
        if !beta.is_empty() {
            request = request.header("anthropic-beta", beta.join(","));
        }
        if let Some(after_id) = after_id {
            request = request.query(&[("after_id", after_id)]);
        }
        if let Some(body) = body {
            request = request.header("Accept", "text/event-stream").json(&body);
        }
        ensure_success(request.send().await.context(error::TransportSnafu)?).await
    }
}

#[derive(crate::Struct!, Deserialize)]
struct ModelPage {
    data: Vec<ListedModel>,
    has_more: bool,
    last_id: Option<String>,
}

#[derive(crate::Struct!, Deserialize)]
struct ListedModel {
    id: String,
    max_tokens: Option<u32>,
    max_input_tokens: Option<u32>,
    capabilities: Option<ModelCapabilities>,
}

#[derive(crate::Struct!, Deserialize)]
struct ModelCapabilities {
    thinking: Option<CapabilitySupport>,
    image_input: Option<CapabilitySupport>,
    effort: Option<EffortCapability>,
}

#[derive(crate::Struct!, Deserialize)]
struct CapabilitySupport {
    supported: bool,
}

#[derive(crate::Struct!, Deserialize)]
struct EffortCapability {
    supported: bool,
    low: Option<CapabilitySupport>,
    medium: Option<CapabilitySupport>,
    high: Option<CapabilitySupport>,
    xhigh: Option<CapabilitySupport>,
    max: Option<CapabilitySupport>,
}

#[async_trait]
impl ProviderImpl for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
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
            model.provider.name() == self.name(),
            error::ModelProviderMismatchSnafu {
                expected: self.name(),
                actual: model.provider.name(),
            }
        );
        ensure!(
            !options.fast || model.overrides.fast != Some(false),
            error::FastModeDisabledSnafu
        );
        let max_tokens = options
            .max_output_tokens
            .or(model.info.config.max_output_tokens)
            .unwrap_or(self.config.max_output_tokens);
        let (thinking, effort) = anthropic_thinking(model, &options.thinking, max_tokens)?;
        let protocol_options = AnthropicOptions::builder()
            .max_tokens(max_tokens)
            .thinking(thinking)
            .maybe_effort(effort)
            .fast(options.fast)
            .build();
        let input = ProtocolInput::builder()
            .model(model)
            .messages(messages)
            .system(system)
            .tools(tools)
            .options(&options)
            .build();
        let body = Messages.request(input, &protocol_options)?;
        let response = self
            .send(
                reqwest::Method::POST,
                "messages",
                Some(body),
                None,
                options.fast,
            )
            .await?;
        Ok(Messages.assemble(response, event_tx).await?)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let mut models = Vec::new();
        let mut after_id = None;
        let mut cursors = BTreeSet::new();
        loop {
            let response = self
                .send(
                    reqwest::Method::GET,
                    "models",
                    None,
                    after_id.as_deref(),
                    false,
                )
                .await?;
            let bytes = crate::protocol::body::read_body(response).await?;
            let page: ModelPage =
                serde_json::from_slice(&bytes).context(error::DecodeModelsSnafu)?;
            for model in page.data {
                let mut info = self
                    .config
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.models.get(&model.id))
                    .map(|model| model.info.clone())
                    .unwrap_or_default();
                info.ids = vec![model.id];
                if let Some(limit) = model.max_tokens {
                    info.config.max_output_tokens = Some(limit);
                }
                if let Some(limit) = model.max_input_tokens {
                    info.config.context_window = Some(limit);
                }
                if let Some(capabilities) = model.capabilities {
                    if let Some(thinking) = capabilities.thinking {
                        info.features.thinking.supported = thinking.supported;
                        if !thinking.supported {
                            info.features.thinking.required = false;
                            info.features.thinking.efforts.clear();
                        }
                    }
                    if let Some(image) = capabilities.image_input {
                        info.features.vision = image.supported;
                    }
                    if let Some(effort) = capabilities.effort {
                        info.features.thinking.efforts = [
                            (Effort::Low, effort.low),
                            (Effort::Medium, effort.medium),
                            (Effort::High, effort.high),
                            (Effort::XHigh, effort.xhigh),
                            (Effort::Max, effort.max),
                        ]
                        .into_iter()
                        .filter_map(|(level, support)| {
                            (effort.supported && support.is_some_and(|support| support.supported))
                                .then_some(level)
                        })
                        .collect();
                    }
                }
                models.push(info);
            }
            if !page.has_more {
                return Ok(models);
            }
            let cursor = page.last_id.context(error::MissingModelCursorSnafu)?;
            ensure!(!cursor.is_empty(), error::MissingModelCursorSnafu);
            ensure!(
                cursors.insert(cursor.clone()),
                error::RepeatedModelCursorSnafu { cursor: &cursor }
            );
            after_id = Some(cursor);
        }
    }

    async fn refresh_auth(&self) -> Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> Result<()> {
        self.key.reload().await
    }
}
