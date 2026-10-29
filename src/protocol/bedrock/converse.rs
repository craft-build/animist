//! Converse request translation and request-owned `EventStream` assembly.

use std::collections::BTreeSet;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bon::Builder;
use flume::Sender;
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};

use super::{error, eventstream::Frames, types};
use crate::{
    history::{ContentBlock, ImageSource, Message, MessageKind, Role, ToolContent},
    protocol::{Protocol, ProtocolInput, Result, sampling, validate_replay},
    provider::{
        ProviderEvent, StopReason, StreamResponse, SystemMessagePolicy, ThinkingConfig, TokenUsage,
    },
};

/// Bedrock Converse protocol, independent of AWS authentication and routing.
#[derive(crate::Struct!)]
pub struct Converse;

/// Explicit model-specific fields, without guessing a model's parameter dialect.
#[derive(crate::Struct!, PartialEq, Builder)]
#[builder(on(_, into))]
pub struct ConverseOptions {
    /// Model-documented settings beyond Converse's portable inference controls.
    pub additional_model_request_fields: Option<Value>,
}

#[derive(Debug)]
enum AssemblyBlock {
    Text(String),
    Thinking {
        text: String,
        signature: String,
    },
    Redacted(Vec<u8>),
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

#[derive(Debug)]
enum Block {
    Open(AssemblyBlock),
    Closed(ContentBlock),
}

#[async_trait]
impl Protocol for Converse {
    type Request = Value;
    type Response = reqwest::Response;
    type Options = ConverseOptions;

    fn request(&self, input: ProtocolInput<'_>, options: &Self::Options) -> Result<Self::Request> {
        validate_replay(input.messages, input.tools)?;
        sampling::validate(input.options, "bedrock_converse", 1.0, 0.0, 4)?;
        for (control, explicit) in [
            (
                "frequency_penalty",
                input.options.frequency_penalty.is_some(),
            ),
            ("presence_penalty", input.options.presence_penalty.is_some()),
        ] {
            ensure!(
                !explicit,
                sampling::UnsupportedSnafu {
                    boundary: "bedrock_converse",
                    control
                }
            );
        }
        ensure!(
            input.options.thinking == ThinkingConfig::Off,
            error::UnsupportedThinkingSnafu {
                thinking: input.options.thinking.clone()
            }
        );
        ensure!(!input.options.fast, error::UnsupportedFastModeSnafu);
        ensure!(
            input.options.max_output_tokens != Some(0),
            error::ZeroMaxTokensSnafu
        );
        ensure!(
            options
                .additional_model_request_fields
                .as_ref()
                .is_none_or(Value::is_object),
            error::InvalidAdditionalFieldsSnafu
        );
        let mut system = Vec::new();
        if !input.system.is_empty() {
            system.push(types::SystemBlock::builder().text(input.system).build());
        }
        let mut messages = Vec::new();
        for (index, message) in input.messages.iter().enumerate() {
            if message.role == Role::System {
                ensure!(
                    input.options.system_message_policy == SystemMessagePolicy::MoveToPreamble,
                    error::UnsupportedSystemPlacementSnafu { message: index }
                );
                for block in &message.content {
                    if let ContentBlock::Text { text } = block {
                        system.push(types::SystemBlock::builder().text(text).build());
                    }
                }
                continue;
            }
            let mut content = Vec::new();
            for block in &message.content {
                content.push(match block {
                    ContentBlock::Text { text } => types::InputBlock::Text(text.clone()),
                    ContentBlock::Image { source } if message.role == Role::User => {
                        types::InputBlock::Image(types::Image::try_from(source)?)
                    }
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => {
                        let signature = signature
                            .as_ref()
                            .filter(|value| !value.is_empty())
                            .context(error::MissingReplaySignatureSnafu)?;
                        types::InputBlock::ReasoningContent(types::ReasoningContent::ReasoningText(
                            types::ReasoningText::builder()
                                .text(thinking)
                                .signature(signature)
                                .build(),
                        ))
                    }
                    ContentBlock::RedactedThinking { data } => {
                        STANDARD.decode(data).context(error::Base64Snafu)?;
                        types::InputBlock::ReasoningContent(
                            types::ReasoningContent::RedactedContent(data.clone()),
                        )
                    }
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        thought_signature: None,
                    } => {
                        ensure!(input.is_object(), error::InvalidToolInputSnafu { id });
                        types::InputBlock::ToolUse(
                            types::ToolUse::builder()
                                .tool_use_id(id)
                                .name(name)
                                .input(input.clone())
                                .build(),
                        )
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        loaded_tools,
                        ..
                    } => {
                        ensure!(loaded_tools.is_empty(), error::UnsupportedLoadedToolsSnafu);
                        let content = content
                            .0
                            .iter()
                            .map(|part| match part {
                                ToolContent::Text { text } => {
                                    Ok(types::ToolResultContent::Text(text.clone()))
                                }
                                ToolContent::Image { source } => {
                                    Ok(types::ToolResultContent::Image(types::Image::try_from(
                                        source,
                                    )?))
                                }
                            })
                            .collect::<Result<Vec<_>>>()?;
                        types::InputBlock::ToolResult(
                            types::ToolResult::builder()
                                .tool_use_id(tool_use_id)
                                .content(content)
                                .status(if *is_error {
                                    types::ToolStatus::Error
                                } else {
                                    types::ToolStatus::Success
                                })
                                .build(),
                        )
                    }
                    _ => {
                        return Err(error::UnsupportedContentSnafu {
                            role: message.role.clone(),
                        }
                        .build()
                        .into());
                    }
                });
            }
            messages.push(
                types::InputMessage::builder()
                    .role(message.role.clone())
                    .content(content)
                    .build(),
            );
        }
        let tools = input
            .tools
            .iter()
            .map(|tool| {
                types::ToolDefinition::ToolSpec(
                    types::ToolSpec::builder()
                        .name(&tool.name)
                        .description(&tool.description)
                        .input_schema(
                            types::ToolSchema::builder()
                                .json(tool.input_schema.clone())
                                .build(),
                        )
                        .build(),
                )
            })
            .collect::<Vec<_>>();
        let request = types::Request::builder()
            .messages(messages)
            .system(system)
            .inference_config(
                types::InferenceConfig::builder()
                    .maybe_max_tokens(input.options.max_output_tokens)
                    .maybe_temperature(input.options.temperature)
                    .maybe_top_p(input.options.top_p)
                    .maybe_stop_sequences(input.options.stop_sequences.clone())
                    .build(),
            )
            .maybe_tool_config(
                (!tools.is_empty()).then(|| types::ToolConfig::builder().tools(tools).build()),
            )
            .maybe_additional_model_request_fields(options.additional_model_request_fields.clone())
            .build();
        Ok(serde_json::to_value(request).context(error::EncodeRequestSnafu)?)
    }

