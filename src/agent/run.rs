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

/// Monotonic cooperative cancellation; dropping a clone does not cancel.
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
    pub fn cancel(&self) {
        self.state.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.state.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.state.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    pub async fn race<T>(
        &self,
        future: impl std::future::Future<Output = super::Result<T>>,
    ) -> super::Result<T> {
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(super::AgentError::Cancelled),
            result = future => result,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Completed,
    StepLimit,
    Cancelled,
    Stopped,
    MaxTokens,
    Paused,
    Refused,
    ContextWindowExceeded,
    Failed,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Provider {
        run_id: u64,
        step: u64,
        attempt: u64,
        event: ProviderEvent,
    },
    ToolStarted {
        call: super::tool::ToolCall,
        parent_call_id: Option<String>,
    },
    ToolProgress {
        id: String,
        content: String,
    },
    ToolFinished {
        call: super::tool::ToolCall,
        output: super::tool::ToolOutput,
    },
    ToolFailed {
        call: super::tool::ToolCall,
        error: Arc<super::tool::ToolError>,
    },
    StepFinished {
        message: Message,
        usage: TokenUsage,
        stop_reason: Option<StopReason>,
    },
    Retrying {
        attempt: u64,
    },
    AttemptInterrupted {
        run_id: u64,
        step: u64,
        attempt: u64,
    },
    Finished {
        run_id: u64,
        status: RunStatus,
    },
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
    unpriced_steps: u64,
    unpriced_attempts: u64,
    by_model: BTreeMap<(String, String), TokenUsage>,
}

#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub steps: u64,
    pub attempts: u64,
    pub tool_calls: u64,
    pub context_size: u64,
    pub usage: TokenUsage,
    pub unpriced_steps: u64,
    pub unpriced_attempts: u64,
    pub reported_incomplete_attempts: u64,
    pub unreported_incomplete_attempts: u64,
    pub by_model: BTreeMap<(String, String), TokenUsage>,
}

impl RunStats {
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

#[derive(Clone, Default, Builder)]
#[builder(on(_, into))]
pub struct RunOptions {
    #[builder(default)]
    pub cancellation: Cancellation,
    pub events: Option<Sender<AgentEvent>>,
    #[builder(default)]
    pub request: RequestOptions,
}

pub struct RunContext {
    pub id: u64,
    pub cancellation: Cancellation,
    pub stats: Arc<RunStats>,
    pub(crate) events: Option<Sender<AgentEvent>>,
    pub(crate) stopped: Mutex<Option<String>>,
    pub(crate) tool_halt: Cancellation,
    pub(crate) tool_dispatches: AtomicU64,
    pub(crate) tool_failures: Mutex<Vec<ToolFailure>>,
    pub(crate) interrupted_attempts: Mutex<Vec<Arc<InterruptedAttempt>>>,
}

impl RunContext {
    pub async fn interrupted_attempts(&self) -> Vec<Arc<InterruptedAttempt>> {
        self.interrupted_attempts.lock().await.clone()
    }

    pub async fn tool_failures(&self) -> Vec<ToolFailure> {
        self.tool_failures.lock().await.clone()
    }

    /// Event delivery is lossless while connected, with cancellation-aware backpressure.
    pub async fn emit(&self, event: AgentEvent) {
        if let Some(events) = &self.events {
            tokio::select! {
                biased;
                result = events.send_async(event) => { let _ = result; }
                _ = self.cancellation.cancelled() => {}
            }
        }
    }
}

#[derive(Clone, Builder)]
#[builder(on(_, into))]
pub struct StepRequest {
    pub model: Model,
    pub messages: Vec<Message>,
    pub system: String,
    pub tools: Vec<Tool>,
    pub options: RequestOptions,
}

#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub call: super::tool::ToolCall,
    pub error: Arc<super::tool::ToolError>,
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub id: u64,
    pub status: RunStatus,
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
