//! `OpenAI` Chat Completions request translation and SSE assembly.

use std::collections::BTreeMap;

use async_trait::async_trait;
use flume::Sender;
use serde_json::Value;
use snafu::{OptionExt, ResultExt, ensure};

use super::error::{
    ApiSnafu, ChatReasoningReplayDisabledSnafu, ChoiceAfterFinishSnafu, DecodeChunkSnafu,
    DecodeEventSnafu, DecodeToolArgumentsSnafu, EncodeRequestSnafu, EventReceiverSnafu,
    IncompleteToolIdentitySnafu, MissingFinishReasonSnafu, MultipleChoicesSnafu, RefusalSnafu,
    ToolArgumentsBeforeIdentitySnafu, TruncatedChatStreamSnafu, UnexpectedFinishReasonSnafu,
    UnsupportedChatContentSnafu, UnsupportedChatToolImageSnafu, UnsupportedLoadedToolsSnafu,
};
use super::types::{
    ChatAssistantContent, ChatCall, ChatChunk, ChatContent, ChatMessage, ChatRequest, ChatTool,
    FunctionCall, FunctionDefinition, ImageUrl, ServiceTier, StreamOptions,
};
use super::{ChatReasoningField, ChatTokenLimit, OpenAiOptions};
use crate::{
    history::{ContentBlock, Message, MessageKind, Role, ToolContent},
    protocol::{Protocol, ProtocolInput, Result, sse::Events},
    provider::{ProviderEvent, StopReason, StreamResponse, TokenUsage},
};

/// `OpenAI` Chat Completions protocol adapter.
#[derive(crate::Struct!)]
pub struct ChatCompletions;

#[derive(crate::Struct!)]
struct Call {
    id: String,
    name: String,
    arguments: String,
    started: bool,
}

#[async_trait]
impl Protocol for ChatCompletions {
    type Request = Value;
    type Response = reqwest::Response;
    type Options = OpenAiOptions;

