use std::collections::BTreeMap;

use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Result;

#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct DeciderModel {
    pub id: String,
    pub provider: String,
}

#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct BooleanCriteria {
    pub when_true: Option<Value>,
    pub when_false: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionQuestion {
    Choice {
        instructions: Value,
        options: BTreeMap<String, Option<Value>>,
    },
    Score {
        instructions: Value,
        levels: Vec<Value>,
    },
    Boolean {
        instructions: Value,
        criteria: Option<BooleanCriteria>,
    },
}

/// State and instructions may contain structured application data.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct DecisionRequest {
    pub state: Value,
    pub questions: BTreeMap<String, DecisionQuestion>,
    /// Inline image/audio data URLs; availability depends on the model and server.
    #[builder(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionAnswer {
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        legend: BTreeMap<u32, Value>,
        probabilities: BTreeMap<u32, f64>,
        confidence: f64,
    },
    Boolean {
        probability: f64,
    },
}

#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct DecisionUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct DecisionResponse {
    pub model: String,
    pub answers: BTreeMap<String, DecisionAnswer>,
    pub usage: Option<DecisionUsage>,
}

#[arc_handle]
#[async_trait]
pub trait DeciderProvider {
    fn name(&self) -> &str;

    /// Dropping this future cancels the exchange; implementations must not detach it.
    async fn decide(
        &self,
        model: &DeciderModel,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse>;

    async fn refresh_auth(&self) -> Result<()>;
    async fn reload_auth(&self) -> Result<()>;
}
