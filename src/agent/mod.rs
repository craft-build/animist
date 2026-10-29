//! Layered agent runtime: tool registry, linked hook chain, and run loop.

mod config;
mod error;
mod layer;
mod prompt;
mod run;
pub mod tool;

pub use config::AgentConfig;
pub use error::AgentError;
pub use layer::{AgentLayer, Layer, LayerImpl, Recovery, StepAction, ToolDecision};
pub use prompt::{Prompt, Section};
pub use run::{
    AgentEvent, Cancellation, InterruptedAttempt, RunContext, RunOptions, RunReport, RunStats,
    RunStatus, StatsSnapshot, StepOutcome, StepRequest, ToolFailure,
};

use crate::{
    history::{
        CompactionEdit, CompactionPlan, Compactor, ContentBlock, History, HistoryEntry,
        HistorySnapshot, Message, Role, StoredMessage,
    },
    provider::{ExchangeEvent, InferenceExchange, Model, StopReason, StreamResponse},
};
use async_trait::async_trait;
use bon::Builder;
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::{Mutex, OnceCell};
use tool::{ToolCall, ToolContext, ToolOutput, ToolRegistry, ToolScheduling, ToolSchedulingPolicy};

pub type Result<T> = std::result::Result<T, AgentError>;

/// A conversation-scoped agent runtime; see `docs/agents.md`.
#[derive(Debug, Builder)]
#[builder(on(_, into))]
pub struct Agent {
    model: Model,
    #[builder(default)]
    prompt: Prompt,
    #[builder(default)]
    history: History,
    #[builder(default)]
    config: AgentConfig,
    #[builder(default)]
    tools: ToolRegistry,
    tool_scheduling_policy: Option<ToolSchedulingPolicy>,
    layers: Option<AgentLayer>,
    #[builder(skip)]
    gate: Mutex<bool>,
    #[builder(skip)]
    started: OnceCell<()>,
    #[builder(skip)]
    next_run: AtomicU64,
}

impl Agent {
    /// Shared history handle; concurrent edits conflict with active runs.
    pub fn history(&self) -> History {
        self.history.clone()
    }

    /// The agent's serializable execution limits.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Startup is lazy and runs once. Shutdown is explicit and terminal.
    pub async fn shutdown(&self) -> Result<()> {
        let Ok(mut closed) = self.gate.try_lock() else {
            return Err(error::AlreadyRunningSnafu.build());
        };
        if *closed {
            return Ok(());
        }
        *closed = true;
        if self.started.initialized()
            && let Some(layers) = &self.layers
        {
            layers.shutdown(self).await?;
        }
        Ok(())
    }

