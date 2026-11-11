use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    panic::AssertUnwindSafe,
};

use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use futures_util::FutureExt;
use serde::de::DeserializeOwned;
use serde_json::Value;
use snafu::{ResultExt, Snafu};

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
    /// Unwinding panics only; under `panic = "abort"` the process exits instead.
    #[snafu(display("Tool execution panicked: {message}"))]
    Panicked {
        message: String,
        backtrace: snafu::Backtrace,
    },
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

    /// Application-owned registration metadata; never sent to providers.
    fn metadata(&self) -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

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

    /// Application-owned registration metadata; never sent to providers.
    fn metadata(&self) -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

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
    /// Isolates user executor panics at the tool boundary as `Panicked` failures.
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
                PanickedSnafu { message }.fail()
            })
    }
}

pub(super) struct TypedTool<T>(pub(super) T);

#[async_trait]
impl<T: ToolImpl> ToolExecutorImpl for TypedTool<T> {
    fn definition(&self) -> ToolDefinition {
        self.0.definition()
    }

    fn metadata(&self) -> BTreeMap<String, Value> {
        self.0.metadata()
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
            .boxed()
            .context(ExecutionSnafu)?
            .into_tool_output()
    }
}

/// A registered tool: its provider declaration plus application-owned metadata.
/// Metadata is opaque to Animist and never sent to providers.
#[derive(Struct!, PartialEq)]
pub struct ToolEntry {
    pub definition: ToolDefinition,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone)]
struct RegisteredTool {
    entry: ToolEntry,
    executor: ToolExecutor,
}

/// Immutable allowlist of tool names built by `ToolRegistry::selection`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSelection {
    names: BTreeSet<String>,
}

