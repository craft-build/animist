//! `OpenAI` API-key provider with explicit protocol selection.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};
use tokio::sync::RwLock;

use super::{
    Effort, Message, Model, ModelInfo, ProviderEvent, ProviderImpl, RequestOptions, Result,
    StreamResponse, ThinkingConfig, ThinkingSupport, Tool, error, read_error_body,
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
    key: RwLock<String>,
    from_env: bool,
}

impl OpenAi {
    /// Validates credentials and binds the provider to its settings.
    pub fn new(mut config: OpenAiConfig) -> Result<Self> {
        let from_env = config.api_key.is_none();
        let key = match config.api_key.take() {
            Some(key) if !key.trim().is_empty() => key,
            Some(_) => {
                return error::EmptyApiKeySnafu.fail();
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
        Ok(Self {
            config,
            client: reqwest::Client::new(),
            key: RwLock::new(key),
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
        let key = self.key.read().await.clone();
        let mut request = self
            .client
            .request(
                method,
                format!("{}/{}", self.config.base_url.trim_end_matches('/'), path),
            )
            .bearer_auth(key);
        if let Some(organization) = &self.config.organization {
            request = request.header("OpenAI-Organization", organization);
        }
        if let Some(project) = &self.config.project {
            request = request.header("OpenAI-Project", project);
        }
        if let Some(body) = body {
            request = request.header("Accept", "text/event-stream").json(&body);
        }
        let response = request.send().await.context(error::TransportSnafu)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let (body, body_truncated) = read_error_body(response).await?;
            return error::HttpSnafu {
                status,
                body,
                body_truncated,
            }
            .fail();
        }
        Ok(response)
    }
}

fn reasoning_effort(model: &Model, options: &RequestOptions) -> Result<Option<&'static str>> {
    let support = match model.overrides.thinking {
        Some(ThinkingSupport::No) => false,
        Some(ThinkingSupport::Yes | ThinkingSupport::Required) => true,
        None => model.info.features.thinking.supported,
    };
    let required = match model.overrides.thinking {
        Some(ThinkingSupport::Required) => true,
        Some(_) => false,
        None => model.info.features.thinking.required,
    };
    match &options.thinking {
        ThinkingConfig::Off if required => error::ReasoningRequiredSnafu.fail(),
        ThinkingConfig::Off if support => Ok(Some("none")),
        ThinkingConfig::Off | ThinkingConfig::Adaptive => Ok(None),
        ThinkingConfig::Budget(_) => error::ReasoningBudgetUnsupportedSnafu.fail(),
        ThinkingConfig::Effort(effort) => {
            ensure!(
                support
                    && !matches!(effort, Effort::Adaptive | Effort::Max)
                    && model.info.features.thinking.efforts.contains(effort),
                error::UnsupportedReasoningEffortSnafu {
                    effort: effort.clone()
                }
            );
            Ok(effort.openai_wire())
        }
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
            .maybe_reasoning_effort(reasoning_effort(model, &options)?)
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
            *self.key.write().await = key;
        }
        Ok(())
    }
}
