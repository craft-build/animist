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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}
