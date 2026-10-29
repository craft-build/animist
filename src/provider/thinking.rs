//! Reasoning policy shared by providers that speak the same wire dialect.

use snafu::ensure;

use super::{Effort, Model, Result, ThinkingConfig, error};
use crate::protocol::anthropic::{AnthropicEffort, AnthropicThinking};

/// Maps a reasoning request onto Anthropic Messages thinking and effort.
pub(super) fn anthropic_thinking(
    model: &Model,
    thinking: &ThinkingConfig,
    max_tokens: u32,
) -> Result<(AnthropicThinking, Option<AnthropicEffort>)> {
    let supported = model.thinking_supported();
    match thinking {
        ThinkingConfig::Off => {
            ensure!(!model.thinking_required(), error::ReasoningRequiredSnafu);
            Ok((AnthropicThinking::Disabled, None))
        }
        ThinkingConfig::Adaptive => {
            ensure!(
                supported,
                error::ThinkingUnsupportedSnafu { model: &model.id }
            );
            Ok((AnthropicThinking::Adaptive, None))
        }
        ThinkingConfig::Budget(budget) => {
            ensure!(
                supported,
                error::ThinkingUnsupportedSnafu { model: &model.id }
            );
            ensure!(
                *budget >= 1024 && *budget < max_tokens,
                error::InvalidThinkingBudgetSnafu {
                    budget: *budget,
                    max_tokens
                }
            );
            Ok((
                AnthropicThinking::Enabled {
                    budget_tokens: *budget,
                },
                None,
            ))
        }
        ThinkingConfig::Effort(requested) => {
            ensure!(
                supported && model.info.features.thinking.efforts.contains(requested),
                error::UnsupportedReasoningEffortSnafu {
                    effort: requested.clone()
                }
            );
            let effort = match requested {
                Effort::Low => AnthropicEffort::Low,
                Effort::Medium => AnthropicEffort::Medium,
                Effort::High => AnthropicEffort::High,
                Effort::XHigh => AnthropicEffort::XHigh,
                Effort::Max => AnthropicEffort::Max,
                Effort::Minimal | Effort::Adaptive => {
                    return error::UnsupportedReasoningEffortSnafu {
                        effort: requested.clone(),
                    }
                    .fail();
                }
            };
            Ok((AnthropicThinking::Adaptive, Some(effort)))
        }
    }
}

/// Resolves `reasoning_effort` for hosted `OpenAI` API endpoints, which
/// accept neither `max` nor explicit budgets.
pub(super) fn openai_reasoning_effort(
    model: &Model,
    thinking: &ThinkingConfig,
) -> Result<Option<&'static str>> {
    let supported = model.thinking_supported();
    match thinking {
        ThinkingConfig::Off => {
            ensure!(!model.thinking_required(), error::ReasoningRequiredSnafu);
            Ok(supported.then_some("none"))
        }
        ThinkingConfig::Adaptive => {
            ensure!(
                supported,
                error::ThinkingUnsupportedSnafu { model: &model.id }
            );
            Ok(None)
        }
        ThinkingConfig::Budget(_) => error::ReasoningBudgetUnsupportedSnafu.fail(),
        ThinkingConfig::Effort(effort) => {
            ensure!(
                supported
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
