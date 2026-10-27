mod config;
mod error;
mod layer;
mod prompt;
mod run;
pub mod tool;

pub use config::AgentConfig;
pub use error::AgentError;
pub use layer::{AgentLayer, Layer, LayerImpl, Recovery, ToolDecision};
pub use prompt::{Prompt, Section};
pub use run::{
    AgentEvent, Cancellation, RunContext, RunOptions, RunReport, RunStats, RunStatus,
    StatsSnapshot, StepRequest, ToolFailure,
};

use crate::{
    history::{
        CompactionEdit, CompactionPlan, Compactor, ContentBlock, History, HistoryEntry,
        HistorySnapshot, Message, Role, StoredMessage,
    },
    provider::{Model, ProviderEvent, StopReason, StreamResponse},
};
use async_trait::async_trait;
use bon::Builder;
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::{Mutex, OnceCell};
use tool::{ToolCall, ToolContext, ToolOutput, ToolRegistry};

pub type Result<T> = std::result::Result<T, AgentError>;

#[derive(Builder)]
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
    layers: Option<AgentLayer>,
    #[builder(skip)]
    gate: Mutex<bool>,
    #[builder(skip)]
    started: OnceCell<()>,
    #[builder(skip)]
    next_run: AtomicU64,
}

impl Agent {
    pub fn history(&self) -> History {
        self.history.clone()
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Startup is lazy and runs once. Shutdown is explicit and terminal.
    pub async fn shutdown(&self) -> Result<()> {
        let mut closed = self
            .gate
            .try_lock()
            .map_err(|_| AgentError::AlreadyRunning)?;
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

    /// Cancel through RunOptions and await this future to finalize tool history.
    /// Dropping it leaves explicit interrupted results for unfinished tools.
    pub async fn run(&self, input: StoredMessage, options: RunOptions) -> Result<RunReport> {
        let closed = self
            .gate
            .try_lock()
            .map_err(|_| AgentError::AlreadyRunning)?;
        if *closed {
            return Err(AgentError::ShutDown);
        }
        if self.config.max_steps == 0 || self.config.max_tool_depth == 0 {
            return Err(AgentError::InvalidLimits);
        }
        let run = RunContext {
            id: self.next_run.fetch_add(1, Ordering::Relaxed),
            cancellation: options.cancellation,
            stats: Default::default(),
            events: options.events,
            stopped: Mutex::new(None),
            tool_failures: Mutex::new(Vec::new()),
        };
        let result = self.run_inner(input, options.request, &run).await;
        let status = match &result {
            Ok(report) => report.status,
            Err(AgentError::Cancelled) => RunStatus::Cancelled,
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
            Err(AgentError::Cancelled) => {
                cleanup?;
                Ok(RunReport {
                    id: run.id,
                    status,
                    response: None,
                    stats: run.stats.snapshot().await,
                    tool_failures: run.tool_failures().await,
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
            return Err(AgentError::Cancelled);
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
            return Err(AgentError::InvalidRole {
                expected: Role::User,
                actual: input.message().role.clone(),
            });
        }
        if input.message().content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
            )
        }) {
            return Err(AgentError::ToolInputMessage);
        }
        if run.cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        self.history.add_stored(input).await;
        let mut last_response = None;
        let mut retries = 0;
        for step in 0..self.config.max_steps {
            let (response, expected) = loop {
                let request = StepRequest::builder()
                    .model(self.model.clone())
                    .messages(self.history.current().await)
                    .system(self.prompt.to_markdown())
                    .tools(self.tools.definitions())
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
                let definitions = self.tools.definitions();
                if request.tools.len() != definitions.len()
                    || request.tools.iter().zip(&definitions).any(|(a, b)| {
                        a.name != b.name
                            || a.description != b.description
                            || a.input_schema != b.input_schema
                    })
                {
                    return Err(AgentError::ToolDeclarationsChanged);
                }
                let expected = self.history.snapshot().await;
                match self.stream(run, step.into(), &request).await {
                    Ok(response) => break (response, expected),
                    Err(error @ AgentError::Provider { .. })
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
                            attempt: retries.into(),
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
                return Err(AgentError::InvalidRole {
                    expected: Role::Assistant,
                    actual: response.message.role,
                });
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
                    return Err(AgentError::AssistantToolResult);
                }
                if let ContentBlock::ToolUse {
                    id, name, input, ..
                } = block
                {
                    if id.is_empty() || !ids.insert(id.clone()) {
                        return Err(AgentError::InvalidToolCallId { id: id.clone() });
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
                return Err(AgentError::MissingToolCalls);
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
            for (index, call) in calls.iter().enumerate() {
                let expected = self.history.snapshot().await;
                if expected
                    .entries()
                    .get(result_index)
                    .and_then(HistoryEntry::current)
                    != Some(&result_message)
                {
                    return Err(AgentError::HistoryChanged {
                        messages: vec![result_message],
                    });
                }
                let output = if matches!(
                    response.stop_reason,
                    Some(
                        StopReason::MaxTokens
                            | StopReason::PauseTurn
                            | StopReason::Refusal
                            | StopReason::ContextWindowExceeded
                    )
                ) {
                    ToolOutput::builder()
                        .content("Not executed because generation did not finish")
                        .is_error(true)
                        .build()
                } else if failure.is_some() {
                    ToolOutput::builder()
                        .content("Not executed because the run stopped")
                        .is_error(true)
                        .build()
                } else {
                    match self.execute_tool(run, call, None, 0).await {
                        Ok(output) => output,
                        Err(error) => {
                            let output = ToolOutput::builder()
                                .content(error.to_string())
                                .is_error(true)
                                .build();
                            failure = Some(error);
                            output
                        }
                    }
                };
                results[index] = ContentBlock::ToolResult {
                    tool_use_id: call.id.clone(),
                    content: output.content,
                    is_error: output.is_error,
                    loaded_tools: Vec::new(),
                };
                result_message.message_mut().content = results.clone();
                self.history
                    .compact(ExchangeCommit {
                        expected: expected.entries().to_vec(),
                        edits: vec![CompactionEdit::Replace {
                            index: result_index,
                            message: result_message.clone(),
                        }],
                    })
                    .await?;
            }
            run.emit(AgentEvent::StepFinished {
                message: response.message,
                usage: response.usage,
                stop_reason: response.stop_reason.clone(),
            })
            .await;
            last_response = Some(stored);
            if let Some(error) = failure {
                return Err(error);
            }
            let terminal = match response.stop_reason {
                Some(StopReason::MaxTokens) => Some(RunStatus::MaxTokens),
                Some(StopReason::PauseTurn) => Some(RunStatus::Paused),
                Some(StopReason::Refusal) => Some(RunStatus::Refused),
                Some(StopReason::ContextWindowExceeded) => Some(RunStatus::ContextWindowExceeded),
                _ if calls.is_empty() => Some(RunStatus::Completed),
                _ => None,
            };
            if let Some(status) = terminal {
                return Ok(RunReport {
                    id: run.id,
                    status,
                    response: last_response,
                    stats: run.stats.snapshot().await,
                    tool_failures: run.tool_failures().await,
                });
            }
        }
        Ok(RunReport {
            id: run.id,
            status: RunStatus::StepLimit,
            response: last_response,
            stats: run.stats.snapshot().await,
            tool_failures: run.tool_failures().await,
        })
    }

    async fn stream(
        &self,
        run: &RunContext,
        step: u64,
        request: &StepRequest,
    ) -> Result<StreamResponse> {
        let (tx, rx) = flume::bounded::<ProviderEvent>(64);
        let attempt = run.stats.attempts.fetch_add(1, Ordering::Relaxed);
        let future = request.model.provider.stream_message(
            &request.model,
            &request.messages,
            &request.system,
            &request.tools,
            &tx,
            request.options.clone(),
        );
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                _ = run.cancellation.cancelled() => return Err(AgentError::Cancelled),
                event = rx.recv_async() => if let Ok(event) = event {
                    run.emit(AgentEvent::Provider { run_id: run.id, step, attempt, event }).await;
                },
                response = &mut future => {
                    while let Ok(event) = rx.try_recv() {
                        run.emit(AgentEvent::Provider { run_id: run.id, step, attempt, event }).await;
                    }
                    let response = response?;
                    run.stats.record(&request.model, &response.usage).await;
                    return Ok(response);
                }
            }
        }
    }

    pub(crate) async fn execute_tool(
        &self,
        run: &RunContext,
        call: &ToolCall,
        parent_call_id: Option<&str>,
        depth: u32,
    ) -> Result<ToolOutput> {
        if depth >= self.config.max_tool_depth {
            return Err(AgentError::ToolDepthExceeded);
        }
        if run.cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if let Some(reason) = run.stopped.lock().await.clone() {
            return Err(AgentError::Stopped { reason });
        }
        let context = ToolContext {
            run,
            call,
            parent_call_id,
            agent: self,
            depth,
        };
        let decision = match &self.layers {
            Some(layers) => tokio::select! {
                biased;
                _ = run.cancellation.cancelled() => return Err(AgentError::Cancelled),
                decision = layers.pre_tool_exec(self, &context) => decision?,
            },
            None => ToolDecision::Run,
        };
        let output = match decision {
            ToolDecision::Stop { reason } => {
                *run.stopped.lock().await = Some(reason.clone());
                return Err(AgentError::Stopped { reason });
            }
            ToolDecision::Deny { reason } => {
                ToolOutput::builder().content(reason).is_error(true).build()
            }
            ToolDecision::Run => match self.tools.get(&call.name) {
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
                        _ = run.cancellation.cancelled() => return Err(AgentError::Cancelled),
                        result = tool.execute(&context, call.input.clone()) => result,
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
                                error: error.clone(),
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
            return Err(AgentError::Stopped { reason });
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
            return Err(AgentError::HistoryChanged {
                messages: self
                    .edits
                    .iter()
                    .filter_map(|edit| match edit {
                        CompactionEdit::Insert { message, .. }
                        | CompactionEdit::Replace { message, .. } => Some(message.clone()),
                        _ => None,
                    })
                    .collect(),
            }
            .into());
        }
        Ok(CompactionPlan::builder().edits(self.edits.clone()).build())
    }
}
