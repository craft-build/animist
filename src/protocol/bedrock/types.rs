use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::history::Role;

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "camelCase")]
pub(super) struct Request {
    pub messages: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<SystemBlock>,
    pub inference_config: InferenceConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<Value>,
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct InputMessage {
    pub role: Role,
    pub content: Vec<InputBlock>,
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct SystemBlock {
    pub text: String,
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "camelCase")]
pub(super) struct InferenceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
}

#[derive(crate::Enum!, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum InputBlock {
    Text(String),
    Image(Image),
    ToolUse(ToolUse),
    ToolResult(ToolResult),
    ReasoningContent(ReasoningContent),
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct Image {
    pub format: String,
    pub source: ImageBytes,
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ImageBytes {
    pub bytes: String,
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolUse {
    pub tool_use_id: String,
    pub name: String,
    pub input: Value,
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolResult {
    pub tool_use_id: String,
    pub content: Vec<ToolResultContent>,
    pub status: ToolStatus,
}

#[derive(crate::Enum!, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ToolStatus {
    #[default]
    Success,
    Error,
}

#[derive(crate::Enum!, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ToolResultContent {
    Text(String),
    Image(Image),
}

#[derive(crate::Enum!, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ReasoningContent {
    ReasoningText(ReasoningText),
    RedactedContent(String),
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ReasoningText {
    pub text: String,
    pub signature: String,
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ToolConfig {
    pub tools: Vec<ToolDefinition>,
}

#[derive(crate::Enum!, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ToolDefinition {
    ToolSpec(ToolSpec),
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: ToolSchema,
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ToolSchema {
    pub json: Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct MessageStart {
    pub role: Role,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BlockStart {
    pub content_block_index: usize,
    pub start: Start,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Start {
    ToolUse(ToolStart),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolStart {
    pub tool_use_id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BlockDelta {
    pub content_block_index: usize,
    pub delta: Delta,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Delta {
    Text(String),
    ToolUse { input: String },
    ReasoningContent(ReasoningDelta),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ReasoningDelta {
    Text(String),
    Signature(String),
    RedactedContent(String),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BlockStop {
    pub content_block_index: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MessageStop {
    pub stop_reason: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Metadata {
    pub usage: Usage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub cache_write_input_tokens: u32,
}
