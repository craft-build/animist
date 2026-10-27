use crate::agent::{Agent, Result};
use crate::history::Message;
use crate::history::StoredMessage;
use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use jiff::Timestamp;

#[arc_handle]
#[async_trait]
pub trait Layer {
    async fn startup(&self, agent: &Agent) -> Result<()> {
        Ok(())
    }

    async fn turn_start(&self, agent: &Agent, message: &StoredMessage) -> Result<StoredMessage> {
        Ok(message.clone())
    }

    async fn pre_tool_exec(&self, agent: &Agent, message: &StoredMessage) -> Result<()> {
        Ok(())
    }

    async fn post_tool_exec(&self, agent: &Agent, message: &StoredMessage) -> Result<()> {
        Ok(())
    }

    async fn turn_end(&self, agent: &Agent, message: &StoredMessage) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self, agent: &Agent) -> Result<()> {
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
        self.layer.startup(agent).await?;
        if let Some(next) = self.next.as_ref() {
            next.startup(agent).await?;
        }
        Ok(())
    }

    async fn turn_start(&self, agent: &Agent, message: &StoredMessage) -> Result<StoredMessage> {
        let msg = self.layer.turn_start(agent, message).await?;
        match self.next.as_ref() {
            Some(next) => next.turn_start(agent, &msg).await,
            None => Ok(msg),
        }
    }

    async fn pre_tool_exec(&self, agent: &Agent, message: &StoredMessage) -> Result<()> {
        self.layer.pre_tool_exec(agent, message).await?;
        if let Some(next) = self.next.as_ref() {
            next.pre_tool_exec(agent, message).await?;
        }
        Ok(())
    }

    async fn post_tool_exec(&self, agent: &Agent, message: &StoredMessage) -> Result<()> {
        self.layer.post_tool_exec(agent, message).await?;
        if let Some(next) = self.next.as_ref() {
            next.post_tool_exec(agent, message).await?;
        }
        Ok(())
    }

    async fn turn_end(&self, agent: &Agent, message: &StoredMessage) -> Result<()> {
        self.layer.turn_end(agent, message).await?;
        if let Some(next) = self.next.as_ref() {
            next.post_tool_exec(agent, message).await?;
        }
        Ok(())
    }

    async fn shutdown(&self, agent: &Agent) -> Result<()> {
        self.layer.shutdown(agent).await?;
        if let Some(next) = self.next.as_ref() {
            next.shutdown(agent).await?;
        }
        Ok(())
    }
}
