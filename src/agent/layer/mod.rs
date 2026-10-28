use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;

use crate::{history::StoredMessage, provider::StreamResponse};

use super::{
    Agent, Result, RunContext, RunStatus, StepRequest,
    tool::{ToolContext, ToolOutput},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecision {
    Run,
    Deny { reason: String },
    Stop { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Propagate,
    Retry,
}

/// Hooks run head-to-tail; transformations consume the preceding result.
#[arc_handle]
#[async_trait]
pub trait Layer {
    async fn startup(&self, _agent: &Agent) -> Result<()> {
        Ok(())
    }

    async fn turn_start(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        message: &StoredMessage,
    ) -> Result<StoredMessage> {
        Ok(message.clone())
    }

    async fn prepare_request(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        request: StepRequest,
    ) -> Result<StepRequest> {
        Ok(request)
    }

    async fn response(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        response: StreamResponse,
    ) -> Result<StreamResponse> {
        Ok(response)
    }

    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
    ) -> Result<ToolDecision> {
        Ok(ToolDecision::Run)
    }

    async fn post_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
        output: ToolOutput,
    ) -> Result<ToolOutput> {
        Ok(output)
    }

    async fn recover(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _error: &super::AgentError,
    ) -> Result<Recovery> {
        Ok(Recovery::Propagate)
    }

    async fn turn_end(&self, _agent: &Agent, _run: &RunContext, _status: RunStatus) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self, _agent: &Agent) -> Result<()> {
        Ok(())
    }
}

#[derive(Builder)]
#[builder(on(_, into))]
pub struct AgentLayer {
    pub layer: Layer,
    pub next: Option<Box<AgentLayer>>,
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
            recovery => Ok(recovery),
        }
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
