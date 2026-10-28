use std::{collections::BTreeMap, error::Error};

use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use serde::de::DeserializeOwned;
use serde_json::Value;
use snafu::{ResultExt, Snafu};

use super::{Agent, Result, RunContext};
use crate::Struct;
pub use crate::history::{ToolContent, ToolContents};
pub use crate::provider::Tool as ToolDefinition;

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
}

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct ToolOutput {
    pub content: ToolContents,
    #[builder(default)]
    pub is_error: bool,
    #[builder(default)]
    pub loaded_tools: Vec<String>,
}

pub trait IntoToolOutput: Send {
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

#[async_trait]
pub trait ToolImpl: Send + Sync + 'static {
    type Args: DeserializeOwned + Send;
    type Output: IntoToolOutput;
    type Error: Error + Send + Sync + 'static;

    fn definition(&self) -> ToolDefinition;

    async fn call(
        &self,
        context: &ToolContext<'_>,
        args: Self::Args,
    ) -> std::result::Result<Self::Output, Self::Error>;
}

#[arc_handle]
#[async_trait]
pub trait ToolExecutor {
    fn definition(&self) -> ToolDefinition;

    async fn execute(
        &self,
        context: &ToolContext<'_>,
        input: Value,
    ) -> std::result::Result<ToolOutput, ToolError>;
}

struct TypedTool<T>(T);

#[async_trait]
impl<T: ToolImpl> ToolExecutorImpl for TypedTool<T> {
    fn definition(&self) -> ToolDefinition {
        self.0.definition()
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
            .map_err(|source| ToolError::Execution {
                source: Box::new(source),
            })?
            .into_tool_output()
    }
}

/// Registration freezes each declaration alongside its executable handle.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, (ToolDefinition, ToolExecutor)>,
}

impl ToolRegistry {
    pub fn register<T: ToolImpl>(&mut self, tool: T) -> Result<()> {
        self.register_executor(ToolExecutor::new(TypedTool(tool)))
    }

    pub fn register_executor(&mut self, tool: ToolExecutor) -> Result<()> {
        let definition = tool.definition();
        if definition.name.is_empty() {
            return Err(super::AgentError::EmptyToolName);
        }
        if self.tools.contains_key(&definition.name) {
            return Err(super::AgentError::DuplicateTool {
                name: definition.name,
            });
        }
        self.tools
            .insert(definition.name.clone(), (definition, tool));
        Ok(())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|(d, _)| d.clone()).collect()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&ToolExecutor> {
        self.tools.get(name).map(|(_, tool)| tool)
    }
}

/// Child calls use the same authorization pipeline and bounded nesting.
pub struct ToolContext<'a> {
    pub run: &'a RunContext,
    pub call: &'a ToolCall,
    pub parent_call_id: Option<&'a str>,
    pub(crate) agent: &'a Agent,
    pub(crate) depth: u32,
}

impl ToolContext<'_> {
    pub async fn dispatch(&self, call: ToolCall) -> Result<ToolOutput> {
        self.agent
            .execute_tool(self.run, &call, Some(&self.call.id), self.depth + 1)
            .await
    }

    pub async fn progress(&self, content: impl Into<String>) {
        self.run
            .emit(super::AgentEvent::ToolProgress {
                id: self.call.id.clone(),
                content: content.into(),
            })
            .await;
    }
}