impl ToolSelection {
    /// Read-only membership check; selections cannot be mutated or forged.
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

/// The exclusive authorization source for one dispatch pipeline.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ToolScope<'a> {
    Request(&'a [ToolDefinition]),
    Selection(&'a ToolSelection),
}

impl ToolScope<'_> {
    pub(crate) fn admits(&self, name: &str) -> bool {
        match self {
            Self::Request(visible) => visible.iter().any(|tool| tool.name == name),
            Self::Selection(selection) => selection.contains(name),
        }
    }
}

/// Registration freezes each declaration alongside its executable handle.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
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
    /// Registration captures `metadata` once; later executor state is ignored.
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
        let entry = ToolEntry {
            metadata: tool.metadata(),
            definition,
        };
        self.tools.insert(
            entry.definition.name.clone(),
            RegisteredTool {
                entry,
                executor: tool,
            },
        );
        Ok(())
    }

    /// Frozen declarations advertised to inference, sorted by tool name.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|tool| tool.entry.definition.clone())
            .collect()
    }

    /// Registered declarations with their application metadata, name-sorted.
    pub fn entries(&self) -> Vec<ToolEntry> {
        self.tools.values().map(|tool| tool.entry.clone()).collect()
    }

    /// Freezes the entries matching `include` into an immutable name allowlist.
    /// The predicate runs once, synchronously; the selection stores names only.
    pub fn selection(&self, include: impl Fn(&ToolEntry) -> bool) -> ToolSelection {
        ToolSelection {
            names: self
                .tools
                .values()
                .map(|tool| &tool.entry)
                .filter(|entry| include(entry))
                .map(|entry| entry.definition.name.clone())
                .collect(),
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&ToolExecutor> {
        self.tools.get(name).map(|tool| &tool.executor)
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
    pub(crate) scope: ToolScope<'a>,
    pub(crate) scheduling: ToolScheduling,
    pub(crate) children: std::sync::atomic::AtomicU64,
    pub(crate) child_gate: tokio::sync::Mutex<()>,
}

impl ToolContext<'_> {
    /// Children inherit the exclusive reservation and execute serially.
    pub async fn dispatch(&self, call: ToolCall) -> Result<ToolOutput> {
        self.dispatch_scoped(call, self.scope).await
    }

    /// Dispatches one child admitted by an explicit application allowlist.
    /// Selected names execute even when hidden from the request; the
    /// selection never widens model or root admission.
    pub async fn dispatch_in(&self, call: ToolCall, tools: &ToolSelection) -> Result<ToolOutput> {
        self.dispatch_scoped(call, ToolScope::Selection(tools))
            .await
    }

    async fn dispatch_scoped(&self, call: ToolCall, scope: ToolScope<'_>) -> Result<ToolOutput> {
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
                scope,
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
        self.dispatch_all_scoped(calls, self.scope).await
    }

    /// Fans child calls out under the wave rules, admitted by an explicit
    /// application allowlist instead of request visibility. Selected names
    /// execute even when hidden; model and root admission are unchanged.
    pub async fn dispatch_all_in(
        &self,
        calls: Vec<ToolCall>,
        tools: &ToolSelection,
    ) -> Result<Vec<ToolOutput>> {
        self.dispatch_all_scoped(calls, ToolScope::Selection(tools))
            .await
    }

    async fn dispatch_all_scoped(
        &self,
        calls: Vec<ToolCall>,
        scope: ToolScope<'_>,
    ) -> Result<Vec<ToolOutput>> {
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
            .execute_child_waves(self.run, &calls, &self.call.id, self.depth + 1, scope)
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
    #![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use serde_json::json;

    use super::{
        ToolContext, ToolDefinition, ToolError, ToolExecutor, ToolExecutorImpl, ToolImpl,
        ToolOutput, ToolRegistry, ToolScheduling,
    };
    use crate::agent::AgentError;

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

    fn entry(name: &str) -> ToolDefinition {
        ToolDefinition::builder()
            .name(name)
            .description("test tool")
            .input_schema(json!({}))
            .build()
    }

    struct Named {
        name: &'static str,
        metadata: BTreeMap<String, serde_json::Value>,
    }

    #[async_trait]
    impl ToolImpl for Named {
        type Args = serde_json::Value;
        type Output = String;
        type Error = std::convert::Infallible;

        fn definition(&self) -> ToolDefinition {
            entry(self.name)
        }

        fn metadata(&self) -> BTreeMap<String, serde_json::Value> {
            self.metadata.clone()
        }

        async fn call(
            &self,
            _context: &ToolContext<'_>,
            _args: Self::Args,
        ) -> Result<String, Self::Error> {
            Ok("ok".into())
        }
    }

    struct ErasedNamed {
        name: &'static str,
        metadata: BTreeMap<String, serde_json::Value>,
    }

    #[async_trait]
    impl ToolExecutorImpl for ErasedNamed {
        fn definition(&self) -> ToolDefinition {
            entry(self.name)
        }

        fn metadata(&self) -> BTreeMap<String, serde_json::Value> {
            self.metadata.clone()
        }

        async fn execute(
            &self,
            _context: &ToolContext<'_>,
            _input: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::builder().content("ok").build())
        }
    }

    fn named(name: &'static str) -> Named {
        Named {
            name,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn metadata_defaults_to_empty_and_leaves_declarations_unchanged() {
        let mut tools = ToolRegistry::default();
        tools.register(named("typed")).unwrap();
        tools
            .register_executor(ToolExecutor::new(ErasedNamed {
                name: "erased",
                metadata: BTreeMap::new(),
            }))
            .unwrap();
        tools.register(named("rich")).unwrap();
        assert!(tools.entries().iter().all(|tool| tool.metadata.is_empty()));
        let plain = {
            let mut plain = ToolRegistry::default();
            plain.register(named("typed")).unwrap();
            plain
                .register_executor(ToolExecutor::new(ErasedNamed {
                    name: "erased",
                    metadata: BTreeMap::new(),
                }))
                .unwrap();
            plain.register(named("rich")).unwrap();
            plain.definitions()
        };
        assert_eq!(tools.definitions(), plain);
        assert_eq!(
            tools
                .entries()
                .iter()
                .map(|tool| tool.definition.clone())
                .collect::<Vec<_>>(),
            tools.definitions()
        );
        assert!(matches!(
            tools.register_executor(ToolExecutor::new(ErasedNamed {
                name: "",
                metadata: BTreeMap::new(),
            })),
            Err(AgentError::EmptyToolName { .. })
        ));
    }

    #[test]
    fn registration_captures_metadata_and_sorts_entries_by_name() {
        let mut tools = ToolRegistry::default();
        let typed = BTreeMap::from([
            ("app.callers".to_owned(), json!(["batch"])),
            ("nested.keys".to_owned(), json!({"deep": {"list": [1, 2]}})),
        ]);
        tools
            .register(Named {
                name: "zeta",
                metadata: typed.clone(),
            })
            .unwrap();
        let erased = BTreeMap::from([("scope".to_owned(), json!("internal"))]);
        tools
            .register_executor(ToolExecutor::new(ErasedNamed {
                name: "alpha",
                metadata: erased.clone(),
            }))
            .unwrap();
        let entries = tools.entries();
        assert_eq!(
            entries
                .iter()
                .map(|tool| tool.definition.name.clone())
                .collect::<Vec<_>>(),
            vec!["alpha".to_owned(), "zeta".to_owned()]
        );
        assert_eq!(entries[0].metadata, erased);
        assert_eq!(entries[1].metadata, typed);
    }

    #[test]
    fn selections_freeze_membership_without_touching_the_registry() {
        let callers = |name: &str| BTreeMap::from([("app.callers".to_owned(), json!([name]))]);
        let mut tools = ToolRegistry::default();
        tools
            .register(Named {
                name: "batch",
                metadata: callers("batch"),
            })
            .unwrap();
        tools
            .register(Named {
                name: "interactive",
                metadata: callers("interactive"),
            })
            .unwrap();
        let selection =
            tools.selection(|tool| tool.metadata.get("app.callers") == Some(&json!(["batch"])));
        assert!(selection.contains("batch"));
        assert!(!selection.contains("interactive"));
        assert!(!selection.contains("absent"));
        let empty = tools.selection(|_| false);
        let no_match = tools.selection(|tool| tool.metadata.contains_key("unrecognized"));
        assert_eq!(empty, no_match);
        assert!(!empty.contains("batch"));
        let before = (tools.definitions(), tools.entries());
        tools.selection(|tool| tool.definition.name == "interactive");
        assert_eq!((tools.definitions(), tools.entries()), before);
        assert_ne!(selection, empty);
        let rebuilt = {
            let mut rebuilt = ToolRegistry::default();
            rebuilt
                .register(Named {
                    name: "batch",
                    metadata: callers("batch"),
                })
                .unwrap();
            rebuilt.selection(|tool| tool.metadata.get("app.callers") == Some(&json!(["batch"])))
        };
        assert_eq!(selection, rebuilt);
    }
}
