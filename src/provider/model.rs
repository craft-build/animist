use std::sync::Arc;

use crate::{Enum, Struct};
use bon::Builder;
use serde::{Deserialize, Serialize};

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct FastPricing {
    pub input: f64,
    pub output: f64,
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub fast: Option<FastPricing>,
}

impl Pricing {
    pub const ZERO: Self = Self {
        input: 0.0,
        output: 0.0,
        cache_write: 0.0,
        cache_read: 0.0,
        fast: None,
    };

    pub fn is_zero(&self) -> bool {
        self.input == 0.0 && self.output == 0.0 && self.cache_write == 0.0 && self.cache_read == 0.0
    }

    /// Cache multipliers Anthropic applies on top of the base input rate.
    const CACHE_WRITE_MULTIPLIER: f64 = 1.25;
    const CACHE_READ_MULTIPLIER: f64 = 0.10;

    /// Fast mode only ever quotes two rates, so its cache rates come off its own
    /// input rate rather than the standard one they no longer relate to.
    fn rates(&self, fast: bool) -> (f64, f64, f64, f64) {
        match &self.fast {
            Some(f) if fast => (
                f.input,
                f.output,
                f.input * Self::CACHE_WRITE_MULTIPLIER,
                f.input * Self::CACHE_READ_MULTIPLIER,
            ),
            _ => (self.input, self.output, self.cache_write, self.cache_read),
        }
    }
}

#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Weak,
    #[default]
    Medium,
    Strong,
}

#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Adaptive,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct Thinking {
    pub required: bool,
    pub efforts: Vec<Effort>,
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Features {
    pub tool_examples: bool,
    pub thinking: Thinking,
    pub vision: bool,
    pub deferred_tools: bool,
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Config {
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub context_window: Option<u32>,
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct ModelInfo {
    pub ids: Vec<String>,
    pub tier: Tier,
    pub features: Features,
    pub pricing: Pricing,
}

#[derive(Enum!)]
pub enum ThinkingSupport {
    No,
    Yes,
    Required,
}

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct Overrides {
    pub tool_examples: Option<bool>,
    pub thinking: Option<ThinkingSupport>,
    pub vision: Option<bool>,
    pub fast: Option<bool>,
}

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct Model {
    pub id: String,
    pub provider: Arc<str>,
    pub info: ModelInfo,
}
