//! `OpenAI` Responses request translation and SSE assembly.

use std::collections::BTreeMap;

use async_trait::async_trait;
use flume::Sender;
use serde::Deserialize;
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};

use super::OpenAiOptions;
use super::error::{
    ApiSnafu, DecodeEventSnafu, DecodeOutputItemSnafu, DecodeReasoningSignatureSnafu,
    DecodeToolArgumentsSnafu, DecodeUsageSnafu, DuplicateOutputIndexSnafu, DuplicateToolItemSnafu,
    EncodeRequestSnafu, EventReceiverSnafu, InvalidReasoningSignatureSnafu, InvalidToolInputSnafu,
    MissingMessageContentSnafu, MissingOutputIndexSnafu, MissingOutputSnafu,
    MissingResponsesReasoningItemSnafu, MissingStringFieldSnafu, RefusalSnafu, ResponseFailedSnafu,
    TruncatedResponsesStreamSnafu, UnknownToolItemSnafu, UnsuccessfulResponseSnafu,
    UnsupportedContentTypeSnafu, UnsupportedLoadedToolsSnafu, UnsupportedOutputItemSnafu,
    UnsupportedResponsesContentSnafu,
};
use super::types::{
    AddedItem, OutputItem, Reasoning, ReasoningSummary, ResponseContent, ResponseInclude,
    ResponseInput, ResponseOutput, ResponseTool, ResponsesRequest, ServiceTier, StreamEvent, Usage,
};
use crate::{
    history::{ContentBlock, Message, MessageKind, Role, ToolContent},
    protocol::{Protocol, ProtocolInput, Result, sse::Events},
    provider::{ProviderEvent, StopReason, StreamResponse, ThinkingConfig, TokenUsage},
};

