use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use bon::Builder;
use flume::Sender;
use tokio::sync::{Mutex, watch};

use crate::{
    history::{Message, StoredMessage},
    provider::{Model, ProviderEvent, RequestOptions, StopReason, TokenUsage, Tool},
};

/// Cooperative cancellation shared across one run and its tool calls.
#[derive(Clone, Debug)]
pub struct Cancellation {
    state: watch::Sender<bool>,
}

impl Default for Cancellation {
    fn default() -> Self {
        Self {
            state: watch::channel(false).0,
        }
    }
}

impl Cancellation {
    /// Requests cancellation; idempotent and visible to every clone.
    pub fn cancel(&self) {
        self.state.send_replace(true);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.state.borrow()
    }

    /// Resolves once cancellation is requested.
    pub async fn cancelled(&self) {
        let mut receiver = self.state.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    /// Resolves to `Cancelled` if cancellation beats `future`.
    pub async fn race<T>(
        &self,
        future: impl std::future::Future<Output = super::Result<T>>,
    ) -> super::Result<T> {
        tokio::select! {
            biased;
            () = self.cancelled() => Err(super::error::CancelledSnafu.build()),
            result = future => result,
        }
    }
}

/// Terminal status of one run; only `Completed` is a successful step loop exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// All steps produced responses without tool calls.
    Completed,
    /// `max_steps` was reached with work still pending.
    StepLimit,
    /// Cancellation was requested and observed.
    Cancelled,
    /// A layer stopped the run with a reason.
    Stopped,
    /// Generation hit the output token limit; results closed as not executed.
    MaxTokens,
    /// Provider paused the turn.
    Paused,
    /// Provider refused the request.
    Refused,
    /// Request exceeded the model context window.
    ContextWindowExceeded,
    /// A run error terminated the conversation.
    Failed,
}

/// Runtime observations for one run; not a durable persistence backend.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A streamed provider event with its run/step/attempt identity.
    Provider {
        run_id: u64,
        step: u64,
        attempt: u64,
        event: ProviderEvent,
    },
    /// A tool call began executing.
    ToolStarted {
        call: super::tool::ToolCall,
        parent_call_id: Option<String>,
    },
    /// Progress note emitted by the executing tool.
    ToolProgress { id: String, content: String },
    /// A tool call finished with model-visible output.
    ToolFinished {
        call: super::tool::ToolCall,
        output: super::tool::ToolOutput,
    },
    /// Structured execution failure; the model sees only rendered text.
    ToolFailed {
        call: super::tool::ToolCall,
        error: Arc<super::tool::ToolError>,
    },
    /// One completed model step with its final usage.
    StepFinished {
        message: Message,
        usage: TokenUsage,
        stop_reason: Option<StopReason>,
    },
    /// The next inference attempt after a layer-requested retry.
    Retrying { attempt: u64 },
    /// An abandoned attempt; its output is retained in the run report.
    AttemptInterrupted {
        run_id: u64,
        step: u64,
        attempt: u64,
    },
    /// Terminal status, emitted after layer finalization.
    Finished { run_id: u64, status: RunStatus },
}

/// Counters are atomic; usage snapshots are coherent under a short async lock.
#[derive(Debug, Default)]
pub struct RunStats {
    pub(crate) steps: AtomicU64,
    pub(crate) attempts: AtomicU64,
    pub(crate) tool_calls: AtomicU64,
    pub(crate) context_size: AtomicU64,
    pub(crate) reported_incomplete_attempts: AtomicU64,
    pub(crate) unreported_incomplete_attempts: AtomicU64,
    usage: Mutex<UsageTotals>,
}

#[derive(Debug, Clone, Default)]
struct UsageTotals {
    usage: TokenUsage,
    /// Unpriced attempts keep the cost total a known subtotal, not a full one.
    unpriced_steps: u64,
    unpriced_attempts: u64,
    by_model: BTreeMap<(String, String), TokenUsage>,
}

/// Settled view of run counters and usage totals by model.
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub steps: u64,
    pub attempts: u64,
    pub tool_calls: u64,
    /// Latest reported input size, not a prediction of the next request.
    pub context_size: u64,
    pub usage: TokenUsage,
    /// Completed steps whose reported usage carried no price.
    pub unpriced_steps: u64,
    /// Reported attempts whose usage carried no price.
    pub unpriced_attempts: u64,
    pub reported_incomplete_attempts: u64,
    pub unreported_incomplete_attempts: u64,
    pub by_model: BTreeMap<(String, String), TokenUsage>,
}

impl RunStats {
    /// Coherent counters-plus-usage view; adjacent updates may interleave.
    pub async fn snapshot(&self) -> StatsSnapshot {
        let totals = self.usage.lock().await;
        StatsSnapshot {
            steps: self.steps.load(Ordering::Relaxed),
            attempts: self.attempts.load(Ordering::Relaxed),
            tool_calls: self.tool_calls.load(Ordering::Relaxed),
            context_size: self.context_size.load(Ordering::Relaxed),
            usage: totals.usage.clone(),
            unpriced_steps: totals.unpriced_steps,
            unpriced_attempts: totals.unpriced_attempts,
            reported_incomplete_attempts: self.reported_incomplete_attempts.load(Ordering::Relaxed),
            unreported_incomplete_attempts: self
                .unreported_incomplete_attempts
                .load(Ordering::Relaxed),
            by_model: totals.by_model.clone(),
        }
    }

