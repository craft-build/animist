//! OpenAI Responses request translation and SSE assembly.

use std::collections::BTreeMap;

use async_trait::async_trait;
use flume::Sender;
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};

use super::{OpenAiOptions, error::*, types::*};
use crate::{
    history::{ContentBlock, Message, MessageKind, Role, ToolContent},
    protocol::{Protocol, ProtocolInput, Result, sse::Events},
    provider::{ProviderEvent, StopReason, StreamResponse, ThinkingConfig, TokenUsage},
};

#[derive(crate::Struct!)]
pub struct Responses;

#[async_trait]
impl Protocol for Responses {
    type Request = Value;
    type Response = reqwest::Response;
    type Options = OpenAiOptions;

    fn request(&self, input: ProtocolInput<'_>, options: &Self::Options) -> Result<Self::Request> {
        crate::protocol::validate_replay(input.messages, input.tools)?;
        for (control, explicit) in [
            (
                "frequency_penalty",
                input.options.frequency_penalty.is_some(),
            ),
            ("presence_penalty", input.options.presence_penalty.is_some()),
            ("stop_sequences", input.options.stop_sequences.is_some()),
        ] {
            ensure!(
                !explicit,
                crate::protocol::sampling::UnsupportedSnafu {
                    boundary: "responses",
                    control,
                }
            );
        }
        crate::protocol::sampling::validate(input.options, "responses", 2.0, 0.0, 0)?;
        let mut items = Vec::new();
        for message in input.messages {
            let mut parts = Vec::new();
            for block in &message.content {
                let item = match block {
                    ContentBlock::Text { text } => {
                        parts.push(ResponseContent::InputText { text: text.clone() });
                        continue;
                    }
                    ContentBlock::Image { source } if message.role == Role::User => {
                        parts.push(ResponseContent::InputImage {
                            image_url: source.to_url(),
                        });
                        continue;
                    }
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        thought_signature: None,
                    } if message.role == Role::Assistant => ResponseInput::FunctionCall {
                        call_id: id.clone(),
                        name: name.clone(),
                        arguments: input.to_string(),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        loaded_tools,
                        ..
                    } if message.role == Role::User => {
                        ensure!(
                            loaded_tools.is_empty(),
                            UnsupportedLoadedToolsSnafu { id: tool_use_id }
                        );
                        let output = match content.0.as_slice() {
                            [ToolContent::Text { text }] => ResponseOutput::Text(text.clone()),
                            parts => ResponseOutput::Parts(
                                parts
                                    .iter()
                                    .map(|part| match part {
                                        ToolContent::Text { text } => {
                                            ResponseContent::InputText { text: text.clone() }
                                        }
                                        ToolContent::Image { source } => {
                                            ResponseContent::InputImage {
                                                image_url: source.to_url(),
                                            }
                                        }
                                    })
                                    .collect(),
                            ),
                        };
                        ResponseInput::FunctionCallOutput {
                            call_id: tool_use_id.clone(),
                            output,
                        }
                    }
                    ContentBlock::Thinking {
                        signature: Some(signature),
                        ..
                    } if message.role == Role::Assistant => {
                        let item: Value = serde_json::from_str(signature)
                            .context(DecodeReasoningSignatureSnafu)?;
                        ensure!(item["type"] == "reasoning", InvalidReasoningSignatureSnafu);
                        serde_json::from_value(item).context(DecodeReasoningSignatureSnafu)?
                    }
                    ContentBlock::Thinking {
                        signature: None, ..
                    } if message.role == Role::Assistant => {
                        return Err(OpenAIProtocolError::MissingResponsesReasoningItem.into());
                    }
                    _ => return Err(OpenAIProtocolError::UnsupportedResponsesContent.into()),
                };
                if !parts.is_empty() {
                    items.push(ResponseInput::Message {
                        role: message.role.clone(),
                        content: std::mem::take(&mut parts),
                    });
                }
                items.push(item);
            }
            if !parts.is_empty() {
                items.push(ResponseInput::Message {
                    role: message.role.clone(),
                    content: parts,
                });
            }
        }
        let tools = input
            .tools
            .iter()
            .map(|tool| ResponseTool::Function {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
                strict: false,
            })
            .collect::<Vec<_>>();
        let summary = (!matches!(input.options.thinking, ThinkingConfig::Off))
            .then_some(ReasoningSummary::Auto);
        let reasoning = if options.reasoning_effort.is_some()
            || matches!(input.options.thinking, ThinkingConfig::Adaptive)
        {
            Some(
                Reasoning::builder()
                    .maybe_effort(options.reasoning_effort.clone())
                    .maybe_summary(summary)
                    .build(),
            )
        } else {
            None
        };
        let request = ResponsesRequest::builder()
            .model(&input.model.id)
            .input(items)
            .stream(true)
            .store(false)
            .include(vec![ResponseInclude::EncryptedReasoning])
            .maybe_temperature(input.options.temperature)
            .maybe_top_p(input.options.top_p)
            .maybe_instructions((!input.system.is_empty()).then_some(input.system))
            .tools(tools)
            .maybe_max_output_tokens(
                input
                    .options
                    .max_output_tokens
                    .or(input.model.info.config.max_output_tokens),
            )
            .maybe_reasoning(reasoning)
            .maybe_service_tier(input.options.fast.then_some(ServiceTier::Priority))
            .build();
        Ok(serde_json::to_value(request).context(EncodeRequestSnafu)?)
    }

    async fn assemble(
        &self,
        response: Self::Response,
        event_tx: &Sender<ProviderEvent>,
    ) -> Result<StreamResponse> {
        let mut events = Events::new(response);
        let mut calls = BTreeMap::new();
        let mut completed_items = BTreeMap::new();
        let mut completed = 0_u64;
        let mut checkpoint_blocked = false;
        let response = loop {
            let data = events
                .next()
                .await?
                .context(TruncatedResponsesStreamSnafu)?;
            let event: Value = serde_json::from_str(&data).context(DecodeEventSnafu)?;
            if let Some(error) = event.get("error").filter(|error| !error.is_null()) {
                return Err(OpenAIProtocolError::Api {
                    error: error.clone(),
                }
                .into());
            }
            match event["type"]
                .as_str()
                .context(MissingStringFieldSnafu { field: "type" })?
            {
                "response.output_text.delta" => {
                    let text = event["delta"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "delta" })?
                        .to_owned();
                    event_tx
                        .send_async(ProviderEvent::TextDelta { text })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                "response.reasoning_summary_text.delta" => {
                    let text = event["delta"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "delta" })?
                        .to_owned();
                    event_tx
                        .send_async(ProviderEvent::ThinkingDelta { text })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                "response.output_item.added" if event["item"]["type"] == "function_call" => {
                    let item = &event["item"];
                    let id = item["call_id"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "call_id" })?
                        .to_owned();
                    let item_id = item["id"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "id" })?;
                    ensure!(
                        calls.insert(item_id.to_owned(), id.clone()).is_none(),
                        DuplicateToolItemSnafu { id: item_id }
                    );
                    let name = item["name"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "name" })?
                        .to_owned();
                    event_tx
                        .send_async(ProviderEvent::ToolUseStart { id, name })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                "response.function_call_arguments.delta" => {
                    let item_id = event["item_id"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "item_id" })?;
                    let id = calls
                        .get(item_id)
                        .context(UnknownToolItemSnafu { id: item_id })?
                        .clone();
                    let partial_json = event["delta"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "delta" })?
                        .to_owned();
                    event_tx
                        .send_async(ProviderEvent::ToolInputDelta { id, partial_json })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                "response.output_item.done" => {
                    let index = event["output_index"]
                        .as_u64()
                        .context(MissingOutputIndexSnafu)?;
                    ensure!(
                        index >= completed && !completed_items.contains_key(&index),
                        DuplicateOutputIndexSnafu { index }
                    );
                    let status = &event["item"]["status"];
                    let blocks = if status.is_null() || status == "completed" {
                        Some(Self::output_blocks(&event["item"])?)
                    } else {
                        None
                    };
                    completed_items.insert(index, blocks);
                    while let Some(item) = completed_items.remove(&completed) {
                        match item {
                            Some(blocks) if !checkpoint_blocked => {
                                for block in blocks {
                                    event_tx
                                        .send_async(ProviderEvent::ContentComplete { block })
                                        .await
                                        .context(EventReceiverSnafu)?;
                                }
                            }
                            None => checkpoint_blocked = true,
                            _ => {}
                        }
                        completed += 1;
                    }
                }
                "response.completed" | "response.incomplete" => {
                    if !event["response"]["usage"].is_null() {
                        let usage =
                            serde_json::from_value::<Usage>(event["response"]["usage"].clone())
                                .context(DecodeUsageSnafu)?
                                .into();
                        event_tx
                            .send_async(ProviderEvent::Usage { usage })
                            .await
                            .context(EventReceiverSnafu)?;
                    }
                    break event["response"].clone();
                }
                "response.failed" => {
                    if let Ok(usage) =
                        serde_json::from_value::<Usage>(event["response"]["usage"].clone())
                    {
                        event_tx
                            .send_async(ProviderEvent::Usage {
                                usage: usage.into(),
                            })
                            .await
                            .context(EventReceiverSnafu)?;
                    }
                    return Err(OpenAIProtocolError::ResponseFailed {
                        error: event["response"]["error"].clone(),
                    }
                    .into());
                }
                "error" => return Err(OpenAIProtocolError::Api { error: event }.into()),
                _ => {}
            }
        };
        if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
            return Err(OpenAIProtocolError::Api {
                error: error.clone(),
            }
            .into());
        }
        let mut blocks = Vec::new();
        let mut checkpoint_prefix = !checkpoint_blocked;
        for (index, item) in response["output"]
            .as_array()
            .context(MissingOutputSnafu)?
            .iter()
            .enumerate()
        {
            let item_blocks = Self::output_blocks(item)?;
            checkpoint_prefix &= item["status"].is_null() || item["status"] == "completed";
            if index as u64 >= completed && checkpoint_prefix {
                for block in &item_blocks {
                    event_tx
                        .send_async(ProviderEvent::ContentComplete {
                            block: block.clone(),
                        })
                        .await
                        .context(EventReceiverSnafu)?;
                }
            }
            blocks.extend(item_blocks);
        }
        let stop = match response["status"].as_str() {
            Some("completed")
                if blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolUse { .. })) =>
            {
                StopReason::ToolUse
            }
            Some("completed") => StopReason::EndTurn,
            Some("incomplete")
                if response["incomplete_details"]["reason"] == "max_output_tokens" =>
            {
                StopReason::MaxTokens
            }
            _ => {
                return Err(OpenAIProtocolError::UnsuccessfulResponse {
                    status: response["status"].clone(),
                    reason: response["incomplete_details"]["reason"].clone(),
                }
                .into());
            }
        };
        let usage: TokenUsage = if response["usage"].is_null() {
            TokenUsage::default()
        } else {
            serde_json::from_value::<Usage>(response["usage"].clone())
                .context(DecodeUsageSnafu)?
                .into()
        };
        Ok(StreamResponse::builder()
            .message(
                Message::builder()
                    .role(Role::Assistant)
                    .content(blocks)
                    .kind(MessageKind::Turn)
                    .build(),
            )
            .usage(usage)
            .stop_reason(stop)
            .input_transformations(Vec::new())
            .build())
    }
}

