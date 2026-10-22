use std::collections::BTreeMap;

use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{Role, TokenUsage};

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    pub stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct StreamOptions {
    pub include_usage: bool,
}

#[derive(crate::Enum!, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub(super) enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: Vec<ChatContent>,
    },
    Assistant {
        content: Option<Vec<ChatContent>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ChatCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(crate::Enum!, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ChatContent {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(crate::Struct!, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ImageUrl {
    pub url: String,
}

#[derive(crate::Enum!, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ChatCall {
    Function { id: String, function: FunctionCall },
}

#[derive(crate::Struct!, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(crate::Enum!, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ChatTool {
    Function { function: FunctionDefinition },
}

#[derive(crate::Struct!, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(crate::Enum!, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ServiceTier {
    Priority,
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ResponsesRequest {
    pub model: String,
    pub input: Vec<ResponseInput>,
    pub stream: bool,
    pub store: bool,
    pub include: Vec<ResponseInclude>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ResponseTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

#[derive(crate::Enum!, Serialize)]
pub(super) enum ResponseInclude {
    #[serde(rename = "reasoning.encrypted_content")]
    EncryptedReasoning,
}

#[derive(crate::Enum!, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponseInput {
    Message {
        role: Role,
        content: Vec<ResponseContent>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    Reasoning {
        #[serde(flatten)]
        fields: BTreeMap<String, Value>,
    },
}

#[derive(crate::Enum!, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponseContent {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(crate::Enum!, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponseTool {
    Function {
        name: String,
        description: String,
        parameters: Value,
        strict: bool,
    },
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
}

#[derive(crate::Enum!, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReasoningSummary {
    Auto,
}

#[derive(crate::Struct!, Deserialize)]
pub(super) struct ChatChunk {
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(crate::Struct!, Deserialize)]
pub(super) struct Choice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(crate::Struct!, Deserialize)]
pub(super) struct Delta {
    pub content: Option<String>,
    pub refusal: Option<String>,
    pub tool_calls: Option<Vec<ToolDelta>>,
}

#[derive(crate::Struct!, Deserialize)]
pub(super) struct ToolDelta {
    pub index: u64,
    pub id: Option<String>,
    pub function: FunctionDelta,
}

#[derive(crate::Struct!, Deserialize)]
pub(super) struct FunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(crate::Struct!, Deserialize)]
#[serde(default)]
pub(super) struct Usage {
    #[serde(alias = "prompt_tokens")]
    pub input_tokens: u32,
    #[serde(alias = "completion_tokens")]
    pub output_tokens: u32,
    #[serde(alias = "prompt_tokens_details")]
    pub input_tokens_details: Option<InputTokenDetails>,
}

#[derive(crate::Struct!, Deserialize)]
#[serde(default)]
pub(super) struct InputTokenDetails {
    pub cached_tokens: u32,
}

impl From<Usage> for TokenUsage {
    fn from(usage: Usage) -> Self {
        let cached = usage.input_tokens_details.unwrap_or_default().cached_tokens;
        Self::builder()
            .input(usage.input_tokens.saturating_sub(cached))
            .output(usage.output_tokens)
            .cache_creation(0_u32)
            .cache_read(cached)
            .build()
    }
}
