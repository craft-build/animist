use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use snafu::ResultExt;

use crate::provider::{Config, Effort, Features, ModelInfo, Pricing, Thinking, Tier};

use super::{Catalog, CatalogModel, ProviderCatalog, ProviderInfo, Result, error};

#[derive(Deserialize)]
struct SourceProvider {
    #[serde(flatten)]
    info: ProviderInfo,
    models: BTreeMap<String, Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SourceModel {
    name: String,
    reasoning: bool,
    reasoning_options: Vec<ReasoningOption>,
    modalities: Modalities,
    cost: Option<Cost>,
    limit: Limits,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Modalities {
    input: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Limits {
    context: Option<u32>,
    output: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Cost {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

#[derive(Deserialize)]
struct ReasoningOption {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Vec<Option<String>>,
}

pub(super) fn parse(bytes: &[u8]) -> Result<Catalog> {
    let providers: BTreeMap<String, SourceProvider> =
        serde_json::from_slice(bytes).context(error::DecodeSnafu)?;
    let mut catalog = Catalog::default();
    for (provider_id, provider) in providers {
        let mut models = BTreeMap::new();
        for (model_id, metadata) in provider.models {
            let source: SourceModel =
                serde_json::from_value(metadata.clone()).context(error::ModelSnafu {
                    provider: &provider_id,
                    model: &model_id,
                })?;
            models.insert(
                model_id.clone(),
                CatalogModel {
                    name: source.name.clone(),
                    info: source.into_info(model_id),
                    metadata,
                },
            );
        }
        catalog.providers.insert(
            provider_id,
            ProviderCatalog {
                info: provider.info,
                models,
            },
        );
    }
    Ok(catalog)
}

impl SourceModel {
    fn into_info(self, id: String) -> ModelInfo {
        let mut efforts = Vec::new();
        if self.reasoning {
            for option in &self.reasoning_options {
                if option.kind == "effort" {
                    for value in option.values.iter().flatten() {
                        if let Some(effort) = effort(value)
                            && !efforts.contains(&effort)
                        {
                            efforts.push(effort);
                        }
                    }
                }
            }
        }
        let cost = self.cost.unwrap_or_default();
        let thinking = Thinking::builder()
            .supported(self.reasoning)
            .required(false)
            .efforts(efforts)
            .build();
        let features = Features::builder()
            .thinking(thinking)
            .vision(self.modalities.input.iter().any(|m| m == "image"))
            .tool_examples(false)
            .deferred_tools(false)
            .build();
        let pricing = Pricing::builder()
            .input(cost.input)
            .output(cost.output)
            .cache_read(cost.cache_read)
            .cache_write(cost.cache_write)
            .build();
        let config = Config::builder()
            .maybe_context_window(self.limit.context)
            .maybe_max_output_tokens(self.limit.output)
            .build();
        ModelInfo::builder()
            .ids(vec![id])
            .tier(Tier::default())
            .features(features)
            .pricing(pricing)
            .config(config)
            .build()
    }
}

fn effort(value: &str) -> Option<Effort> {
    match value {
        "adaptive" => Some(Effort::Adaptive),
        "minimal" => Some(Effort::Minimal),
        "low" => Some(Effort::Low),
        "medium" => Some(Effort::Medium),
        "high" => Some(Effort::High),
        "xhigh" => Some(Effort::XHigh),
        "max" => Some(Effort::Max),
        _ => None,
    }
}
