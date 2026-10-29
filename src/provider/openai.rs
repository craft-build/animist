//! `OpenAI` API-key provider with explicit protocol selection.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::ResultExt;

use super::credential::ApiKey;
use super::http::ensure_success;
use super::thinking::openai_reasoning_effort;
use super::{
    Message, Model, ModelInfo, ProviderEvent, ProviderImpl, RequestOptions, Result, StreamResponse,
    Tool, error,
};
use crate::protocol::{
    Protocol as ProtocolAdapter, ProtocolInput,
    openai::{ChatCompletions, OpenAiOptions, Responses},
};

/// Wire protocol used for chat requests; `Responses` is OpenAI-native.
#[derive(crate::Enum!, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// `OpenAI` Chat Completions compatibility surface.
    ChatCompletions,
    /// `OpenAI` Responses API surface.
    #[default]
    Responses,
}

/// `GET /models` page shared by OpenAI-compatible servers.
#[derive(crate::Struct!, Deserialize)]
pub(super) struct ModelList {
    pub data: Vec<ListedModel>,
}

/// One `GET /models` entry; other fields are not used.
#[derive(crate::Struct!, Deserialize)]
pub(super) struct ListedModel {
    pub id: String,
}

/// Serializable provider settings; API keys are never serialized.
#[derive(crate::StructNoDebug!, PartialEq, Builder, Serialize, Deserialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
#[expect(missing_debug_implementations, reason = "credential-bearing type; Debug omitted by policy")]
pub struct OpenAiConfig {
    /// Explicit key; `None` reads `api_key_env` at construction.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    /// Environment variable consulted when `api_key` is unset.
    #[builder(default = "OPENAI_API_KEY".to_owned())]
    pub api_key_env: String,
    /// API root prepended to request paths.
    #[builder(default = "https://api.openai.com/v1".to_owned())]
    pub base_url: String,
    /// Wire protocol for chat requests.
    #[builder(default)]
    pub protocol: Protocol,
    /// `OpenAI-Organization` header, when the key belongs to one.
    pub organization: Option<String>,
    /// `OpenAI-Project` header, when the key is project-scoped.
    pub project: Option<String>,
    /// Optional catalog metadata; only models returned by `OpenAI` are listed.
    pub catalog: Option<crate::catalog::ProviderCatalog>,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// `OpenAI` provider over an API-key transport, with explicit protocol choice.
#[expect(
    missing_debug_implementations,
    reason = "credential-bearing type; Debug omitted by policy"
)]
pub struct OpenAi {
    config: OpenAiConfig,
    client: reqwest::Client,
    key: ApiKey,
}

impl OpenAi {
    /// Validates credentials and binds the provider to its settings.
    pub fn new(mut config: OpenAiConfig) -> Result<Self> {
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
            .header(AUTHORIZATION, self.key.header().await);
        if let Some(organization) = &self.config.organization {
            request = request.header("OpenAI-Organization", organization);
        }
        if let Some(project) = &self.config.project {
            request = request.header("OpenAI-Project", project);
        }
        if let Some(body) = body {
            request = request.header("Accept", "text/event-stream").json(&body);
        }
        ensure_success(request.send().await.context(error::TransportSnafu)?).await
    }
}

#[async_trait]
impl ProviderImpl for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
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
        if model.provider.name() != self.name() {
            return error::ModelProviderMismatchSnafu {
                expected: self.name(),
                actual: model.provider.name(),
            }
            .fail();
        }
        if options.fast && model.overrides.fast == Some(false) {
            return error::FastModeDisabledSnafu.fail();
        }
        let protocol_options = OpenAiOptions::builder()
            .maybe_reasoning_effort(openai_reasoning_effort(model, &options.thinking)?)
            .build();
        let (path, protocol): (
            &str,
            &dyn ProtocolAdapter<
                Request = Value,
                Response = reqwest::Response,
                Options = OpenAiOptions,
            >,
        ) = match self.config.protocol {
            Protocol::ChatCompletions => ("chat/completions", &ChatCompletions),
            Protocol::Responses => ("responses", &Responses),
        };
        let input = ProtocolInput::builder()
            .model(model)
            .messages(messages)
            .system(system)
            .tools(tools)
            .options(&options)
            .build();
        let body = protocol.request(input, &protocol_options)?;
        let response = self.send(reqwest::Method::POST, path, Some(body)).await?;
        Ok(protocol.assemble(response, event_tx).await?)
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
        self.key.reload().await
    }
}
