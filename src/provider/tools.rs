use crate::Struct;
use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool declaration, independent of the harness that executes it.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct Tool {
    /// Identifier the model uses to invoke the tool.
    pub name: String,
    /// When and how the model should use the tool.
    pub description: String,
    /// Arbitrary JSON Schema, translated to each provider's wire format.
    pub input_schema: Value,
}