    /// Cancel through `RunOptions` and await this future to finalize tool history.
    /// Dropping it leaves explicit interrupted results for unfinished tools.
    pub async fn run(&self, input: StoredMessage, options: RunOptions) -> Result<RunReport> {
        let Ok(closed) = self.gate.try_lock() else {
            return Err(error::AlreadyRunningSnafu.build());
        };
        if *closed {
            return Err(error::ShutDownSnafu.build());
        }
        if self.config.max_steps == 0
            || self.config.max_tool_depth == 0
            || self.config.max_concurrent_tools == 0
            || self.config.max_tool_calls == 0
            || self.config.max_child_calls == 0
        {
            return Err(error::InvalidLimitsSnafu.build());
        }
        let run = RunContext {
            id: self.next_run.fetch_add(1, Ordering::Relaxed),
            cancellation: options.cancellation,
            stats: std::sync::Arc::default(),
            events: options.events,
            stopped: Mutex::new(None),
            tool_halt: Cancellation::default(),
            tool_dispatches: AtomicU64::default(),
            tool_failures: Mutex::default(),
            interrupted_attempts: Mutex::default(),
        };
        let result = self.run_inner(input, options.request, &run).await;
        let status = match &result {
            Ok(report) => report.status,
            Err(AgentError::Cancelled { .. }) => RunStatus::Cancelled,
            Err(AgentError::Stopped { .. }) => RunStatus::Stopped,
            Err(_) => RunStatus::Failed,
        };
        let cleanup = match &self.layers {
            Some(layers) => layers.turn_end(self, &run, status).await,
            None => Ok(()),
        };
        run.emit(AgentEvent::Finished {
            run_id: run.id,
            status: if cleanup.is_err() {
                RunStatus::Failed
            } else {
                status
            },
        })
        .await;
        match result {
            Err(AgentError::Cancelled { .. }) => {
                cleanup?;
                Ok(RunReport {
                    id: run.id,
                    status,
                    response: None,
                    stats: run.stats.snapshot().await,
                    tool_failures: run.tool_failures().await,
                    interrupted_attempts: run.interrupted_attempts().await,
                })
            }
            Err(error) => match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(AgentError::Finalization {
                    source: Box::new(error),
                    cleanup: Box::new(cleanup),
                }),
            },
            Ok(report) => {
                cleanup?;
                Ok(report)
            }
        }
    }

    async fn run_inner(
        &self,
        input: StoredMessage,
        options: crate::provider::RequestOptions,
        run: &RunContext,
    ) -> Result<RunReport> {
        if run.cancellation.is_cancelled() {
            return Err(error::CancelledSnafu.build());
        }
        self.started
            .get_or_try_init(|| async {
                if let Some(layers) = &self.layers {
                    layers.startup(self).await?;
                }
                Ok::<(), AgentError>(())
            })
            .await?;
        let input = match &self.layers {
            Some(layers) => {
                run.cancellation
                    .race(layers.turn_start(self, run, &input))
                    .await?
            }
            None => input,
        };
        if input.message().role != Role::User {
            return Err(error::InvalidRoleSnafu {
                expected: Role::User,
                actual: input.message().role.clone(),
            }
            .build());
        }
        if input.message().content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
            )
        }) {
            return Err(error::ToolInputMessageSnafu.build());
        }
        if run.cancellation.is_cancelled() {
            return Err(error::CancelledSnafu.build());
        }
        crate::protocol::validate_replay(std::slice::from_ref(input.message()), &[])?;
        self.history.add_stored(input).await;
        let system = self.prompt.to_markdown();
        let mut last_response = None;
        let mut retries = 0;
        for step in 0..self.config.max_steps {
            let (response, expected, visible) = loop {
                let messages = self.history.current().await;
                let definitions = self.tools.definitions();
                crate::protocol::validate_replay(&messages, &definitions)?;
                let request = StepRequest::builder()
                    .model(self.model.clone())
                    .messages(messages)
                    .system(system.clone())
                    .tools(definitions.clone())
                    .options(options.clone())
                    .build();
                let request = match &self.layers {
                    Some(layers) => {
                        run.cancellation
                            .race(layers.prepare_request(self, run, request))
                            .await?
                    }
                    None => request,
                };
                crate::protocol::validate_replay(&request.messages, &request.tools)?;
                validate_tool_subset(&request.tools, &definitions)?;
                let expected = self.history.snapshot().await;
                match self.stream(run, step.into(), &request).await {
                    Ok(response) => break (response, expected, request.tools),
                    Err(error @ AgentError::Inference { .. })
                        if retries < self.config.max_retries =>
                    {
                        let recovery = match &self.layers {
                            Some(layers) => {
                                run.cancellation
                                    .race(layers.recover(self, run, &error))
                                    .await?
                            }
                            None => Recovery::Propagate,
                        };
                        if recovery == Recovery::Propagate {
                            return Err(error);
                        }
                        retries += 1;
                        run.emit(AgentEvent::Retrying {
                            attempt: run.stats.attempts.load(Ordering::Relaxed),
                        })
                        .await;
                    }
                    Err(error) => return Err(error),
                }
            };
            let response = match &self.layers {
                Some(layers) => {
                    run.cancellation
                        .race(layers.response(self, run, response))
                        .await?
                }
                None => response,
            };
            if response.message.role != Role::Assistant {
                return Err(error::InvalidRoleSnafu {
                    expected: Role::Assistant,
                    actual: response.message.role,
                }
                .build());
            }
            let mut ids: BTreeSet<_> = expected
                .entries()
                .iter()
                .filter_map(HistoryEntry::current)
                .flat_map(|stored| &stored.message().content)
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect();
            let mut calls = Vec::new();
            for block in &response.message.content {
                if matches!(block, ContentBlock::ToolResult { .. }) {
                    return Err(error::AssistantToolResultSnafu.build());
                }
                if let ContentBlock::ToolUse {
                    id, name, input, ..
                } = block
                {
                    if id.is_empty() || !ids.insert(id.clone()) {
                        return Err(error::InvalidToolCallIdSnafu { id: id.clone() }.build());
                    }
                    calls.push(
                        ToolCall::builder()
                            .id(id)
                            .name(name)
                            .input(input.clone())
                            .build(),
                    );
                }
            }
            if calls.is_empty() && response.stop_reason == Some(StopReason::ToolUse) {
                return Err(error::MissingToolCallsSnafu.build());
            }
            let stored = StoredMessage::builder()
                .timestamp(jiff::Timestamp::now())
                .message(response.message.clone())
                .build();
            let mut results: Vec<_> = calls.iter().map(|call| ContentBlock::ToolResult {
                tool_use_id: call.id.clone(),
                content: "Tool execution interrupted or not completed; external effects may have occurred".into(),
                is_error: true,
                loaded_tools: Vec::new(),
            }).collect();
            let mut result_message = StoredMessage::builder()
                .timestamp(jiff::Timestamp::now())
                .message(
                    Message::builder()
                        .role(Role::User)
                        .content(results.clone())
                        .kind(crate::history::MessageKind::Turn)
                        .build(),
                )
                .build();
            let result_index = expected.entries().len() + 1;
            let mut edits = vec![CompactionEdit::Insert {
                before: expected.entries().len(),
                message: stored.clone(),
            }];
            if !calls.is_empty() {
                edits.push(CompactionEdit::Insert {
                    before: expected.entries().len(),
                    message: result_message.clone(),
                });
            }
            self.history
                .compact(ExchangeCommit {
                    expected: expected.entries().to_vec(),
                    edits,
                })
                .await?;
            let mut failure = None;
            let mut cursor = 0;
            while cursor < calls.len() {
                let mut expected = self.history.snapshot().await.entries().to_vec();
                if expected.get(result_index).and_then(HistoryEntry::current)
                    != Some(&result_message)
                {
                    return Err(error::HistoryChangedSnafu {
                        messages: vec![result_message],
                    }
                    .build());
                }
                let start = cursor;
                let claims = self.wave_claims(&calls, cursor);
                cursor += claims.len();
                let skipped = failure.is_some();
                let mut running: FuturesUnordered<_> = calls[start..cursor]
                    .iter()
                    .zip(claims)
                    .enumerate()
                    .map(|(offset, (call, claim))| {
                        let visible = &visible;
                        let stop_reason = &response.stop_reason;
                        async move {
                            let output = if skipped {
                                Ok(ToolOutput::builder()
                                    .content("Not executed because the run stopped")
                                    .is_error(true)
                                    .build())
                            } else if matches!(
                                stop_reason,
                                Some(
                                    StopReason::MaxTokens
                                        | StopReason::PauseTurn
                                        | StopReason::Refusal
                                        | StopReason::ContextWindowExceeded
                                )
                            ) {
                                Ok(ToolOutput::builder()
                                    .content("Not executed because generation did not finish")
                                    .is_error(true)
                                    .build())
                            } else {
                                self.execute_tool(run, call, None, 0, visible, claim).await
                            };
                            (start + offset, output)
                        }
                    })
                    .collect();
                while let Some((index, output)) = running.next().await {
                    let call = &calls[index];
                    let output = match output {
                        Ok(output) => output,
                        Err(error) => {
                            let output = ToolOutput::builder()
                                .content(error.to_string())
                                .is_error(true)
                                .build();
                            if failure.is_none() {
                                failure = Some(error);
                                run.tool_halt.cancel();
                            }
                            output
                        }
                    };
                    results[index] = ContentBlock::ToolResult {
                        tool_use_id: call.id.clone(),
                        content: output.content,
                        is_error: output.is_error,
                        loaded_tools: output.loaded_tools,
                    };
                    result_message.message_mut().content.clone_from(&results);
                    self.history
                        .compact(ExchangeCommit {
                            expected: expected.clone(),
                            edits: vec![CompactionEdit::Replace {
                                index: result_index,
                                message: result_message.clone(),
                            }],
                        })
                        .await?;
                    let entry = &expected[result_index];
                    let mut revisions = entry.revisions().to_vec();
                    revisions.push(result_message.clone());
                    expected[result_index] = HistoryEntry::builder()
                        .source(entry.verbatim().clone())
                        .revisions(revisions)
                        .build();
                }
            }
            run.emit(AgentEvent::StepFinished {
                message: response.message.clone(),
                usage: response.usage,
                stop_reason: response.stop_reason.clone(),
            })
            .await;
            last_response = Some(stored);
            if let Some(error) = failure {
                return Err(error);
            }
            let terminal = match response.stop_reason.clone() {
                Some(StopReason::MaxTokens) => Some(RunStatus::MaxTokens),
                Some(StopReason::PauseTurn) => Some(RunStatus::Paused),
                Some(StopReason::Refusal) => Some(RunStatus::Refused),
                Some(StopReason::ContextWindowExceeded) => Some(RunStatus::ContextWindowExceeded),
                _ if calls.is_empty() => Some(RunStatus::Completed),
                _ => None,
            };
            let boundary = self.history.snapshot().await;
            let outcome = StepOutcome {
                step: step.into(),
                response: response.message,
                stop_reason: response.stop_reason,
                terminal,
            };
            let action = match &self.layers {
                Some(layers) => {
                    run.cancellation
                        .race(layers.step_end(self, run, &outcome))
                        .await?
                }
                None => StepAction::default(),
            };
            if action.continue_run
                && let Some(status @ (RunStatus::Refused | RunStatus::ContextWindowExceeded)) =
                    terminal
            {
                return Err(error::UnsupportedContinuationSnafu { status }.build());
            }
            if !action.messages.is_empty() {
                self.append_step_messages(run, step.into(), &boundary, action.messages)
                    .await?;
            }
            if !action.continue_run
                && let Some(status) = terminal
            {
                return Ok(RunReport {
                    id: run.id,
                    status,
                    response: last_response,
                    stats: run.stats.snapshot().await,
                    tool_failures: run.tool_failures().await,
                    interrupted_attempts: run.interrupted_attempts().await,
                });
            }
        }
        Ok(RunReport {
            id: run.id,
            status: RunStatus::StepLimit,
            response: last_response,
            stats: run.stats.snapshot().await,
            tool_failures: run.tool_failures().await,
            interrupted_attempts: run.interrupted_attempts().await,
        })
    }

    /// Validates and atomically appends step-boundary messages against the
    /// post-step snapshot; any other edit in the window fails unchanged.
    async fn append_step_messages(
        &self,
        run: &RunContext,
        step: u64,
        boundary: &HistorySnapshot,
        messages: Vec<StoredMessage>,
    ) -> Result<()> {
        for message in &messages {
            let content = message.message();
            if content.role != Role::User {
                return Err(error::InvalidRoleSnafu {
                    expected: Role::User,
                    actual: content.role.clone(),
                }
                .build());
            }
            if content.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
                )
            }) {
                return Err(error::ToolInputMessageSnafu.build());
            }
        }
        let definitions = self.tools.definitions();
        let mut view: Vec<_> = boundary
            .entries()
            .iter()
            .filter_map(HistoryEntry::current)
            .map(|stored| stored.message().clone())
            .collect();
        view.extend(messages.iter().map(|stored| stored.message().clone()));
        crate::protocol::validate_replay(&view, &definitions)?;
        let edits = messages
            .iter()
            .map(|message| CompactionEdit::Insert {
                before: boundary.entries().len(),
                message: message.clone(),
            })
            .collect();
        self.history
            .compact(ExchangeCommit {
                expected: boundary.entries().to_vec(),
                edits,
            })
            .await?;
        run.emit(AgentEvent::MessagesAppended {
            run_id: run.id,
            step,
            messages,
        })
        .await;
        Ok(())
    }

    async fn stream(
        &self,
        run: &RunContext,
        step: u64,
        request: &StepRequest,
    ) -> Result<StreamResponse> {
        let attempt = run.stats.attempts.fetch_add(1, Ordering::Relaxed);
        let mut exchange = InferenceExchange::new(|tx| async move {
            request
                .model
                .provider
                .stream_message(
                    &request.model,
                    &request.messages,
                    &request.system,
                    &request.tools,
                    &tx,
                    request.options.clone(),
                )
                .await
        });
        let failure = loop {
            let event = tokio::select! {
                biased;
                () = run.cancellation.cancelled() => break None,
                event = exchange.next() => event,
            };
            match event {
                Some(ExchangeEvent::Provider(event)) => {
                    run.emit(AgentEvent::Provider {
                        run_id: run.id,
                        step,
                        attempt,
                        event,
                    })
                    .await;
                }
                Some(ExchangeEvent::Finished(Ok(response))) => {
                    run.stats
                        .record(&request.model, &response.usage, true)
                        .await;
                    return Ok(response);
                }
                Some(ExchangeEvent::Finished(Err(error))) => break Some(error),
                None => break None,
            }
        };
        let partial = exchange.cancel().clone();
        if let Some(usage) = &partial.usage {
            run.stats.record(&request.model, usage, false).await;
        } else {
            run.stats
                .unreported_incomplete_attempts
                .fetch_add(1, Ordering::Relaxed);
        }
        let interrupted = std::sync::Arc::new(InterruptedAttempt {
            run_id: run.id,
            step,
            attempt,
            provider: request.model.provider.name().to_owned(),
            model: request.model.id.clone(),
            cancelled: failure.is_none(),
            partial,
        });
        run.interrupted_attempts
            .lock()
            .await
            .push(std::sync::Arc::clone(&interrupted));
        run.emit(AgentEvent::AttemptInterrupted {
            run_id: run.id,
            step,
            attempt,
        })
        .await;
        match failure {
            Some(source) => Err(AgentError::Inference {
                source,
                attempt: interrupted,
            }),
            None => Err(error::CancelledSnafu.build()),
        }
    }

    /// Claims for the maximal bounded conflict-free wave starting at `start`.
    ///
    /// The wave stays contiguous in call order; a conflicting call is a
    /// barrier, so later independent calls never overtake it.
    fn wave_claims(&self, calls: &[ToolCall], start: usize) -> Vec<ToolScheduling> {
        let mut claims = Vec::new();
        let mut cursor = start;
        while cursor < calls.len() && claims.len() < self.config.max_concurrent_tools {
            let call = &calls[cursor];
            let declared = self
                .tools
                .get(&call.name)
                .map(|tool| tool.scheduling(&call.input))
                .unwrap_or_default();
            let claim = match &self.tool_scheduling_policy {
                Some(policy) => policy.scheduling(call, declared),
                None => declared,
            };
            if claims.iter().any(|previous| claim.conflicts(previous)) {
                break;
            }
            claims.push(claim);
            cursor += 1;
        }
        claims
    }

    /// Runs one parent's child calls in bounded conflict-free waves.
    ///
    /// Claims only partition waves; each child executes under the parent's
    /// inherited exclusive reservation. The first fatal child error drops
    /// in-flight siblings and prevents later waves.
    pub(crate) async fn execute_child_waves(
        &self,
        run: &RunContext,
        calls: &[ToolCall],
        parent_call_id: &str,
        depth: u32,
        visible: &[crate::provider::Tool],
    ) -> Result<Vec<ToolOutput>> {
        let mut outputs: Vec<Option<ToolOutput>> = (0..calls.len()).map(|_| None).collect();
        let mut cursor = 0;
        while cursor < calls.len() {
            let start = cursor;
            cursor += self.wave_claims(calls, cursor).len();
            let mut running: FuturesUnordered<_> = calls[start..cursor]
                .iter()
                .enumerate()
                .map(|(offset, call)| async move {
                    let output = self
                        .execute_tool(
                            run,
                            call,
                            Some(parent_call_id),
                            depth,
                            visible,
                            ToolScheduling::Exclusive,
                        )
                        .await;
                    (start + offset, output)
                })
                .collect();
            while let Some((index, output)) = running.next().await {
                match output {
                    Ok(output) => outputs[index] = Some(output),
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(outputs
            .into_iter()
            .map(|output| output.expect("every wave runs to completion before the next starts"))
            .collect())
    }

    pub(crate) async fn execute_tool(
        &self,
        run: &RunContext,
        call: &ToolCall,
        parent_call_id: Option<&str>,
        depth: u32,
        visible: &[crate::provider::Tool],
        scheduling: ToolScheduling,
    ) -> Result<ToolOutput> {
        tokio::select! {
            biased;
            () = run.cancellation.cancelled() => Err(error::CancelledSnafu.build()),
            () = run.tool_halt.cancelled() => {
                match run.stopped.lock().await.clone() {
                    Some(reason) => Err(error::StoppedSnafu { reason }.build()),
                    None => Err(error::CancelledSnafu.build()),
                }
            }
            result = self.execute_tool_inner(run, call, parent_call_id, depth, visible, scheduling) => result,
        }
    }

    async fn execute_tool_inner(
        &self,
        run: &RunContext,
        call: &ToolCall,
        parent_call_id: Option<&str>,
        depth: u32,
        visible: &[crate::provider::Tool],
        scheduling: ToolScheduling,
    ) -> Result<ToolOutput> {
        if depth >= self.config.max_tool_depth {
            return Err(error::ToolDepthExceededSnafu.build());
        }
        if run.cancellation.is_cancelled() {
            return Err(error::CancelledSnafu.build());
        }
        if let Some(reason) = run.stopped.lock().await.clone() {
            return Err(error::StoppedSnafu { reason }.build());
        }
        if run
            .tool_dispatches
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count
                    .checked_add(1)
                    .filter(|next| *next <= self.config.max_tool_calls)
            })
            .is_err()
        {
            return Err(error::ToolCallLimitExceededSnafu.build());
        }
        let context = ToolContext {
            run,
            call,
            parent_call_id,
            agent: self,
            depth,
            visible,
            scheduling,
            children: AtomicU64::default(),
            child_gate: Mutex::default(),
        };
        let decision = match &self.layers {
            Some(layers) => tokio::select! {
                biased;
                () = run.cancellation.cancelled() => return Err(error::CancelledSnafu.build()),
                decision = layers.pre_tool_exec(self, &context) => decision?,
            },
            None => ToolDecision::Run,
        };
        let output = match decision {
            ToolDecision::Stop { reason } => {
                *run.stopped.lock().await = Some(reason.clone());
                run.tool_halt.cancel();
                return Err(error::StoppedSnafu { reason }.build());
            }
            ToolDecision::Deny { reason } => {
                ToolOutput::builder().content(reason).is_error(true).build()
            }
            ToolDecision::Run => match self
                .tools
                .get(&call.name)
                .filter(|_| visible.iter().any(|tool| tool.name == call.name))
            {
                None => ToolOutput::builder()
                    .content(format!("Unknown tool: {}", call.name))
                    .is_error(true)
                    .build(),
                Some(tool) => {
                    run.stats.tool_calls.fetch_add(1, Ordering::Relaxed);
                    run.emit(AgentEvent::ToolStarted {
                        call: call.clone(),
                        parent_call_id: parent_call_id.map(str::to_owned),
                    })
                    .await;
                    let result = tokio::select! {
                        biased;
                        () = run.cancellation.cancelled() => return Err(error::CancelledSnafu.build()),
                        result = async {
                            match self.config.tool_timeout_ms {
                                Some(ms) => match tokio::time::timeout(
                                    std::time::Duration::from_millis(ms),
                                    tool.execute(&context, call.input.clone()),
                                )
                                .await
                                {
                                    Ok(result) => Ok(result),
                                    Err(_) => Err(error::ToolTimeoutSnafu {
                                        name: call.name.clone(),
                                    }
                                    .build()),
                                },
                                None => Ok(tool.execute(&context, call.input.clone()).await),
                            }
                        } => result?,
                    };
                    match result {
                        Ok(output) => output,
                        Err(error) => {
                            let output = ToolOutput::builder()
                                .content(error.to_string())
                                .is_error(true)
                                .build();
                            let error = std::sync::Arc::new(error);
                            run.tool_failures.lock().await.push(ToolFailure {
                                call: call.clone(),
                                error: std::sync::Arc::clone(&error),
                            });
                            run.emit(AgentEvent::ToolFailed {
                                call: call.clone(),
                                error,
                            })
                            .await;
                            output
                        }
                    }
                }
            },
        };
        if let Some(reason) = run.stopped.lock().await.clone() {
            return Err(error::StoppedSnafu { reason }.build());
        }
        let output = match &self.layers {
            Some(layers) => {
                run.cancellation
                    .race(layers.post_tool_exec(self, &context, output))
                    .await?
            }
            None => output,
        };
        run.emit(AgentEvent::ToolFinished {
            call: call.clone(),
            output: output.clone(),
        })
        .await;
        Ok(output)
    }
}

