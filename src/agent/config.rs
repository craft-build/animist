use bon::Builder;
use serde::{Deserialize, Serialize};

use crate::StructWithCustomDefault;

#[derive(StructWithCustomDefault!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct AgentConfig {
    #[builder(default = 32_u32)]
    pub max_steps: u32,
    #[builder(default = 0_u32)]
    pub max_retries: u32,
    #[builder(default = 8_u32)]
    pub max_tool_depth: u32,
    #[builder(default = 1_usize)]
    pub max_concurrent_tools: usize,
    #[builder(default = 1024_u64)]
    pub max_tool_calls: u64,
    #[builder(default = 32_u64)]
    pub max_child_calls: u64,
    /// Applies to execution only; timeout does not undo external effects.
    pub tool_timeout_ms: Option<u64>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}
