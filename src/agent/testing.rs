//! Direct tool invocation without inference or a conversation loop.

use std::sync::{Arc, atomic::AtomicU64};

use async_trait::async_trait;
use flume::{Receiver, Sender};
use serde_json::Value;
use tokio::sync::Mutex;

use super::{
    Agent, AgentConfig, AgentEvent, AgentLayer, Cancellation, RunContext,
    tool::{
        ToolCall, ToolContext, ToolError, ToolExecutor, ToolImpl, ToolOutput, ToolRegistry,
        ToolSchedulingPolicy, TypedTool,
    },
};
use crate::{
    history::Message,
    provider::{
        Model, ModelInfo, Provider, ProviderEvent, ProviderImpl, RequestOptions, StreamResponse,
        Tool,
    },
};

/// One tool-test run with shared cancellation, counters, and an unbounded event queue.
/// Layers must not invoke inference through the placeholder model.
/// Doing so panics; only child tool hooks are supported.
#[derive(Debug)]
pub struct ToolHarness {
    agent: Agent,
    run: RunContext,
    events: Receiver<AgentEvent>,
    next_call: u64,
}

#[bon::bon]
impl ToolHarness {
    /// Builds a provider-free run; child calls use the supplied registry and layers.
    /// Zero tool limits are rejected. No layer lifecycle or inference hooks run.
    #[builder]
    pub fn new(
        #[builder(default)] tools: ToolRegistry,
        #[builder(default)] config: AgentConfig,
        layers: Option<AgentLayer>,
        tool_scheduling_policy: Option<ToolSchedulingPolicy>,
        #[builder(default)] cancellation: Cancellation,
        #[builder(default)] run_id: u64,
    ) -> super::Result<Self> {
        if config.max_tool_depth == 0
            || config.max_concurrent_tools == 0
            || config.max_tool_calls == 0
            || config.max_child_calls == 0
        {
            return Err(super::error::InvalidLimitsSnafu.build());
        }
        let (sender, events) = flume::unbounded();
        Ok(Self {
            agent: Agent::builder()
                .model(
                    Model::builder()
                        .id("tool-harness")
                        .provider(Arc::new(Provider::new(NoInference)))
                        .info(ModelInfo::default())
                        .build(),
                )
                .tools(tools)
                .config(config)
                .maybe_layers(layers)
                .maybe_tool_scheduling_policy(tool_scheduling_policy)
                .build(),
            run: RunContext {
                id: run_id,
                cancellation,
                stats: Arc::default(),
                events: Some(sender),
                stopped: Mutex::default(),
                tool_halt: Cancellation::default(),
                tool_dispatches: AtomicU64::default(),
                tool_failures: Mutex::default(),
                interrupted_attempts: Mutex::default(),
            },
            events,
            next_call: 0,
        })
    }

    /// Run state shared by every invocation; cancellation and child budgets persist.
    pub fn run(&self) -> &RunContext {
        &self.run
    }

    /// Clone this handle before invoking a tool to cancel it concurrently.
    pub fn cancellation(&self) -> Cancellation {
        self.run.cancellation.clone()
    }

    /// Observes progress and child execution events; cloned receivers compete.
    pub fn events(&self) -> Receiver<AgentEvent> {
        self.events.clone()
    }

    /// Decodes arguments and calls a typed tool, preserving its structured errors.
    pub async fn call<T: ToolImpl>(
        &mut self,
        tool: T,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        self.call_executor(ToolExecutor::new(TypedTool(tool)), input)
            .await
    }

    /// Invokes an executor directly; root cancellation is cooperative.
    /// Root calls bypass layers, timeouts, and counters; children use the agent pipeline.
    /// Mutable access serializes roots, preserving exclusive dispatch reservations.
    /// Child stop decisions terminate the root and reject later invocations.
    pub async fn call_executor(
        &mut self,
        tool: ToolExecutor,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        let call = ToolCall::builder()
            .id(format!("harness-{}", self.next_call))
            .name(tool.definition().name)
            .input(input)
            .build();
        self.next_call += 1;
        let declared = tool.scheduling(&call.input);
        let scheduling = match &self.agent.tool_scheduling_policy {
            Some(policy) => policy.scheduling(&call, declared),
            None => declared,
        };
        let visible = self.agent.tools.definitions();
        let context = ToolContext {
            run: &self.run,
            call: &call,
            parent_call_id: None,
            agent: &self.agent,
            depth: 0,
            visible: &visible,
            scheduling,
            children: AtomicU64::default(),
            child_gate: Mutex::default(),
        };
        let result = tokio::select! {
            biased;
            () = self.run.tool_halt.cancelled() => Err(ToolError::Execution {
                source: Box::new(super::error::CancelledSnafu.build()),
            }),
            result = tool.execute_catching_panics(&context, call.input.clone()) => result,
        };
        if let Some(reason) = self.run.stopped.lock().await.clone() {
            return Err(ToolError::Execution {
                source: Box::new(super::error::StoppedSnafu { reason }.build()),
            });
        }
        result
    }
}

struct NoInference;

#[async_trait]
impl ProviderImpl for NoInference {
    fn name(&self) -> &'static str {
        "tool-harness"
    }

    async fn stream_message(
        &self,
        _model: &Model,
        _messages: &[Message],
        _system: &str,
        _tools: &[Tool],
        _events: &Sender<ProviderEvent>,
        _options: RequestOptions,
    ) -> crate::provider::Result<StreamResponse> {
        unreachable!("tool harness layers must not invoke inference")
    }

    async fn list_models(&self) -> crate::provider::Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }

    async fn refresh_auth(&self) -> crate::provider::Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> crate::provider::Result<()> {
        Ok(())
    }
}
