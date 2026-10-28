use std::sync::Arc;

use crate::{Enum, Struct, StructNoDebug, provider::Provider};
use bon::Builder;
use serde::{Deserialize, Serialize};

/// Per-million USD rates for fast-mode responses.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct FastPricing {
    /// Fast-mode input tokens, USD per million.
    pub input: f64,
    /// Fast-mode output tokens, USD per million.
    pub output: f64,
}

/// Per-million USD rates; `None` rates are unreported by the source, not free.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Pricing {
    /// Standard input tokens, USD per million.
    #[serde(default)]
    pub input: Option<f64>,
    /// Standard output tokens, USD per million.
    #[serde(default)]
    pub output: Option<f64>,
    /// Prompt cache writes, USD per million.
    #[serde(default)]
    pub cache_write: Option<f64>,
    /// Prompt cache reads, USD per million.
    #[serde(default)]
    pub cache_read: Option<f64>,
    /// Rates that replace the standard ones in fast mode.
    #[serde(default)]
    pub fast: Option<FastPricing>,
}

impl Pricing {
    /// True when every standard rate is known; fast pricing is ignored.
    pub fn is_complete(&self) -> bool {
        self.input.is_some()
            && self.output.is_some()
            && self.cache_write.is_some()
            && self.cache_read.is_some()
    }

    /// Cache multipliers Anthropic apply on top of the base input rate.
    #[expect(dead_code, reason = "wired up with usage cost totals")]
    const CACHE_WRITE_MULTIPLIER: f64 = 1.25;
    #[expect(dead_code, reason = "wired up with usage cost totals")]
    const CACHE_READ_MULTIPLIER: f64 = 0.10;

    /// Fast mode only ever quotes two rates, so its cache rates come off its own
    /// input rate rather than the standard one they no longer relate to.
    #[expect(dead_code, reason = "wired up with usage cost totals")]
    fn rates(&self, fast: bool) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
        match &self.fast {
            Some(f) if fast => (
                Some(f.input),
                Some(f.output),
                Some(f.input * Self::CACHE_WRITE_MULTIPLIER),
                Some(f.input * Self::CACHE_READ_MULTIPLIER),
            ),
            _ => (self.input, self.output, self.cache_write, self.cache_read),
        }
    }
}

/// Relative capability tier for routing; sources may omit one.
#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Cheapest tier for trivial or bulk work.
    Weak,
    /// Default tier for general-purpose work.
    #[default]
    Medium,
    /// Highest-capability tier for hard tasks.
    Strong,
}

/// Requested reasoning intensity; `Adaptive` delegates budgeting to the model.
#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Model-allocated reasoning budget.
    Adaptive,
    /// Least reasoning the protocol supports.
    Minimal,
    /// Light reasoning.
    Low,
    /// Balanced reasoning.
    #[default]
    Medium,
    /// Heavy reasoning.
    High,
    /// Extended reasoning beyond `High`.
    XHigh,
    /// Maximum reasoning the model supports.
    Max,
}

impl Effort {
    /// Wire string for OpenAI-compatible `reasoning_effort` fields;
    /// `None` marks adaptive budgeting, which has no effort string.
    pub fn openai_wire(&self) -> Option<&'static str> {
        match self {
            Self::Adaptive => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }
}

/// Whether and at which intensities a model can reason.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct Thinking {
    /// Model can reason when requested.
    #[serde(default)]
    #[builder(default)]
    pub supported: bool,
    /// Model refuses to answer without reasoning.
    pub required: bool,
    /// Efforts the source reports as accepted.
    pub efforts: Vec<Effort>,
}

/// Capability flags reported by the source catalog.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Features {
    /// Tool schemas may embed worked examples.
    pub tool_examples: bool,
    /// Reasoning support and accepted efforts.
    pub thinking: Thinking,
    /// Image inputs are accepted.
    pub vision: bool,
    /// Tool execution can be deferred server-side.
    pub deferred_tools: bool,
}

/// Source-reported token limits, omitted when unknown.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Config {
    /// Hard cap on generated tokens.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Total tokens the model can attend over.
    #[serde(default)]
    pub context_window: Option<u32>,
}

/// Catalog metadata for one model, merged over source listings.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct ModelInfo {
    /// IDs this entry answers to; the first is canonical.
    pub ids: Vec<String>,
    /// Routing tier.
    pub tier: Tier,
    /// Reported capabilities.
    pub features: Features,
    /// Per-million USD rates.
    pub pricing: Pricing,
    /// Source-reported token limits.
    #[serde(default)]
    #[builder(default)]
    pub config: Config,
}

/// Runtime override for a model's thinking capability.
#[derive(Enum!)]
pub enum ThinkingSupport {
    /// Forces reasoning off even when the model supports it.
    No,
    /// Forces reasoning available even when unreported.
    Yes,
    /// Forces every request to reason.
    Required,
}

/// Runtime capability overrides layered over catalog data.
#[derive(Struct!, PartialEq, Eq, Builder)]
#[builder(on(_, into))]
pub struct Overrides {
    /// Overrides the catalog's tool-example flag.
    pub tool_examples: Option<bool>,
    /// Overrides the catalog's thinking capability.
    pub thinking: Option<ThinkingSupport>,
    /// Overrides the catalog's vision flag.
    pub vision: Option<bool>,
    /// `Some(false)` disables fast mode; `Some(true)` asserts support.
    pub fast: Option<bool>,
}

/// A model bound to its owning provider, with catalog info and overrides.
#[derive(StructNoDebug!, Builder)]
#[builder(on(_, into))]
pub struct Model {
    /// Wire model ID sent in requests.
    pub id: String,
    /// Provider that owns and validates this model.
    pub provider: Arc<Provider>,
    /// Catalog metadata merged with source listings.
    pub info: ModelInfo,
    /// Runtime capability overrides.
    #[builder(default)]
    pub overrides: Overrides,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("id", &self.id)
            .field("provider", &self.provider.name())
            .field("info", &self.info)
            .field("overrides", &self.overrides)
            .finish()
    }
}
