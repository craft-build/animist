//! Structural replay validation shared by every conversation adapter.

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

impl std::fmt::Display for ReplayLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "message {}, block {}", self.message, self.block)
    }
}

/// Replay validation failures for message, call, result, and tool shapes.
#[derive(Debug, Snafu)]
pub enum ReplayError {
    #[snafu(display("Non-text content in system message at {location}"))]
    NonTextSystemContent {
        location: ReplayLocation,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Assistant-only content in user message at {location}"))]
    AssistantContentInUser {
        location: ReplayLocation,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Tool result in assistant message at {location}"))]
    ResultInAssistant {
        location: ReplayLocation,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Empty tool call ID at {location}"))]
    EmptyCallId {
        location: ReplayLocation,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Duplicate call ID {id} at {location}; first at {first}"))]
    DuplicateCallId {
        location: ReplayLocation,
        first: ReplayLocation,
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Empty tool name at {location}"))]
    EmptyCallName {
        location: ReplayLocation,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Empty tool result ID at {location}"))]
    EmptyResultId {
        location: ReplayLocation,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Result for {id} at {location} has no preceding call"))]
    OrphanResult {
        location: ReplayLocation,
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Duplicate result for {id} at {location}; first at {first}"))]
    DuplicateResult {
        location: ReplayLocation,
        first: ReplayLocation,
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Call {id} at {location} has no result"))]
    MissingResult {
        location: ReplayLocation,
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Empty tool declaration name at index {index}"))]
    EmptyDeclaration {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Duplicate tool declaration {name} at {index}; first at {first_index}"))]
    DuplicateDeclaration {
        index: usize,
        first_index: usize,
        name: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Empty loaded tool name at {location}, index {index}"))]
    EmptyLoadedTool {
        location: ReplayLocation,
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display(
        "Duplicate loaded tool {name} at {location}, index {index}; first at {first_index}"
    ))]
    DuplicateLoadedTool {
        location: ReplayLocation,
        index: usize,
        first_index: usize,
        name: String,
        backtrace: snafu::Backtrace,
    },
}

/// Validates a complete inference view without rewriting it or interpreting opaque data.
/// Historical calls need not refer to currently advertised tools.
/// Protocol adapters retain wire-specific content and sequencing restrictions.
pub fn validate_replay(messages: &[Message], tools: &[Tool]) -> Result<(), ReplayError> {
    let mut declarations = BTreeMap::new();
    for (index, tool) in tools.iter().enumerate() {
        if tool.name.is_empty() {
            return Err(EmptyDeclarationSnafu { index }.build());
        }
        if let Some(first_index) = declarations.insert(&tool.name, index) {
            return Err(DuplicateDeclarationSnafu {
                index,
                first_index,
                name: tool.name.clone(),
            }
            .build());
        }
    }
    let mut calls: BTreeMap<&str, (ReplayLocation, Option<ReplayLocation>)> = BTreeMap::new();
    for (message_index, message) in messages.iter().enumerate() {
        for (block_index, block) in message.content.iter().enumerate() {
            let location = ReplayLocation {
                message: message_index,
                block: block_index,
            };
            if message.role == Role::System && !matches!(block, ContentBlock::Text { .. }) {
                return Err(NonTextSystemContentSnafu { location }.build());
            }
            match block {
                ContentBlock::Thinking { .. }
                | ContentBlock::RedactedThinking { .. }
                | ContentBlock::ToolUse { .. }
                    if message.role == Role::User =>
                {
                    return Err(AssistantContentInUserSnafu { location }.build());
                }
                ContentBlock::ToolResult { .. } if message.role == Role::Assistant => {
                    return Err(ResultInAssistantSnafu { location }.build());
                }
                ContentBlock::ToolUse { id, name, .. } => {
                    if id.is_empty() {
                        return Err(EmptyCallIdSnafu { location }.build());
                    }
                    if name.is_empty() {
                        return Err(EmptyCallNameSnafu { location }.build());
                    }
                    if let Some((first, _)) = calls.insert(id, (location, None)) {
                        return Err(DuplicateCallIdSnafu {
                            location,
                            first,
                            id: id.clone(),
                        }
                        .build());
                    }
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    loaded_tools,
                    ..
                } => {
                    if tool_use_id.is_empty() {
                        return Err(EmptyResultIdSnafu { location }.build());
                    }
                    let Some((_, result)) = calls.get_mut(tool_use_id.as_str()) else {
                        return Err(OrphanResultSnafu {
                            location,
                            id: tool_use_id.clone(),
                        }
                        .build());
                    };
                    if let Some(first) = result.replace(location) {
                        return Err(DuplicateResultSnafu {
                            location,
                            first,
                            id: tool_use_id.clone(),
                        }
                        .build());
                    }
                    let mut loaded = BTreeMap::new();
                    for (index, name) in loaded_tools.iter().enumerate() {
                        if name.is_empty() {
                            return Err(EmptyLoadedToolSnafu { location, index }.build());
                        }
                        if let Some(first_index) = loaded.insert(name, index) {
                            return Err(DuplicateLoadedToolSnafu {
                                location,
                                index,
                                first_index,
                                name: name.clone(),
                            }
                            .build());
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
        return Err(MissingResultSnafu {
            location: *location,
            id: (*id).to_owned(),
        }
        .build());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

    use super::*;

    use serde_json::json;

    use crate::history::MessageKind;

    type Expected = fn(&ReplayError) -> bool;

    fn message(role: Role, content: Vec<ContentBlock>) -> Message {
        Message::builder()
            .role(role)
            .content(content)
            .kind(MessageKind::Turn)
            .build()
    }

    fn call(id: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.into(),
            name: "historical-tool".into(),
            input: json!({}),
            thought_signature: None,
        }
    }

    fn result(id: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: id.into(),
            name: None,
            content: "done".into(),
            is_error: false,
            loaded_tools: vec![],
        }
    }

    fn loaded_result(id: &str, names: &[&str]) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: id.into(),
            name: None,
            content: "done".into(),
            is_error: false,
            loaded_tools: names.iter().map(|name| (*name).into()).collect(),
        }
    }

    fn tool(name: &str) -> Tool {
        Tool::builder()
            .name(name)
            .description("")
            .input_schema(json!({}))
            .build()
    }

    #[test]
    fn invalid_views_report_the_exact_variant_and_position() {
        let a = Role::Assistant;
        let u = Role::User;
        let empty_name = || {
            let mut block = call("a");
            if let ContentBlock::ToolUse { name, .. } = &mut block {
                name.clear();
            }
            block
        };
        let cases: Vec<(Vec<Message>, Vec<Tool>, Expected)> = vec![
            (vec![message(a.clone(), vec![call("")])], vec![], |error| {
                matches!(
                    error,
                    ReplayError::EmptyCallId { location, .. }
                        if *location == ReplayLocation { message: 0, block: 0 }
                )
            }),
            (
                vec![message(a.clone(), vec![call("a"), call("a")])],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::DuplicateCallId { location, first, id, .. }
                            if *location == ReplayLocation { message: 0, block: 1 }
                                && *first == ReplayLocation { message: 0, block: 0 }
                                && id == "a"
                    )
                },
            ),
            (
                vec![message(a.clone(), vec![empty_name()])],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::EmptyCallName { location, .. }
                            if *location == ReplayLocation { message: 0, block: 0 }
                    )
                },
            ),
            (
                vec![message(u.clone(), vec![result("")])],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::EmptyResultId { location, .. }
                            if *location == ReplayLocation { message: 0, block: 0 }
                    )
                },
            ),
            (
                vec![message(u.clone(), vec![result("unknown")])],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::OrphanResult { location, id, .. }
                            if *location == ReplayLocation { message: 0, block: 0 }
                                && id == "unknown"
                    )
                },
            ),
            (
                vec![
                    message(a.clone(), vec![call("a")]),
                    message(u.clone(), vec![result("a"), result("a")]),
                ],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::DuplicateResult { location, first, id, .. }
                            if *location == ReplayLocation { message: 1, block: 1 }
                                && *first == ReplayLocation { message: 1, block: 0 }
                                && id == "a"
                    )
                },
            ),
            (
                vec![message(a.clone(), vec![call("z"), call("a")])],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::MissingResult { location, id, .. }
                            if *location == ReplayLocation { message: 0, block: 0 } && id == "z"
                    )
                },
            ),
            (
                vec![message(
                    u.clone(),
                    vec![ContentBlock::Thinking {
                        thinking: "private".into(),
                        signature: None,
                    }],
                )],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::AssistantContentInUser { location, .. }
                            if *location == ReplayLocation { message: 0, block: 0 }
                    )
                },
            ),
            (
                vec![message(a.clone(), vec![result("a")])],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::ResultInAssistant { location, .. }
                            if *location == ReplayLocation { message: 0, block: 0 }
                    )
                },
            ),
            (vec![], vec![tool("")], |error| {
                matches!(error, ReplayError::EmptyDeclaration { index: 0, .. })
            }),
            (vec![], vec![tool("x"), tool("x")], |error| {
                matches!(
                    error,
                    ReplayError::DuplicateDeclaration { index: 1, first_index: 0, name, .. }
                        if name == "x"
                )
            }),
            (
                vec![
                    message(a.clone(), vec![call("a")]),
                    message(u.clone(), vec![loaded_result("a", &[""])]),
                ],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::EmptyLoadedTool { location, index: 0, .. }
                            if *location == ReplayLocation { message: 1, block: 0 }
                    )
                },
            ),
            (
                vec![
                    message(a, vec![call("a")]),
                    message(u, vec![loaded_result("a", &["x", "x"])]),
                ],
                vec![],
                |error| {
                    matches!(
                        error,
                        ReplayError::DuplicateLoadedTool {
                            location,
                            index: 1,
                            first_index: 0,
                            name,
                            ..
                        } if *location == ReplayLocation { message: 1, block: 0 } && name == "x"
                    )
                },
            ),
        ];
        for (messages, tools, expected) in cases {
            assert!(expected(&validate_replay(&messages, &tools).unwrap_err()));
        }
    }

    #[test]
    fn complete_interleaved_views_validate_without_rewriting() {
        let messages = vec![
            message(
                Role::Assistant,
                vec![
                    ContentBlock::Thinking {
                        thinking: "private".into(),
                        signature: Some("opaque".into()),
                    },
                    ContentBlock::RedactedThinking {
                        data: "redacted".into(),
                    },
                    call("a"),
                    call("b"),
                ],
            ),
            message(
                Role::User,
                vec![ContentBlock::Text {
                    text: "Request-only context".into(),
                }],
            ),
            message(Role::User, vec![result("b")]),
            message(Role::User, vec![result("a")]),
        ];
        let before = serde_json::to_value(&messages).unwrap();
        validate_replay(&messages, &[tool("current-tool")]).unwrap();
        assert_eq!(serde_json::to_value(&messages).unwrap(), before);
    }

    #[test]
    fn locations_render_as_message_and_block_positions() {
        assert_eq!(
            ReplayLocation {
                message: 3,
                block: 1
            }
            .to_string(),
            "message 3, block 1"
        );
        let error =
            validate_replay(&[message(Role::User, vec![result("orphan")])], &[]).unwrap_err();
        assert!(error.to_string().contains("at message 0, block 0"));
    }
}
