use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;

use crate::{Struct, history::StoredMessage, provider::StreamResponse};

use super::{
    Agent, Result, RunContext, RunStatus, StepOutcome, StepRequest,
    tool::{ToolContext, ToolOutput},
};

/// Authorization outcome for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecision {
    /// Execute the call through the remaining layers.
    Run,
    /// Refuse the call; the model receives the reason as an error result.
    Deny { reason: String },
    /// Terminate the run; unfinished calls close as not executed.
    Stop { reason: String },
    /// Complete the call with this output; the executor never runs.
    /// Admitted against `max_tool_calls`, but not counted as executed.
    Respond { output: ToolOutput },
}

/// Layer response to an inference failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Propagate the failure to the caller.
    Propagate,
    /// Retry inference within the run's retry budget.
    Retry,
}

/// Layer decision at a step boundary.
#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct StepAction {
    /// Durable user messages to append before the next inference attempt.
    #[builder(default)]
    pub messages: Vec<StoredMessage>,
    /// Continue the run when the step would otherwise terminate it.
    #[builder(default)]
    pub continue_run: bool,
}

/// Hooks run head-to-tail; transformations consume the preceding result.
#[arc_handle]
#[async_trait]
pub trait Layer {
    /// Runs once at first use; failure rolls back initialized nodes.
    async fn startup(&self, _agent: &Agent) -> Result<()> {
        Ok(())
    }

    /// Transforms the submitted input before it is recorded.
    async fn turn_start(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        message: &StoredMessage,
    ) -> Result<StoredMessage> {
        Ok(message.clone())
    }

    /// Transforms each inference request, including retries.
    async fn prepare_request(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        request: StepRequest,
    ) -> Result<StepRequest> {
        Ok(request)
    }

    /// Transforms a fully assembled response before it is recorded.
    async fn response(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        response: StreamResponse,
    ) -> Result<StreamResponse> {
        Ok(response)
    }

    /// Authorizes one call; denial short-circuits later layers.
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
    ) -> Result<ToolDecision> {
        Ok(ToolDecision::Run)
    }

    /// Transforms the model-visible outcome, including denied calls.
    async fn post_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
        output: ToolOutput,
    ) -> Result<ToolOutput> {
        Ok(output)
    }

    /// Decides whether a failed inference attempt is retried.
    async fn recover(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _error: &super::AgentError,
    ) -> Result<Recovery> {
        Ok(Recovery::Propagate)
    }

    /// Runs after a step's results commit, before termination is decided.
    /// Returns messages to append durably and whether to continue the run.
    async fn step_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _outcome: &StepOutcome,
    ) -> Result<StepAction> {
        Ok(StepAction::default())
    }

    /// Observes the terminal status of every admitted run.
    async fn turn_end(&self, _agent: &Agent, _run: &RunContext, _status: RunStatus) -> Result<()> {
        Ok(())
    }

    /// Releases resources; runs once, after the last run completes.
    async fn shutdown(&self, _agent: &Agent) -> Result<()> {
        Ok(())
    }
}

/// One linked-list node pairing a layer handle with the remaining chain.
#[derive(Builder)]
#[builder(on(_, into))]
pub struct AgentLayer {
    pub layer: Layer,
    pub next: Option<Box<AgentLayer>>,
}

impl std::fmt::Debug for AgentLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLayer").finish_non_exhaustive()
    }
}

#[async_trait]
impl LayerImpl for AgentLayer {
    async fn startup(&self, agent: &Agent) -> Result<()> {
        if let Err(error) = self.layer.startup(agent).await {
            return match self.layer.shutdown(agent).await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(super::AgentError::Finalization {
                    source: Box::new(error),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        if let Some(next) = self.next.as_ref()
            && let Err(error) = next.startup(agent).await
        {
            return match self.layer.shutdown(agent).await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(super::AgentError::Finalization {
                    source: Box::new(error),
                    cleanup: Box::new(cleanup),
                }),
            };
        }
        Ok(())
    }

    async fn turn_start(
        &self,
        agent: &Agent,
        run: &RunContext,
        message: &StoredMessage,
    ) -> Result<StoredMessage> {
        let msg = self.layer.turn_start(agent, run, message).await?;
        match self.next.as_ref() {
            Some(next) => next.turn_start(agent, run, &msg).await,
            None => Ok(msg),
        }
    }

    async fn prepare_request(
        &self,
        agent: &Agent,
        run: &RunContext,
        request: StepRequest,
    ) -> Result<StepRequest> {
        let available = request.tools.clone();
        let request = self.layer.prepare_request(agent, run, request).await?;
        super::validate_tool_subset(&request.tools, &available)?;
        match &self.next {
            Some(next) => next.prepare_request(agent, run, request).await,
            None => Ok(request),
        }
    }

    async fn response(
        &self,
        agent: &Agent,
        run: &RunContext,
        response: StreamResponse,
    ) -> Result<StreamResponse> {
        let response = self.layer.response(agent, run, response).await?;
        match &self.next {
            Some(next) => next.response(agent, run, response).await,
            None => Ok(response),
        }
    }

    async fn pre_tool_exec(
        &self,
        agent: &Agent,
        context: &ToolContext<'_>,
    ) -> Result<ToolDecision> {
        match self.layer.pre_tool_exec(agent, context).await? {
            ToolDecision::Run => match &self.next {
                Some(next) => next.pre_tool_exec(agent, context).await,
                None => Ok(ToolDecision::Run),
            },
            decision => Ok(decision),
        }
    }

    async fn post_tool_exec(
        &self,
        agent: &Agent,
        context: &ToolContext<'_>,
        output: ToolOutput,
    ) -> Result<ToolOutput> {
        let output = self.layer.post_tool_exec(agent, context, output).await?;
        match &self.next {
            Some(next) => next.post_tool_exec(agent, context, output).await,
            None => Ok(output),
        }
    }

    async fn recover(
        &self,
        agent: &Agent,
        run: &RunContext,
        error: &super::AgentError,
    ) -> Result<Recovery> {
        match self.layer.recover(agent, run, error).await? {
            Recovery::Propagate => match &self.next {
                Some(next) => next.recover(agent, run, error).await,
                None => Ok(Recovery::Propagate),
            },
            Recovery::Retry => Ok(Recovery::Retry),
        }
    }

    async fn step_end(
        &self,
        agent: &Agent,
        run: &RunContext,
        outcome: &StepOutcome,
    ) -> Result<StepAction> {
        let head = self.layer.step_end(agent, run, outcome).await?;
        let Some(next) = self.next.as_ref() else {
            return Ok(head);
        };
        let tail = next.step_end(agent, run, outcome).await?;
        let mut messages = head.messages;
        messages.extend(tail.messages);
        Ok(StepAction::builder()
            .messages(messages)
            .continue_run(head.continue_run || tail.continue_run)
            .build())
    }

    async fn turn_end(&self, agent: &Agent, run: &RunContext, status: RunStatus) -> Result<()> {
        let result = self.layer.turn_end(agent, run, status).await;
        let rest = match &self.next {
            Some(next) => next.turn_end(agent, run, status).await,
            None => Ok(()),
        };
        result.and(rest)
    }

    async fn shutdown(&self, agent: &Agent) -> Result<()> {
        let result = self.layer.shutdown(agent).await;
        let rest = match &self.next {
            Some(next) => next.shutdown(agent).await,
            None => Ok(()),
        };
        result.and(rest)
    }
}
