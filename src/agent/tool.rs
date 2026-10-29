use std::{collections::BTreeMap, error::Error, panic::AssertUnwindSafe};

use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use futures_util::FutureExt;
use serde::de::DeserializeOwned;
use serde_json::Value;
use snafu::{IntoError, ResultExt, Snafu};

use super::{Agent, Result, RunContext};
use crate::Struct;
pub use crate::history::{ToolContent, ToolContents};
pub use crate::provider::Tool as ToolDefinition;

/// Resource keys are opaque application-owned identities, not normalized paths.
#[derive(crate::Enum!, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolScheduling {
    #[default]
    Exclusive,
    Resources {
        reads: Vec<String>,
        writes: Vec<String>,
    },
}

impl ToolScheduling {
    pub(crate) fn conflicts(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Resources { reads, writes },
                Self::Resources {
                    reads: other_reads,
                    writes: other_writes,
                },
            ) => {
                writes
                    .iter()
                    .any(|key| other_reads.contains(key) || other_writes.contains(key))
                    || other_writes.iter().any(|key| reads.contains(key))
            }
            _ => true,
        }
    }
}

/// An application may override tool claims without relying on tool-name heuristics.
#[arc_handle]
pub trait ToolSchedulingPolicy {
    /// The effective claim for one call; unknown tools must stay exclusive.
    fn scheduling(&self, call: &ToolCall, declared: ToolScheduling) -> ToolScheduling;
}

impl std::fmt::Debug for ToolSchedulingPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSchedulingPolicy")
            .finish_non_exhaustive()
    }
}

/// Typed tool failures surfaced to the model and retained in run reports.
#[derive(Debug, Snafu)]
pub enum ToolError {
    #[snafu(display("Invalid tool arguments: {source}"))]
    Arguments { source: serde_json::Error },
    #[snafu(display("Tool execution failed: {source}"))]
    Execution {
        source: Box<dyn Error + Send + Sync>,
    },
    #[snafu(display("Tool output conversion failed: {source}"))]
    Output {
        source: Box<dyn Error + Send + Sync>,
    },
    #[snafu(display("Tool execution panicked: {message}"))]
    Panicked { message: String },
}

/// One admitted model tool call with its decoded JSON arguments.
#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Model-visible tool output; `is_error` marks correctable failures.
#[derive(Struct!, PartialEq, Eq, Builder)]
#[builder(on(_, into))]
pub struct ToolOutput {
    pub content: ToolContents,
    #[builder(default)]
    pub is_error: bool,
    #[builder(default)]
    pub loaded_tools: Vec<String>,
}

/// Conversion into model-visible output for typed tool results.
pub trait IntoToolOutput: Send {
    /// Converts the typed result, failing on invalid output content.
    fn into_tool_output(self) -> std::result::Result<ToolOutput, ToolError>;
}

impl IntoToolOutput for ToolOutput {
    fn into_tool_output(self) -> std::result::Result<ToolOutput, ToolError> {
        Ok(self)
    }
}

impl IntoToolOutput for String {
    fn into_tool_output(self) -> std::result::Result<ToolOutput, ToolError> {
        Ok(ToolOutput::builder().content(self).build())
    }
}

/// A typed tool: declaration, scheduling claim, and async execution.
#[async_trait]
pub trait ToolImpl: Send + Sync + 'static {
    type Args: DeserializeOwned + Send;
    type Output: IntoToolOutput;
    type Error: Error + Send + Sync + 'static;

    /// The frozen declaration advertised to inference.
    fn definition(&self) -> ToolDefinition;

    /// Resource claim for one call; defaults to fully exclusive.
    fn scheduling(&self, _input: &Value) -> ToolScheduling {
        ToolScheduling::Exclusive
    }

    /// Executes one call with decoded arguments; never retried automatically.
    async fn call(
        &self,
        context: &ToolContext<'_>,
        args: Self::Args,
    ) -> std::result::Result<Self::Output, Self::Error>;
}

/// Erased executor pairing a declaration with runtime execution.
#[arc_handle]
#[async_trait]
pub trait ToolExecutor {
    /// The frozen declaration advertised to inference.
    fn definition(&self) -> ToolDefinition;

    /// Resource claim for one call; defaults to fully exclusive.
    fn scheduling(&self, _input: &Value) -> ToolScheduling {
        ToolScheduling::Exclusive
    }

    /// Executes one call; decoding failures become model-visible errors.
    async fn execute(
        &self,
        context: &ToolContext<'_>,
        input: Value,
    ) -> std::result::Result<ToolOutput, ToolError>;
}

impl ToolExecutor {
    pub(crate) async fn execute_catching_panics(
        &self,
        context: &ToolContext<'_>,
        input: Value,
    ) -> std::result::Result<ToolOutput, ToolError> {
        AssertUnwindSafe(async { self.execute(context, input).await })
            .catch_unwind()
            .await
            .unwrap_or_else(|payload| {
                let message = if let Some(message) = payload.downcast_ref::<String>() {
                    message.clone()
                } else if let Some(message) = payload.downcast_ref::<&str>() {
                    (*message).to_owned()
                } else {
                    "non-string panic payload".to_owned()
                };
                Err(ToolError::Panicked { message })
            })
    }
}

pub(super) struct TypedTool<T>(pub(super) T);

#[async_trait]
impl<T: ToolImpl> ToolExecutorImpl for TypedTool<T> {
    fn definition(&self) -> ToolDefinition {
        self.0.definition()
    }

    fn scheduling(&self, input: &Value) -> ToolScheduling {
        self.0.scheduling(input)
    }