    pub(crate) async fn record(&self, model: &Model, usage: &TokenUsage, completed: bool) {
        let mut totals = self.usage.lock().await;
        let model_usage = totals
            .by_model
            .entry((model.provider.name().to_owned(), model.id.clone()))
            .or_default();
        model_usage.input = model_usage.input.saturating_add(usage.input);
        model_usage.output = model_usage.output.saturating_add(usage.output);
        model_usage.cache_creation = model_usage
            .cache_creation
            .saturating_add(usage.cache_creation);
        model_usage.cache_read = model_usage.cache_read.saturating_add(usage.cache_read);
        if let Some(cost) = usage.cost {
            model_usage.cost = Some(model_usage.cost.unwrap_or_default() + cost);
        }
        totals.usage.input = totals.usage.input.saturating_add(usage.input);
        totals.usage.output = totals.usage.output.saturating_add(usage.output);
        totals.usage.cache_creation = totals
            .usage
            .cache_creation
            .saturating_add(usage.cache_creation);
        totals.usage.cache_read = totals.usage.cache_read.saturating_add(usage.cache_read);
        if let Some(cost) = usage.cost {
            totals.usage.cost = Some(totals.usage.cost.unwrap_or_default() + cost);
        } else {
            totals.unpriced_attempts += 1;
            if completed {
                totals.unpriced_steps += 1;
            }
        }
        self.context_size
            .store(u64::from(usage.input), Ordering::Relaxed);
        if completed {
            self.steps.fetch_add(1, Ordering::Relaxed);
        } else {
            self.reported_incomplete_attempts
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Per-run options; defaults mirror `RunOptions::default()`.
#[derive(Clone, Debug, Default, Builder)]
#[builder(on(_, into))]
pub struct RunOptions {
    #[builder(default)]
    pub cancellation: Cancellation,
    /// Observer for run events; disconnected observers do not fail the run.
    pub events: Option<Sender<AgentEvent>>,
    #[builder(default)]
    pub request: RequestOptions,
}

/// Shared per-run state: identity, cancellation, statistics, and events.
#[derive(Debug)]
pub struct RunContext {
    /// Zero-based run identity within the agent.
    pub id: u64,
    /// Cancellation shared by the run and its tool calls.
    pub cancellation: Cancellation,
    /// Live counters; prefer `snapshot` for a coherent view.
    pub stats: Arc<RunStats>,
    pub(crate) events: Option<Sender<AgentEvent>>,
    pub(crate) stopped: Mutex<Option<String>>,
    pub(crate) tool_halt: Cancellation,
    pub(crate) tool_dispatches: AtomicU64,
    pub(crate) tool_failures: Mutex<Vec<ToolFailure>>,
    pub(crate) interrupted_attempts: Mutex<Vec<Arc<InterruptedAttempt>>>,
}

impl RunContext {
    /// Interrupted attempts recorded so far, oldest first.
    pub async fn interrupted_attempts(&self) -> Vec<Arc<InterruptedAttempt>> {
        self.interrupted_attempts.lock().await.clone()
    }

    /// Structured tool failures recorded so far, in completion order.
    pub async fn tool_failures(&self) -> Vec<ToolFailure> {
        self.tool_failures.lock().await.clone()
    }

    /// Event delivery is lossless while connected, with cancellation-aware
    /// backpressure. Cancellation may drop events, including `Finished`;
    /// the returned run result stays authoritative.
    pub async fn emit(&self, event: AgentEvent) {
        if let Some(events) = &self.events {
            tokio::select! {
                biased;
                result = events.send_async(event) => { let _ = result; }
                () = self.cancellation.cancelled() => {}
            }
        }
    }
}

/// One inference attempt after layer request transformations.
#[derive(Clone, Debug, Builder)]
#[builder(on(_, into))]
pub struct StepRequest {
    pub model: Model,
    pub messages: Vec<Message>,
    pub system: String,
    pub tools: Vec<Tool>,
    pub options: RequestOptions,
}

/// A structured tool failure retained for the run report.
#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub call: super::tool::ToolCall,
    pub error: Arc<super::tool::ToolError>,
}

/// Terminal state of one run with settled statistics and recovery data.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub id: u64,
    pub status: RunStatus,
    /// Last stored assistant response, when the run produced one.
    pub response: Option<StoredMessage>,
    pub stats: StatsSnapshot,
    pub tool_failures: Vec<ToolFailure>,
    pub interrupted_attempts: Vec<Arc<InterruptedAttempt>>,
}

/// Recovery data, never automatically committed or executed.
#[derive(crate::Struct!)]
pub struct InterruptedAttempt {
    pub run_id: u64,
    pub step: u64,
    pub attempt: u64,
    pub provider: String,
    pub model: String,
    pub cancelled: bool,
    pub partial: crate::provider::PartialResponse,
}