fn validate_tool_subset(
    tools: &[crate::provider::Tool],
    available: &[crate::provider::Tool],
) -> Result<()> {
    let mut names = BTreeSet::new();
    for tool in tools {
        if !names.insert(&tool.name)
            || !available.iter().any(|original| {
                original.name == tool.name
                    && original.description == tool.description
                    && original.input_schema == tool.input_schema
            })
        {
            return Err(error::ToolDeclarationsChangedSnafu.build());
        }
    }
    Ok(())
}

struct ExchangeCommit {
    expected: Vec<HistoryEntry>,
    edits: Vec<CompactionEdit>,
}

#[async_trait]
impl Compactor for ExchangeCommit {
    async fn call(
        &self,
        history: &HistorySnapshot,
    ) -> std::result::Result<CompactionPlan, crate::Error> {
        if history.entries() != self.expected {
            return Err(error::HistoryChangedSnafu {
                messages: self
                    .edits
                    .iter()
                    .filter_map(|edit| match edit {
                        CompactionEdit::Insert { message, .. }
                        | CompactionEdit::Replace { message, .. } => Some(message.clone()),
                        CompactionEdit::Remove { .. } => None,
                    })
                    .collect::<Vec<_>>(),
            }
            .build()
            .into());
        }
        Ok(CompactionPlan::builder().edits(self.edits.clone()).build())
    }
}