    fn request(&self, input: ProtocolInput<'_>, options: &Self::Options) -> Result<Self::Request> {
        crate::protocol::validate_replay(input.messages, input.tools)?;
        crate::protocol::sampling::validate(input.options, "chat_completions", 2.0, 0.0, 4)?;
        let mut messages = Vec::new();
        if !input.system.is_empty() {
            messages.push(ChatMessage::System {
                content: input.system.into(),
            });
        }
        for message in input.messages {
            for segment in message
                .content
                .split_inclusive(|block| matches!(block, ContentBlock::ToolResult { .. }))
            {
                let mut content = Vec::new();
                let mut calls = Vec::new();
                let mut reasoning = String::new();
                let mut assistant_text = String::new();
                let mut has_assistant_text = false;
                for block in segment {
                    match block {
                        ContentBlock::Text { text } => {
                            if message.role == Role::Assistant
                                && matches!(
                                    options.chat_reasoning_field,
                                    ChatReasoningField::Reasoning
                                )
                            {
                                assistant_text.push_str(text);
                                has_assistant_text = true;
                            } else {
                                content.push(ChatContent::Text { text: text.clone() });
                            }
                        }
                        ContentBlock::Image { source } if message.role == Role::User => content
                            .push(ChatContent::ImageUrl {
                                image_url: ImageUrl::builder().url(source.to_url()).build(),
                            }),
                        ContentBlock::ToolUse {
                            id,
                            name,
                            input,
                            thought_signature: None,
                        } if message.role == Role::Assistant => calls.push(ChatCall::Function {
                            id: id.clone(),
                            function: FunctionCall::builder()
                                .name(name)
                                .arguments(input.to_string())
                                .build(),
                        }),
                        ContentBlock::ToolResult { .. } if message.role == Role::User => {}
                        ContentBlock::Thinking {
                            thinking,
                            signature: None,
                        } if message.role == Role::Assistant => {
                            ensure!(
                                options.replay_reasoning_content,
                                ChatReasoningReplayDisabledSnafu
                            );
                            reasoning.push_str(thinking);
                        }
                        _ => return Err(UnsupportedChatContentSnafu.build().into()),
                    }
                }
                if !content.is_empty()
                    || !calls.is_empty()
                    || !reasoning.is_empty()
                    || has_assistant_text
                {
                    messages.push(match message.role {
                        Role::User => ChatMessage::User { content },
                        Role::Assistant => match options.chat_reasoning_field {
                            ChatReasoningField::ReasoningContent => ChatMessage::Assistant {
                                content: (!content.is_empty())
                                    .then_some(ChatAssistantContent::Parts(content)),
                                tool_calls: calls,
                                reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
                                reasoning: None,
                            },
                            ChatReasoningField::Reasoning => ChatMessage::Assistant {
                                content: Some(ChatAssistantContent::Text(assistant_text)),
                                tool_calls: calls,
                                reasoning_content: None,
                                reasoning: (!reasoning.is_empty()).then_some(reasoning),
                            },
                        },
                    });
                }
                if let Some(ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    loaded_tools,
                    ..
                }) = segment.last()
                {
                    ensure!(
                        loaded_tools.is_empty(),
                        UnsupportedLoadedToolsSnafu { id: tool_use_id }
                    );
                    let mut parts = Vec::new();
                    for part in &content.0 {
                        match part {
                            ToolContent::Text { text } => {
                                parts.push(ChatContent::Text { text: text.clone() });
                            }
                            ToolContent::Image { .. } => {
                                return Err(UnsupportedChatToolImageSnafu {
                                    id: tool_use_id.clone(),
                                }
                                .build()
                                .into());
                            }
                        }
                    }
                    let content = match parts.as_slice() {
                        [ChatContent::Text { text }] => ChatAssistantContent::Text(text.clone()),
                        _ => ChatAssistantContent::Parts(parts),
                    };
                    messages.push(ChatMessage::Tool {
                        tool_call_id: tool_use_id.clone(),
                        content,
                    });
                }
            }
        }
        let tools = input
            .tools
            .iter()
            .map(|tool| ChatTool::Function {
                function: FunctionDefinition::builder()
                    .name(&tool.name)
                    .description(&tool.description)
                    .parameters(tool.input_schema.clone())
                    .build(),
            })
            .collect::<Vec<_>>();
        let request = ChatRequest::builder()
            .model(&input.model.id)
            .messages(messages)
            .stream(true)
            .stream_options(StreamOptions::builder().include_usage(true).build())
            .maybe_temperature(input.options.temperature)
            .maybe_top_p(input.options.top_p)
            .maybe_frequency_penalty(input.options.frequency_penalty)
            .maybe_presence_penalty(input.options.presence_penalty)
            .maybe_stop(input.options.stop_sequences.clone())
            .tools(tools)
            .maybe_max_completion_tokens(
                input
                    .options
                    .max_output_tokens
                    .or(input.model.info.config.max_output_tokens)
                    .filter(|_| options.chat_token_limit == ChatTokenLimit::MaxCompletionTokens),
            )
            .maybe_max_tokens(
                input
                    .options
                    .max_output_tokens
                    .or(input.model.info.config.max_output_tokens)
                    .filter(|_| options.chat_token_limit == ChatTokenLimit::MaxTokens),
            )
            .maybe_thinking(options.chat_thinking.clone())
            .maybe_tool_stream(options.tool_stream)
            .maybe_reasoning_effort(options.reasoning_effort.clone())
            .maybe_reasoning_budget_tokens(options.chat_reasoning_budget)
            .maybe_enable_thinking(options.chat_enable_thinking)
            .maybe_thinking_budget(options.chat_thinking_budget)
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
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls: BTreeMap<u64, Call> = BTreeMap::new();
        let mut blocks = Vec::new();
        let mut usage = TokenUsage::default();
        let mut stop = None;
        let mut done = false;
        while let Some(data) = events.next().await? {
            if data == "[DONE]" {
                done = true;
                break;
            }
            let value: Value = serde_json::from_str(&data).context(DecodeEventSnafu)?;
            if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                return Err(ApiSnafu {
                    error: error.clone(),
                }
                .build()
                .into());
            }
            let chunk: ChatChunk = serde_json::from_value(value).context(DecodeChunkSnafu)?;
            if let Some(tokens) = chunk.usage {
                usage = tokens.into();
                event_tx
                    .send_async(ProviderEvent::Usage {
                        usage: usage.clone(),
                    })
                    .await
                    .context(EventReceiverSnafu)?;
            }
            for choice in chunk.choices {
                ensure!(stop.is_none(), ChoiceAfterFinishSnafu);
                ensure!(
                    choice.index == 0,
                    MultipleChoicesSnafu {
                        index: choice.index
                    }
                );
                let delta = choice.delta;
                if let Some(piece) = delta.reasoning_content.filter(|piece| !piece.is_empty()) {
                    reasoning.push_str(&piece);
                    event_tx
                        .send_async(ProviderEvent::ThinkingDelta { text: piece })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                if let Some(piece) = delta.content.filter(|piece| !piece.is_empty()) {
                    content.push_str(&piece);
                    event_tx
                        .send_async(ProviderEvent::TextDelta { text: piece })
                        .await
                        .context(EventReceiverSnafu)?;
                }
                ensure!(delta.refusal.is_none(), RefusalSnafu);
                if let Some(deltas) = delta.tool_calls {
                    for delta in deltas {
                        let call = calls.entry(delta.index).or_default();
                        if let Some(id) = delta.id {
                            call.id.push_str(&id);
                        }
                        if let Some(name) = delta.function.name {
                            call.name.push_str(&name);
                        }
                        if !call.started
                            && !call.id.is_empty()
                            && !call.name.is_empty()
                            && delta
                                .function
                                .arguments
                                .as_ref()
                                .is_some_and(|piece| !piece.is_empty())
                        {
                            event_tx
                                .send_async(ProviderEvent::ToolUseStart {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                })
                                .await
                                .context(EventReceiverSnafu)?;
                            call.started = true;
                        }
                        if let Some(piece) =
                            delta.function.arguments.filter(|piece| !piece.is_empty())
                        {
                            ensure!(
                                call.started,
                                ToolArgumentsBeforeIdentitySnafu { index: delta.index }
                            );
                            call.arguments.push_str(&piece);
                            event_tx
                                .send_async(ProviderEvent::ToolInputDelta {
                                    id: call.id.clone(),
                                    partial_json: piece,
                                })
                                .await
                                .context(EventReceiverSnafu)?;
                        }
                    }
                }
                if let Some(reason) = choice.finish_reason {
                    for call in calls.values_mut() {
                        if !call.started && !call.id.is_empty() && !call.name.is_empty() {
                            event_tx
                                .send_async(ProviderEvent::ToolUseStart {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                })
                                .await
                                .context(EventReceiverSnafu)?;
                            call.started = true;
                        }
                    }
                    stop = Some(match reason.as_str() {
                        "stop" => StopReason::EndTurn,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        _ => {
                            return Err(UnexpectedFinishReasonSnafu { reason }.build().into());
                        }
                    });
                    if !reasoning.is_empty() {
                        blocks.push(ContentBlock::Thinking {
                            thinking: std::mem::take(&mut reasoning),
                            signature: None,
                        });
                    }
                    if !content.is_empty() {
                        blocks.push(ContentBlock::Text {
                            text: std::mem::take(&mut content),
                        });
                    }
                    for block in &blocks {
                        event_tx
                            .send_async(ProviderEvent::ContentComplete {
                                block: block.clone(),
                            })
                            .await
                            .context(EventReceiverSnafu)?;
                    }
                    for (index, call) in std::mem::take(&mut calls) {
                        ensure!(call.started, IncompleteToolIdentitySnafu { index });
                        let input = serde_json::from_str(&call.arguments)
                            .context(DecodeToolArgumentsSnafu { id: &call.id })?;
                        let block = ContentBlock::ToolUse {
                            id: call.id,
                            name: call.name,
                            input,
                            thought_signature: None,
                        };
                        event_tx
                            .send_async(ProviderEvent::ContentComplete {
                                block: block.clone(),
                            })
                            .await
                            .context(EventReceiverSnafu)?;
                        blocks.push(block);
                    }
                }
            }
        }
        ensure!(
            done,
            TruncatedChatStreamSnafu {
                finish_reason: stop.clone()
            }
        );
        let stop = stop.context(MissingFinishReasonSnafu)?;
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