    async fn assemble(
        &self,
        response: Self::Response,
        event_tx: &Sender<ProviderEvent>,
    ) -> Result<StreamResponse> {
        let mut frames = Frames::new(response);
        let mut started = false;
        let mut stop_reason = None;
        let mut usage = None;
        let mut blocks = Vec::<Block>::new();
        let mut completed = 0;
        let mut tool_ids = BTreeSet::new();
        while let Some(frame) = frames
            .next()
            .await
            .map_err(super::BedrockProtocolError::from)?
        {
            let message_type = frame
                .header(":message-type")
                .map_err(super::BedrockProtocolError::from)?;
            if matches!(message_type, "exception" | "error") {
                let kind = frame
                    .header(if message_type == "exception" {
                        ":exception-type"
                    } else {
                        ":error-code"
                    })
                    .map_err(super::BedrockProtocolError::from)?
                    .to_owned();
                return Err(error::ApiSnafu {
                    kind,
                    headers: frame.headers,
                    body: frame.payload.to_vec(),
                }
                .build()
                .into());
            }
            ensure!(
                message_type == "event",
                error::UnexpectedEventSnafu {
                    event_type: message_type
                }
            );
            let event_type = frame
                .header(":event-type")
                .map_err(super::BedrockProtocolError::from)?;
            match event_type {
                "messageStart" => {
                    ensure!(!started, error::UnexpectedEventSnafu { event_type });
                    let start: types::MessageStart = serde_json::from_slice(&frame.payload)
                        .context(error::DecodeEventSnafu { event_type })?;
                    ensure!(
                        start.role == Role::Assistant,
                        error::UnexpectedEventSnafu { event_type }
                    );
                    started = true;
                }
                "contentBlockStart" => {
                    ensure!(
                        started && stop_reason.is_none(),
                        error::UnexpectedEventSnafu { event_type }
                    );
                    let start: types::BlockStart = serde_json::from_slice(&frame.payload)
                        .context(error::DecodeEventSnafu { event_type })?;
                    let index = start.content_block_index;
                    ensure!(
                        index == blocks.len(),
                        error::BlockIndexSnafu {
                            index,
                            expected: blocks.len()
                        }
                    );
                    let types::Start::ToolUse(tool) = start.start;
                    ensure!(
                        !tool.tool_use_id.is_empty()
                            && !tool.name.is_empty()
                            && tool_ids.insert(tool.tool_use_id.clone()),
                        error::InvalidToolStartSnafu {
                            id: &tool.tool_use_id
                        }
                    );
                    event_tx
                        .send_async(ProviderEvent::ToolUseStart {
                            id: tool.tool_use_id.clone(),
                            name: tool.name.clone(),
                        })
                        .await
                        .context(error::EventReceiverSnafu)?;
                    blocks.push(Block::Open(AssemblyBlock::Tool {
                        id: tool.tool_use_id,
                        name: tool.name,
                        json: String::new(),
                    }));
                }
                "contentBlockDelta" => {
                    ensure!(
                        started && stop_reason.is_none(),
                        error::UnexpectedEventSnafu { event_type }
                    );
                    let update: types::BlockDelta = serde_json::from_slice(&frame.payload)
                        .context(error::DecodeEventSnafu { event_type })?;
                    let index = update.content_block_index;
                    if index == blocks.len() {
                        let content = match &update.delta {
                            types::Delta::Text(_) => AssemblyBlock::Text(String::new()),
                            types::Delta::ReasoningContent(
                                types::ReasoningDelta::Text(_)
                                | types::ReasoningDelta::Signature(_),
                            ) => AssemblyBlock::Thinking {
                                text: String::new(),
                                signature: String::new(),
                            },
                            types::Delta::ReasoningContent(
                                types::ReasoningDelta::RedactedContent(_),
                            ) => AssemblyBlock::Redacted(Vec::new()),
                            types::Delta::ToolUse { .. } => {
                                return Err(error::BlockIndexSnafu {
                                    index,
                                    expected: blocks.len(),
                                }
                                .build()
                                .into());
                            }
                        };
                        blocks.push(Block::Open(content));
                    }
                    let expected = blocks.len();
                    let block = blocks
                        .get_mut(index)
                        .context(error::BlockIndexSnafu { index, expected })?;
                    let Block::Open(block) = block else {
                        return Err(error::ClosedBlockSnafu { index }.build().into());
                    };
                    match (block, update.delta) {
                        (AssemblyBlock::Text(assembled), types::Delta::Text(text)) => {
                            assembled.push_str(&text);
                            event_tx
                                .send_async(ProviderEvent::TextDelta { text })
                                .await
                                .context(error::EventReceiverSnafu)?;
                        }
                        (
                            AssemblyBlock::Thinking {
                                text: assembled, ..
                            },
                            types::Delta::ReasoningContent(types::ReasoningDelta::Text(text)),
                        ) => {
                            assembled.push_str(&text);
                            event_tx
                                .send_async(ProviderEvent::ThinkingDelta { text })
                                .await
                                .context(error::EventReceiverSnafu)?;
                        }
                        (
                            AssemblyBlock::Thinking { signature, .. },
                            types::Delta::ReasoningContent(types::ReasoningDelta::Signature(part)),
                        ) => signature.push_str(&part),
                        (
                            AssemblyBlock::Redacted(data),
                            types::Delta::ReasoningContent(types::ReasoningDelta::RedactedContent(
                                part,
                            )),
                        ) => data.extend(STANDARD.decode(part).context(error::Base64Snafu)?),
                        (AssemblyBlock::Tool { id, json, .. }, types::Delta::ToolUse { input }) => {
                            json.push_str(&input);
                            event_tx
                                .send_async(ProviderEvent::ToolInputDelta {
                                    id: id.clone(),
                                    partial_json: input,
                                })
                                .await
                                .context(error::EventReceiverSnafu)?;
                        }
                        _ => return Err(error::DeltaMismatchSnafu { index }.build().into()),
                    }
                }
                "contentBlockStop" => {
                    ensure!(
                        started && stop_reason.is_none(),
                        error::UnexpectedEventSnafu { event_type }
                    );
                    let stop: types::BlockStop = serde_json::from_slice(&frame.payload)
                        .context(error::DecodeEventSnafu { event_type })?;
                    let index = stop.content_block_index;
                    let expected = blocks.len();
                    let block = blocks
                        .get_mut(index)
                        .context(error::BlockIndexSnafu { index, expected })?;
                    let Block::Open(content) = block else {
                        return Err(error::ClosedBlockSnafu { index }.build().into());
                    };
                    let closed = match content {
                        AssemblyBlock::Text(text) => ContentBlock::Text {
                            text: std::mem::take(text),
                        },
                        AssemblyBlock::Thinking { text, signature } => {
                            ensure!(
                                !signature.is_empty(),
                                error::MissingSignatureSnafu { index }
                            );
                            ContentBlock::Thinking {
                                thinking: std::mem::take(text),
                                signature: Some(std::mem::take(signature)),
                            }
                        }
                        AssemblyBlock::Redacted(data) => ContentBlock::RedactedThinking {
                            data: STANDARD.encode(data),
                        },
                        AssemblyBlock::Tool { id, name, json } => {
                            let input: Value = serde_json::from_str(json)
                                .context(error::DecodeToolInputSnafu { id: &*id })?;
                            ensure!(input.is_object(), error::InvalidToolInputSnafu { id: &*id });
                            ContentBlock::ToolUse {
                                id: std::mem::take(id),
                                name: std::mem::take(name),
                                input,
                                thought_signature: None,
                            }
                        }
                    };
                    *block = Block::Closed(closed);
                    while let Some(Block::Closed(content)) = blocks.get(completed) {
                        event_tx
                            .send_async(ProviderEvent::ContentComplete {
                                block: content.clone(),
                            })
                            .await
                            .context(error::EventReceiverSnafu)?;
                        completed += 1;
                    }
                }
                "messageStop" => {
                    ensure!(
                        started && stop_reason.is_none(),
                        error::UnexpectedEventSnafu { event_type }
                    );
                    ensure!(completed == blocks.len(), error::UnclosedBlocksSnafu);
                    let stop: types::MessageStop = serde_json::from_slice(&frame.payload)
                        .context(error::DecodeEventSnafu { event_type })?;
                    stop_reason = Some(match stop.stop_reason.as_str() {
                        "end_turn" | "stop_sequence" => StopReason::EndTurn,
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "guardrail_intervened" | "content_filtered" => StopReason::Refusal,
                        "model_context_window_exceeded" => StopReason::ContextWindowExceeded,
                        _ => {
                            return Err(error::StopReasonSnafu {
                                reason: stop.stop_reason,
                            }
                            .build()
                            .into());
                        }
                    });
                }
                "metadata" => {
                    ensure!(
                        stop_reason.is_some() && usage.is_none(),
                        error::UnexpectedEventSnafu { event_type }
                    );
                    let metadata: types::Metadata = serde_json::from_slice(&frame.payload)
                        .context(error::DecodeEventSnafu { event_type })?;
                    let update = TokenUsage::builder()
                        .input(metadata.usage.input_tokens)
                        .output(metadata.usage.output_tokens)
                        .cache_creation(metadata.usage.cache_write_input_tokens)
                        .cache_read(metadata.usage.cache_read_input_tokens)
                        .build();
                    event_tx
                        .send_async(ProviderEvent::Usage {
                            usage: update.clone(),
                        })
                        .await
                        .context(error::EventReceiverSnafu)?;
                    usage = Some(update);
                }
                _ => return Err(error::UnexpectedEventSnafu { event_type }.build().into()),
            }
        }
        let stop = stop_reason.context(error::TruncatedStreamSnafu)?;
        let usage = usage.context(error::TruncatedStreamSnafu)?;
        let content = blocks
            .into_iter()
            .map(|block| match block {
                Block::Closed(content) => content,
                Block::Open(_) => unreachable!("messageStop requires every block to be closed"),
            })
            .collect::<Vec<_>>();
        Ok(StreamResponse::builder()
            .message(
                Message::builder()
                    .role(Role::Assistant)
                    .content(content)
                    .kind(MessageKind::Turn)
                    .build(),
            )
            .usage(usage)
            .stop_reason(stop)
            .input_transformations(Vec::new())
            .build())
    }
}

impl TryFrom<&ImageSource> for types::Image {
    type Error = crate::protocol::ProtocolError;

    fn try_from(source: &ImageSource) -> Result<Self> {
        let ImageSource::Base64 { media_type, data } = source else {
            return Err(error::RemoteImageSnafu.build().into());
        };
        let format = match media_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpeg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => return Err(error::ImageMediaTypeSnafu { media_type }.build().into()),
        };
        STANDARD.decode(data).context(error::Base64Snafu)?;
        Ok(Self::builder()
            .format(format)
            .source(types::ImageBytes::builder().bytes(data).build())
            .build())
    }
}