/// `OpenAI` Responses protocol adapter.
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
        crate::protocol::sampling::validate(input.options, "responses", 2.0, 0)?;
        let mut items = Vec::new();
        for message in input.messages {
            let mut parts = Vec::new();
            for block in &message.content {
                let item = match block {
                    ContentBlock::Text { text } => {
                        let text = text.clone();
                        parts.push(if message.role == Role::Assistant {
                            ResponseContent::OutputText { text }
                        } else {
                            ResponseContent::InputText { text }
                        });
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
                        let item: ResponseInput = serde_json::from_str(signature)
                            .context(DecodeReasoningSignatureSnafu)?;
                        ensure!(
                            matches!(item, ResponseInput::Reasoning { .. }),
                            InvalidReasoningSignatureSnafu
                        );
                        item
                    }
                    ContentBlock::Thinking {
                        signature: None, ..
                    } if message.role == Role::Assistant => {
                        return Err(MissingResponsesReasoningItemSnafu.build().into());
                    }
                    _ => return Err(UnsupportedResponsesContentSnafu.build().into()),
                };
                if !parts.is_empty() {
                    items.push(ResponseInput::Message {
                        role: message.role.clone(),
                        content: std::mem::take(&mut parts),
                    });
                }
                items.push(item);
            }
            if !parts.is_empty() || message.role == Role::System {
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
            let value: Value = serde_json::from_str(&data).context(DecodeEventSnafu)?;
            if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                return Err(ApiSnafu {
                    error: error.clone(),
                }
                .build()
                .into());
            }
            let event = StreamEvent::deserialize(&value).context(DecodeEventSnafu)?;
            #[expect(
                clippy::match_same_arms,
                reason = "empty arms keep ignored event kinds explicit"
            )]
            match event {
                StreamEvent::OutputTextDelta { delta } => {
                    let text = delta.context(MissingStringFieldSnafu { field: "delta" })?;
                    event_tx
                        .send_async(ProviderEvent::TextDelta { text })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                StreamEvent::ReasoningSummaryTextDelta { delta } => {
                    let text = delta.context(MissingStringFieldSnafu { field: "delta" })?;
                    event_tx
                        .send_async(ProviderEvent::ThinkingDelta { text })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                StreamEvent::OutputItemAdded {
                    item:
                        Some(AddedItem::FunctionCall {
                            call_id,
                            id: item_id,
                            name,
                        }),
                } => {
                    let id = call_id.context(MissingStringFieldSnafu { field: "call_id" })?;
                    let item_id = item_id.context(MissingStringFieldSnafu { field: "id" })?;
                    ensure!(
                        calls.insert(item_id.clone(), id.clone()).is_none(),
                        DuplicateToolItemSnafu { id: item_id }
                    );
                    let name = name.context(MissingStringFieldSnafu { field: "name" })?;
                    event_tx
                        .send_async(ProviderEvent::ToolUseStart { id, name })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                StreamEvent::OutputItemAdded { .. } => {}
                StreamEvent::FunctionCallArgumentsDelta { item_id, delta } => {
                    let item_id = item_id.context(MissingStringFieldSnafu { field: "item_id" })?;
                    let id = calls
                        .get(&item_id)
                        .context(UnknownToolItemSnafu {
                            id: item_id.as_str(),
                        })?
                        .clone();
                    let partial_json = delta.context(MissingStringFieldSnafu { field: "delta" })?;
                    event_tx
                        .send_async(ProviderEvent::ToolInputDelta { id, partial_json })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                StreamEvent::OutputItemDone { output_index, item } => {
                    let index = output_index.context(MissingOutputIndexSnafu)?;
                    ensure!(
                        index >= completed && !completed_items.contains_key(&index),
                        DuplicateOutputIndexSnafu { index }
                    );
                    let item = item.unwrap_or_default();
                    let fields = OutputItem::deserialize(&item).context(DecodeOutputItemSnafu)?;
                    let blocks = if fields.is_completed() {
                        Some(Self::output_blocks(&item, fields)?)
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
                StreamEvent::Completed { response } | StreamEvent::Incomplete { response } => {
                    let response = response.unwrap_or_default();
                    if let Some(usage) = response.usage.as_ref().filter(|usage| !usage.is_null()) {
                        let usage = Usage::deserialize(usage).context(DecodeUsageSnafu)?;
                        event_tx
                            .send_async(ProviderEvent::Usage {
                                usage: usage.into(),
                            })
                            .await
                            .context(EventReceiverSnafu)?;
                    }
                    break response;
                }
                StreamEvent::Failed { response } => {
                    let response = response.unwrap_or_default();
                    if let Some(Ok(usage)) = response.usage.as_ref().map(Usage::deserialize) {
                        event_tx
                            .send_async(ProviderEvent::Usage {
                                usage: usage.into(),
                            })
                            .await
                            .context(EventReceiverSnafu)?;
                    }
                    return Err(ResponseFailedSnafu {
                        error: response.error.unwrap_or_default(),
                    }
                    .build()
                    .into());
                }
                StreamEvent::Error => return Err(ApiSnafu { error: value }.build().into()),
                StreamEvent::Unknown => {}
            }
        };
        if let Some(error) = response.error.filter(|error| !error.is_null()) {
            return Err(ApiSnafu { error }.build().into());
        }
        let mut blocks = Vec::new();
        let mut checkpoint_prefix = !checkpoint_blocked;
        for (index, item) in response
            .output
            .context(MissingOutputSnafu)?
            .iter()
            .enumerate()
        {
            let fields = OutputItem::deserialize(item).context(DecodeOutputItemSnafu)?;
            checkpoint_prefix &= fields.is_completed();
            let item_blocks = Self::output_blocks(item, fields)?;
            #[expect(
                clippy::as_conversions,
                reason = "usize output index fits u64 wire counters"
            )]
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
        let reason = response
            .incomplete_details
            .and_then(|details| details.reason);
        let stop = match response.status.as_deref() {
            Some("completed")
                if blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolUse { .. })) =>
            {
                StopReason::ToolUse
            }
            Some("completed") => StopReason::EndTurn,
            Some("incomplete") if reason.as_deref() == Some("max_output_tokens") => {
                StopReason::MaxTokens
            }
            _ => {
                return Err(UnsuccessfulResponseSnafu {
                    status: response.status,
                    reason,
                }
                .build()
                .into());
            }
        };
        let usage: TokenUsage = match response.usage.filter(|usage| !usage.is_null()) {
            Some(usage) => Usage::deserialize(&usage).context(DecodeUsageSnafu)?.into(),
            None => TokenUsage::default(),
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
    /// Converts one completed item; `raw` is kept verbatim as the reasoning signature.
    fn output_blocks(raw: &Value, item: OutputItem) -> Result<Vec<ContentBlock>> {
        let mut blocks = Vec::new();
        match item
            .kind
            .as_deref()
            .context(MissingStringFieldSnafu { field: "type" })?
        {
            "message" => {
                for part in item.content.context(MissingMessageContentSnafu)? {
                    match part
                        .kind
                        .as_deref()
                        .context(MissingStringFieldSnafu { field: "type" })?
                    {
                        "output_text" => blocks.push(ContentBlock::Text {
                            text: part
                                .text
                                .context(MissingStringFieldSnafu { field: "text" })?,
                        }),
                        "refusal" => return Err(RefusalSnafu.build().into()),
                        kind => {
                            return Err(UnsupportedContentTypeSnafu { kind }.build().into());
                        }
                    }
                }
            }
            "function_call" => {
                let id = item
                    .call_id
                    .context(MissingStringFieldSnafu { field: "call_id" })?;
                let name = item
                    .name
                    .context(MissingStringFieldSnafu { field: "name" })?;
                let arguments = item
                    .arguments
                    .context(MissingStringFieldSnafu { field: "arguments" })?;
                let input = if arguments.is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&arguments)
                        .context(DecodeToolArgumentsSnafu { id: &id })?
                };
                ensure!(input.is_object(), InvalidToolInputSnafu { id: &id });
                blocks.push(ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    thought_signature: None,
                });
            }
            "reasoning" => {
                blocks.push(ContentBlock::Thinking {
                    thinking: item
                        .summary
                        .into_iter()
                        .filter_map(|part| part.text)
                        .collect::<Vec<_>>()
                        .join("\n"),
                    signature: Some(raw.to_string()),
                });
            }
            kind => {
                return Err(UnsupportedOutputItemSnafu { kind }.build().into());
            }
        }
        Ok(blocks)
    }
}
