use std::collections::BTreeMap;

use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Result;

/// Explicit decider model identity, independent of chat configuration.
#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct DeciderModel {
    /// Wire model ID sent in requests.
    pub id: String,
    /// Provider name that owns and validates this model.
    pub provider: String,
}

/// Boolean-answer guidance applied before scoring the options.
#[derive(crate::Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct BooleanCriteria {
    /// JSON template evaluated when the answer is true.
    pub when_true: Option<Value>,
    /// JSON template evaluated when the answer is false.
    pub when_false: Option<Value>,
}

/// Structured question a decider model answers in one exchange.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionQuestion {
    /// Pick one labeled option and score every alternative.
    Choice {
        instructions: Value,
        options: BTreeMap<String, Option<Value>>,
    },
    /// Map instructions onto a fixed ladder of levels.
    Score {
        instructions: Value,
        levels: Vec<Value>,
    },
    /// Answer yes or no, optionally guided by structured criteria.
    Boolean {
        instructions: Value,
        criteria: Option<BooleanCriteria>,
    },
}

/// State and instructions may contain structured application data.
#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct DecisionRequest {
    /// Application state the questions are evaluated against.
    pub state: Value,
    /// Questions keyed by the answer name they populate.
    pub questions: BTreeMap<String, DecisionQuestion>,
    /// Inline image/audio data URLs; availability depends on the model and server.
    #[builder(default)]
    pub files: Vec<String>,
}

/// Structured answer a decider model returns for one question.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionAnswer {
    /// Chosen option with per-option probabilities and confidence.
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// Continuous score over the declared levels, with per-level probabilities.
    Score {
        score: f64,
        legend: BTreeMap<u32, Value>,
        probabilities: BTreeMap<u32, f64>,
        confidence: f64,
    },
    /// Probability that the boolean question is true.
    Boolean { probability: f64 },
}

/// Token accounting for one decider exchange.
#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct DecisionUsage {
    /// Tokens consumed from the prompt.
    pub input_tokens: u64,
    /// Tokens generated across all answers.
    pub output_tokens: u64,
}

/// Assembled answers for one decision request.
#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct DecisionResponse {
    /// Model ID that produced the answers.
    pub model: String,
    /// Answers keyed by their request's question name.
    pub answers: BTreeMap<String, DecisionAnswer>,
    /// Usage for the exchange, when the server reports it.
    pub usage: Option<DecisionUsage>,
}

/// Provider specialized for cheap, structured single-shot decisions.
#[arc_handle]
#[async_trait]
pub trait DeciderProvider {
    /// Stable provider identifier used for model ownership checks.
    fn name(&self) -> &'static str;

    /// Dropping this future cancels the exchange; implementations must not detach it.
    async fn decide(
        &self,
        model: &DeciderModel,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse>;

    /// Renews expiring credentials held in memory.
    async fn refresh_auth(&self) -> Result<()>;

    /// Re-reads credentials from their original source.
    async fn reload_auth(&self) -> Result<()>;
}
