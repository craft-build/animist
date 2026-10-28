//! Anthropic Messages translation and request-owned stream assembly.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};

use super::{AnthropicOptions, error::*, types::*};
use crate::{
    history::{ContentBlock, ImageSource, Message, MessageKind, Role, ToolContent},
    protocol::{Protocol, ProtocolInput, Result, sse::Events},
    provider::{ProviderEvent, StopReason, StreamResponse, TokenUsage},
};

#[derive(crate::Struct!)]
pub struct Messages;

#[derive(Debug, Clone, PartialEq, Builder)]
#[builder(on(_, into))]
struct BlockState {
    content: ContentBlock,
    open: bool,
    partial_json: Option<String>,
}

#[async_trait]
impl Protocol for Messages {
    type Request = Value;
    type Response = reqwest::Response;
    type Options = AnthropicOptions;

    fn request(&self, input: ProtocolInput<'_>, options: &Self::Options) -> Result<Self::Request> {
        let mut messages = Vec::new();
        for message in input.messages {
            let mut content = Vec::new();
            for block in &message.content {
                content.push(match block {
                    ContentBlock::Text { text } => InputBlock::Text { text: text.clone() },
                    ContentBlock::Image { source } if message.role == Role::User => {
                        if let ImageSource::Base64 { media_type, .. } = source {
                            ensure!(
                                matches!(
                                    media_type.as_str(),
                                    "image/jpeg" | "image/png" | "image/gif" | "image/webp"
                                ),
                                UnsupportedImageMediaTypeSnafu { media_type }
                            );
                        }
                        InputBlock::Image {
                            source: source.clone(),
                        }
                    }
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } if message.role == Role::Assistant => {
                        let signature = signature
                            .as_ref()
                            .filter(|signature| !signature.is_empty())
                            .context(MissingReplaySignatureSnafu)?;
                        InputBlock::Thinking {
                            thinking: thinking.clone(),
                            signature: signature.clone(),
                        }
                    }
                    ContentBlock::RedactedThinking { data } if message.role == Role::Assistant => {
                        InputBlock::RedactedThinking { data: data.clone() }
                    }
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        thought_signature: None,
                    } if message.role == Role::Assistant => {
                        ensure!(input.is_object(), InvalidToolInputSnafu { id });
                        InputBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        }
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        loaded_tools,
                    } if message.role == Role::User => {
                        ensure!(loaded_tools.is_empty(), UnsupportedLoadedToolsSnafu);
                        for part in &content.0 {
                            if let ToolContent::Image {
                                source: ImageSource::Base64 { media_type, .. },
                            } = part
                            {
                                ensure!(
                                    matches!(
                                        media_type.as_str(),
                                        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
                                    ),
                                    UnsupportedImageMediaTypeSnafu { media_type }
                                );
                            }
                        }
                        InputBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: content.clone(),
                            is_error: *is_error,
                        }
                    }
                    _ => {
                        return Err(AnthropicProtocolError::UnsupportedContent {
                            role: message.role.clone(),
                        }
                        .into());
                    }
                });
            }
            messages.push(
                InputMessage::builder()
                    .role(message.role.clone())
                    .content(content)
                    .build(),
            );
        }
        let tools = input
            .tools
            .iter()
            .map(|tool| {
                ToolDefinition::builder()
                    .name(&tool.name)
                    .description(&tool.description)
                    .input_schema(tool.input_schema.clone())
                    .build()
            })
            .collect::<Vec<_>>();
        let request = Request::builder()
            .model(&input.model.id)
            .messages(messages)
            .max_tokens(options.max_tokens)
            .stream(true)
            .maybe_system((!input.system.is_empty()).then_some(input.system))
            .tools(tools)
            .maybe_thinking(options.thinking.clone())
            .maybe_output_config(
                options
                    .effort
                    .clone()
                    .map(|effort| OutputConfig::builder().effort(effort).build()),
            )
            .maybe_speed(options.fast.then_some(Speed::Fast))
            .build();
        Ok(serde_json::to_value(request).context(EncodeRequestSnafu)?)
    }

    async fn assemble(
        &self,
        response: Self::Response,
        event_tx: &Sender<ProviderEvent>,
    ) -> Result<StreamResponse> {
        let mut events = Events::new(response);
        let mut blocks: Vec<BlockState> = Vec::new();
        let mut started = false;
        let mut message_delta = false;
        let mut stop_reason = None;
        let mut usage = TokenUsage::default();
        while let Some(data) = events.next().await? {
            let event: Event = serde_json::from_str(&data).context(DecodeEventSnafu)?;
            match event {
                Event::MessageStart { message } => {
                    ensure!(
                        !started,
                        UnexpectedEventSnafu {
                            event: "message_start"
                        }
                    );
                    ensure!(
                        message.role == Role::Assistant && message.content.is_empty(),
                        InvalidMessageStartSnafu
                    );
                    started = true;
                    usage = TokenUsage::builder()
                        .input(message.usage.input_tokens)
                        .output(message.usage.output_tokens)
                        .cache_creation(
                            message
                                .usage
                                .cache_creation_input_tokens
                                .unwrap_or_default(),
                        )
                        .cache_read(message.usage.cache_read_input_tokens.unwrap_or_default())
                        .build();
                }
                Event::ContentBlockStart {
                    index,
                    content_block,
                } => {
                    ensure!(
                        started && !message_delta,
                        UnexpectedEventSnafu {
                            event: "content_block_start"
                        }
                    );
                    ensure!(
                        index as usize == blocks.len(),
                        UnexpectedBlockIndexSnafu {
                            index,
                            expected: blocks.len()
                        }
                    );
                    let content = match content_block {
                        StartBlock::Text { text } => {
                            if !text.is_empty() {
                                event_tx
                                    .send_async(ProviderEvent::TextDelta { text: text.clone() })
                                    .await
                                    .context(EventReceiverSnafu)?;
                            }
                            ContentBlock::Text { text }
                        }
                        StartBlock::Thinking {
                            thinking,
                            signature,
                        } => {
                            if !thinking.is_empty() {
                                event_tx
                                    .send_async(ProviderEvent::ThinkingDelta {
                                        text: thinking.clone(),
                                    })
                                    .await
                                    .context(EventReceiverSnafu)?;
                            }
                            ContentBlock::Thinking {
                                thinking,
                                signature: Some(signature),
                            }
                        }
                        StartBlock::RedactedThinking { data } => {
                            ContentBlock::RedactedThinking { data }
                        }
                        StartBlock::ToolUse { id, name, input } => {
                            ensure!(input.is_object(), InvalidToolInputSnafu { id: &id });
                            event_tx
                                .send_async(ProviderEvent::ToolUseStart {
                                    id: id.clone(),
                                    name: name.clone(),
                                })
                                .await
                                .context(EventReceiverSnafu)?;
                            if input.as_object().is_some_and(|input| !input.is_empty()) {
                                event_tx
                                    .send_async(ProviderEvent::ToolInputDelta {
                                        id: id.clone(),
                                        partial_json: input.to_string(),
                                    })
                                    .await
                                    .context(EventReceiverSnafu)?;
                            }
                            ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                thought_signature: None,
                            }
                        }
                    };
                    blocks.push(BlockState::builder().content(content).open(true).build());
                }
                Event::ContentBlockDelta { index, delta } => {
                    ensure!(
                        started && !message_delta,
                        UnexpectedEventSnafu {
                            event: "content_block_delta"
                        }
                    );
                    let block = blocks
                        .get_mut(index as usize)
                        .context(UnknownBlockSnafu { index })?;
                    ensure!(block.open, BlockAlreadyStoppedSnafu { index });
                    match (delta, &mut block.content) {
                        (BlockDelta::Text { text }, ContentBlock::Text { text: assembled }) => {
                            assembled.push_str(&text);
                            event_tx
                                .send_async(ProviderEvent::TextDelta { text })
                                .await
                                .context(EventReceiverSnafu)?;
                        }
                        (
                            BlockDelta::Thinking { thinking },
                            ContentBlock::Thinking {
                                thinking: assembled,
                                ..
                            },
                        ) => {
                            assembled.push_str(&thinking);
                            event_tx
                                .send_async(ProviderEvent::ThinkingDelta { text: thinking })
                                .await
                                .context(EventReceiverSnafu)?;
                        }
                        (
                            BlockDelta::Signature { signature },
                            ContentBlock::Thinking {
                                signature: assembled,
                                ..
                            },
                        ) => {
                            assembled.get_or_insert_default().push_str(&signature);
                        }
                        (
                            BlockDelta::InputJson { partial_json },
                            ContentBlock::ToolUse { id, input, .. },
                        ) => {
                            if !partial_json.is_empty() {
                                ensure!(
                                    input.as_object().is_some_and(|input| input.is_empty()),
                                    ConflictingToolInputSnafu { id: &*id }
                                );
                                block
                                    .partial_json
                                    .get_or_insert_default()
                                    .push_str(&partial_json);
                            }
                            event_tx
                                .send_async(ProviderEvent::ToolInputDelta {
                                    id: id.clone(),
                                    partial_json,
                                })
                                .await
                                .context(EventReceiverSnafu)?;
                        }
                        _ => return Err(AnthropicProtocolError::DeltaTypeMismatch { index }.into()),
                    }
                }
                Event::ContentBlockStop { index } => {
                    ensure!(
                        started && !message_delta,
                        UnexpectedEventSnafu {
                            event: "content_block_stop"
                        }
                    );
                    let block = blocks
                        .get_mut(index as usize)
                        .context(UnknownBlockSnafu { index })?;
                    ensure!(block.open, BlockAlreadyStoppedSnafu { index });
                    if let ContentBlock::ToolUse { id, input, .. } = &mut block.content
                        && let Some(arguments) = block.partial_json.take()
                    {
                        *input = serde_json::from_str(&arguments)
                            .context(DecodeToolArgumentsSnafu { id: &*id })?;
                        ensure!(input.is_object(), InvalidToolInputSnafu { id: &*id });
                    }
                    if let ContentBlock::Thinking { signature, .. } = &block.content {
                        ensure!(
                            signature
                                .as_ref()
                                .is_some_and(|signature| !signature.is_empty()),
                            MissingThinkingSignatureSnafu { index }
                        );
                    }
                    block.open = false;
                }
                Event::MessageDelta {
                    delta,
                    usage: update,
                } => {
                    ensure!(
                        started,
                        UnexpectedEventSnafu {
                            event: "message_delta"
                        }
                    );
                    ensure!(blocks.iter().all(|block| !block.open), UnclosedBlocksSnafu);
                    message_delta = true;
                    if let Some(reason) = delta.stop_reason {
                        stop_reason = Some(reason);
                    }
                    usage.output = update.output_tokens;
                    if let Some(tokens) = update.input_tokens {
                        usage.input = tokens;
                    }
                    if let Some(tokens) = update.cache_creation_input_tokens {
                        usage.cache_creation = tokens;
                    }
                    if let Some(tokens) = update.cache_read_input_tokens {
                        usage.cache_read = tokens;
                    }
                }
                Event::MessageStop => {
                    ensure!(
                        started,
                        UnexpectedEventSnafu {
                            event: "message_stop"
                        }
                    );
                    ensure!(blocks.iter().all(|block| !block.open), UnclosedBlocksSnafu);
                    let reason: String = stop_reason.context(MissingStopReasonSnafu)?;
                    let stop = match reason.as_str() {
                        "end_turn" | "stop_sequence" => StopReason::EndTurn,
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "pause_turn" => StopReason::PauseTurn,
                        "refusal" => StopReason::Refusal,
                        "model_context_window_exceeded" => StopReason::ContextWindowExceeded,
                        _ => {
                            return Err(
                                AnthropicProtocolError::UnsupportedStopReason { reason }.into()
                            );
                        }
                    };
                    return Ok(StreamResponse::builder()
                        .message(
                            Message::builder()
                                .role(Role::Assistant)
                                .content(
                                    blocks
                                        .into_iter()
                                        .map(|block| block.content)
                                        .collect::<Vec<_>>(),
                                )
                                .kind(MessageKind::Turn)
                                .build(),
                        )
                        .usage(usage)
                        .stop_reason(stop)
                        .input_transformations(Vec::new())
                        .build());
                }
                Event::Error { error } => return Err(AnthropicProtocolError::Api { error }.into()),
                Event::Ping | Event::Unknown => {}
            }
        }
        Err(AnthropicProtocolError::TruncatedStream.into())
    }
}