    async fn execute(
        &self,
        context: &ToolContext<'_>,
        input: Value,
    ) -> std::result::Result<ToolOutput, ToolError> {
        let args = serde_json::from_value(input).context(ArgumentsSnafu)?;
        self.0
            .call(context, args)
            .await
            .map_err(|source| ExecutionSnafu.into_error(Box::new(source)))?
            .into_tool_output()
    }
}

/// Registration freezes each declaration alongside its executable handle.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, (ToolDefinition, ToolExecutor)>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}

impl ToolRegistry {
    /// Registers a typed tool; duplicate or empty names fail.
    pub fn register<T: ToolImpl>(&mut self, tool: T) -> Result<()> {
        self.register_executor(ToolExecutor::new(TypedTool(tool)))
    }

    /// Registers a runtime-named executor under its own declaration.
    pub fn register_executor(&mut self, tool: ToolExecutor) -> Result<()> {
        let definition = tool.definition();
        if definition.name.is_empty() {
            return Err(super::error::EmptyToolNameSnafu.build());
        }
        if self.tools.contains_key(&definition.name) {
            return Err(super::error::DuplicateToolSnafu {
                name: definition.name,
            }
            .build());
        }
        self.tools
            .insert(definition.name.clone(), (definition, tool));
        Ok(())
    }

    /// Frozen declarations advertised to inference, in registration order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|(d, _)| d.clone()).collect()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&ToolExecutor> {
        self.tools.get(name).map(|(_, tool)| tool)
    }
}

/// Child calls use the same authorization pipeline and bounded nesting.
#[derive(Debug)]
pub struct ToolContext<'a> {
    /// Run the call belongs to; drives cancellation and statistics.
    pub run: &'a RunContext,
    /// The admitted call being executed.
    pub call: &'a ToolCall,
    /// Parent call identity for child dispatches; absent for root calls.
    pub parent_call_id: Option<&'a str>,
    pub(crate) agent: &'a Agent,
    pub(crate) depth: u32,
    pub(crate) visible: &'a [ToolDefinition],
    pub(crate) scheduling: ToolScheduling,
    pub(crate) children: std::sync::atomic::AtomicU64,
    pub(crate) child_gate: tokio::sync::Mutex<()>,
}

impl ToolContext<'_> {
    /// Children inherit the exclusive reservation and execute serially.
    pub async fn dispatch(&self, call: ToolCall) -> Result<ToolOutput> {
        if self.scheduling != ToolScheduling::Exclusive {
            return Err(super::error::ChildDispatchRequiresExclusiveSnafu.build());
        }
        if self
            .children
            .try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |count| {
                    count
                        .checked_add(1)
                        .filter(|next| *next <= self.agent.config().max_child_calls)
                },
            )
            .is_err()
        {
            return Err(super::error::ChildCallLimitExceededSnafu.build());
        }
        let _gate = self
            .run
            .cancellation
            .race(async { Ok(self.child_gate.lock().await) })
            .await?;
        self.agent
            .execute_tool(
                self.run,
                &call,
                Some(&self.call.id),
                self.depth + 1,
                self.visible,
                ToolScheduling::Exclusive,
            )
            .await
    }

    /// Dispatches child calls concurrently under the top-level wave rules.
    /// Exclusive top-level parents fan out into bounded, conflict-free
    /// waves; results arrive in call order, per-child failures stay
    /// `is_error` outputs, and fatal errors drop siblings with the typed
    /// error. See *Bounded scheduling* in `docs/agents.md`.
    pub async fn dispatch_all(&self, calls: Vec<ToolCall>) -> Result<Vec<ToolOutput>> {
        if self.scheduling != ToolScheduling::Exclusive {
            return Err(super::error::ChildDispatchRequiresExclusiveSnafu.build());
        }
        if self.parent_call_id.is_some() {
            return Err(super::error::NestedConcurrentDispatchSnafu.build());
        }
        if self
            .children
            .try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |count| {
                    count
                        .checked_add(u64::try_from(calls.len()).unwrap_or(u64::MAX))
                        .filter(|next| *next <= self.agent.config().max_child_calls)
                },
            )
            .is_err()
        {
            return Err(super::error::ChildCallLimitExceededSnafu.build());
        }
        let _gate = self
            .run
            .cancellation
            .race(async { Ok(self.child_gate.lock().await) })
            .await?;
        self.agent
            .execute_child_waves(
                self.run,
                &calls,
                &self.call.id,
                self.depth + 1,
                self.visible,
            )
            .await
    }

    /// Emits a progress note for this call; display-only, never persisted.
    pub async fn progress(&self, content: impl Into<String>) {
        self.run
            .emit(super::AgentEvent::ToolProgress {
                id: self.call.id.clone(),
                content: content.into(),
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::ToolScheduling;

    #[test]
    fn resource_conflicts_are_symmetric_and_exclusive_is_conservative() {
        let read = ToolScheduling::Resources {
            reads: vec!["a".into()],
            writes: vec![],
        };
        let write = ToolScheduling::Resources {
            reads: vec![],
            writes: vec!["a".into()],
        };
        let unrelated = ToolScheduling::Resources {
            reads: vec![],
            writes: vec!["b".into()],
        };
        let independent = ToolScheduling::Resources {
            reads: vec![],
            writes: vec![],
        };
        assert!(!read.conflicts(&read));
        assert!(read.conflicts(&write));
        assert!(write.conflicts(&read));
        assert!(write.conflicts(&write));
        assert!(!write.conflicts(&unrelated));
        assert!(!write.conflicts(&independent));
        assert!(ToolScheduling::default().conflicts(&independent));
        assert!(independent.conflicts(&ToolScheduling::default()));
    }
}
