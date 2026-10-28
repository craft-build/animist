use std::collections::BTreeMap;

use snafu::Snafu;

use crate::{
    history::{ContentBlock, Message, Role},
    provider::Tool,
};

/// Zero-based position in the exact request view being validated.
#[derive(crate::Struct!, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReplayLocation {
    pub message: usize,
    pub block: usize,
}

#[derive(crate::Enum!, Snafu)]
pub enum ReplayError {
    #[snafu(display("Assistant-only content in user message at {location:?}"))]
    AssistantContentInUser { location: ReplayLocation },
    #[snafu(display("Tool result in assistant message at {location:?}"))]
    ResultInAssistant { location: ReplayLocation },
    #[snafu(display("Empty tool call ID at {location:?}"))]
    EmptyCallId { location: ReplayLocation },
    #[snafu(display("Duplicate call ID {id} at {location:?}; first at {first:?}"))]
    DuplicateCallId {
        location: ReplayLocation,
        first: ReplayLocation,
        id: String,
    },
    #[snafu(display("Empty tool name at {location:?}"))]
    EmptyCallName { location: ReplayLocation },
    #[snafu(display("Empty tool result ID at {location:?}"))]
    EmptyResultId { location: ReplayLocation },
    #[snafu(display("Result for {id} at {location:?} has no preceding call"))]
    OrphanResult {
        location: ReplayLocation,
        id: String,
    },
    #[snafu(display("Duplicate result for {id} at {location:?}; first at {first:?}"))]
    DuplicateResult {
        location: ReplayLocation,
        first: ReplayLocation,
        id: String,
    },
    #[snafu(display("Call {id} at {location:?} has no result"))]
    MissingResult {
        location: ReplayLocation,
        id: String,
    },
    #[snafu(display("Empty tool declaration name at index {index}"))]
    EmptyDeclaration { index: usize },
    #[snafu(display("Duplicate tool declaration {name} at {index}; first at {first_index}"))]
    DuplicateDeclaration {
        index: usize,
        first_index: usize,
        name: String,
    },
    #[snafu(display("Empty loaded tool name at {location:?}, index {index}"))]
    EmptyLoadedTool {
        location: ReplayLocation,
        index: usize,
    },
    #[snafu(display(
        "Duplicate loaded tool {name} at {location:?}, index {index}; first at {first_index}"
    ))]
    DuplicateLoadedTool {
        location: ReplayLocation,
        index: usize,
        first_index: usize,
        name: String,
    },
}

/// Validates a complete inference view without rewriting it or interpreting opaque data.
/// Historical calls need not refer to currently advertised tools.
/// Protocol adapters retain wire-specific content and sequencing restrictions.
pub fn validate_replay(messages: &[Message], tools: &[Tool]) -> Result<(), ReplayError> {
    let mut declarations = BTreeMap::new();
    for (index, tool) in tools.iter().enumerate() {
        if tool.name.is_empty() {
            return Err(ReplayError::EmptyDeclaration { index });
        }
        if let Some(first_index) = declarations.insert(&tool.name, index) {
            return Err(ReplayError::DuplicateDeclaration {
                index,
                first_index,
                name: tool.name.clone(),
            });
        }
    }
    let mut calls: BTreeMap<&str, (ReplayLocation, Option<ReplayLocation>)> = BTreeMap::new();
    for (message_index, message) in messages.iter().enumerate() {
        for (block_index, block) in message.content.iter().enumerate() {
            let location = ReplayLocation {
                message: message_index,
                block: block_index,
            };
            match block {
                ContentBlock::Thinking { .. }
                | ContentBlock::RedactedThinking { .. }
                | ContentBlock::ToolUse { .. }
                    if message.role == Role::User =>
                {
                    return Err(ReplayError::AssistantContentInUser { location });
                }
                ContentBlock::ToolResult { .. } if message.role == Role::Assistant => {
                    return Err(ReplayError::ResultInAssistant { location });
                }
                ContentBlock::ToolUse { id, name, .. } => {
                    if id.is_empty() {
                        return Err(ReplayError::EmptyCallId { location });
                    }
                    if name.is_empty() {
                        return Err(ReplayError::EmptyCallName { location });
                    }
                    if let Some((first, _)) = calls.insert(id, (location, None)) {
                        return Err(ReplayError::DuplicateCallId {
                            location,
                            first,
                            id: id.clone(),
                        });
                    }
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    loaded_tools,
                    ..
                } => {
                    if tool_use_id.is_empty() {
                        return Err(ReplayError::EmptyResultId { location });
                    }
                    let Some((_, result)) = calls.get_mut(tool_use_id.as_str()) else {
                        return Err(ReplayError::OrphanResult {
                            location,
                            id: tool_use_id.clone(),
                        });
                    };
                    if let Some(first) = result.replace(location) {
                        return Err(ReplayError::DuplicateResult {
                            location,
                            first,
                            id: tool_use_id.clone(),
                        });
                    }
                    let mut loaded = BTreeMap::new();
                    for (index, name) in loaded_tools.iter().enumerate() {
                        if name.is_empty() {
                            return Err(ReplayError::EmptyLoadedTool { location, index });
                        }
                        if let Some(first_index) = loaded.insert(name, index) {
                            return Err(ReplayError::DuplicateLoadedTool {
                                location,
                                index,
                                first_index,
                                name: name.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if let Some((id, (location, _))) = calls
        .iter()
        .filter(|(_, (_, result))| result.is_none())
        .min_by_key(|(_, (location, _))| *location)
    {
        return Err(ReplayError::MissingResult {
            location: *location,
            id: (*id).to_owned(),
        });
    }
    Ok(())
}