impl Responses {
    fn output_blocks(item: &Value) -> Result<Vec<ContentBlock>> {
        let mut blocks = Vec::new();
        match item["type"]
            .as_str()
            .context(MissingStringFieldSnafu { field: "type" })?
        {
            "message" => {
                for part in item["content"]
                    .as_array()
                    .context(MissingMessageContentSnafu)?
                {
                    match part["type"]
                        .as_str()
                        .context(MissingStringFieldSnafu { field: "type" })?
                    {
                        "output_text" => blocks.push(ContentBlock::Text {
                            text: part["text"]
                                .as_str()
                                .context(MissingStringFieldSnafu { field: "text" })?
                                .to_owned(),
                        }),
                        "refusal" => return Err(OpenAIProtocolError::Refusal.into()),
                        kind => {
                            return Err(OpenAIProtocolError::UnsupportedContentType {
                                kind: kind.into(),
                            }
                            .into());
                        }
                    }
                }
            }
            "function_call" => {
                let id = item["call_id"]
                    .as_str()
                    .context(MissingStringFieldSnafu { field: "call_id" })?;
                let name = item["name"]
                    .as_str()
                    .context(MissingStringFieldSnafu { field: "name" })?
                    .to_owned();
                let arguments = item["arguments"]
                    .as_str()
                    .context(MissingStringFieldSnafu { field: "arguments" })?;
                let input =
                    serde_json::from_str(arguments).context(DecodeToolArgumentsSnafu { id })?;
                blocks.push(ContentBlock::ToolUse {
                    id: id.into(),
                    name,
                    input,
                    thought_signature: None,
                });
            }
            "reasoning" => {
                let summary = item["summary"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                blocks.push(ContentBlock::Thinking {
                    thinking: summary,
                    signature: Some(item.to_string()),
                });
            }
            kind => {
                return Err(
                    OpenAIProtocolError::UnsupportedOutputItem { kind: kind.into() }.into(),
                );
            }
        }
        Ok(blocks)
    }
}
