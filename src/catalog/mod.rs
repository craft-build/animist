mod error;
mod models_dev;

use std::{collections::BTreeMap, path::Path};

use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use snafu::ResultExt;

use crate::{Struct, provider::ModelInfo};

/// Catalog failures from fetching, loading, and decoding models.dev data.
pub use error::Error;
/// Catalog results; see [`Error`] for the failure variants.
pub type Result<T> = std::result::Result<T, Error>;

/// Default models.dev endpoint used by [`Catalog::fetch`].
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Provider metadata only; no credentials are read and no clients are created.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub env: Vec<String>,
    pub api: Option<String>,
    pub npm: Option<String>,
    pub doc: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Normalized model information alongside its unmodified models.dev record.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct CatalogModel {
    /// Display name; falls back to the model ID when the source omits it.
    pub name: String,
    pub info: ModelInfo,
    /// Retains unrepresented capabilities, pricing tiers, and unknown fields.
    pub metadata: Value,
}

/// One provider's models.dev entry: metadata plus its normalized models.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct ProviderCatalog {
    pub info: ProviderInfo,
    pub models: BTreeMap<String, CatalogModel>,
}

/// Parsed models.dev catalog keyed by provider identifiers.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct Catalog {
    pub providers: BTreeMap<String, ProviderCatalog>,
}

impl Catalog {
    /// Parses a models.dev API document, keeping provider model IDs separate.
    /// Prices are USD per million tokens; absent rates stay unknown, not free.
    /// Tier and required-thinking flags retain defaults rather than inferred rankings.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        models_dev::parse(bytes)
    }

    /// Loads a previously saved models.dev API document.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = tokio::fs::read(path).await.context(error::ReadSnafu {
            path: path.to_path_buf(),
        })?;
        Self::from_slice(&bytes)
    }

    /// Fetches the live catalog from [`MODELS_DEV_URL`] with the caller's client.
    pub async fn fetch(client: &reqwest::Client) -> Result<Self> {
        Self::fetch_from(client, MODELS_DEV_URL).await
    }

    /// Loads from an alternate endpoint, such as a models.dev mirror.
    pub async fn fetch_from(client: &reqwest::Client, url: &str) -> Result<Self> {
        let response = client
            .get(url)
            .send()
            .await
            .context(error::FetchSnafu { url })?
            .error_for_status()
            .context(error::FetchSnafu { url })?;
        let bytes = response.bytes().await.context(error::FetchSnafu { url })?;
        Self::from_slice(&bytes)
    }

    /// The provider catalog keyed by models.dev identifier, e.g. `"anthropic"`.
    pub fn provider(&self, id: &str) -> Option<&ProviderCatalog> {
        self.providers.get(id)
    }

    /// One model by provider and model identifier.
    pub fn model(&self, provider: &str, id: &str) -> Option<&CatalogModel> {
        self.provider(provider)?.models.get(id)
    }
}
