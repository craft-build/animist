use std::collections::BTreeSet;

use bytes::Bytes;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use snafu::{ResultExt, ensure};

use super::{Bedrock, error};
use crate::provider::{ModelInfo, Result};

/// Total JSON response bytes allowed across one discovery operation.
const LISTING_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FoundationModels {
    #[serde(default)]
    model_summaries: Vec<FoundationModel>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FoundationModel {
    model_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InferenceProfiles {
    #[serde(default)]
    inference_profile_summaries: Vec<InferenceProfile>,
    next_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InferenceProfile {
    inference_profile_id: String,
}

impl Bedrock {
    pub(super) async fn list_control_plane_models(&self) -> Result<Vec<ModelInfo>> {
        let mut remaining = LISTING_LIMIT;
        let foundations: FoundationModels = self
            .list_page("/foundation-models", None, &mut remaining)
            .await?;
        let mut ids: BTreeSet<_> = foundations
            .model_summaries
            .into_iter()
            .map(|model| model.model_id)
            .collect();
        let mut token = None;
        let mut seen = BTreeSet::new();
        loop {
            let profiles: InferenceProfiles = self
                .list_page("/inference-profiles", token.as_deref(), &mut remaining)
                .await?;
            ids.extend(
                profiles
                    .inference_profile_summaries
                    .into_iter()
                    .map(|profile| profile.inference_profile_id),
            );
            let Some(next) = profiles.next_token else {
                break;
            };
            ensure!(
                !next.is_empty() && seen.insert(next.clone()),
                error::InvalidListingTokenSnafu { token: next }
            );
            token = Some(next);
        }
        ids.into_iter()
            .map(|id| {
                ensure!(!id.is_empty(), error::EmptyListedModelIdSnafu);
                let mut info = self
                    .config
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.models.get(&id))
                    .map(|model| model.info.clone())
                    .unwrap_or_default();
                info.ids = vec![id];
                Ok(info)
            })
            .collect()
    }

    async fn list_page<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
        remaining: &mut usize,
    ) -> Result<T> {
        let mut url = self.control_plane_endpoint.clone();
        url.set_path(path);
        if let Some(token) = token {
            url.query_pairs_mut().append_pair("nextToken", token);
        }
        let request = http::Request::builder()
            .method("GET")
            .uri(url.as_str())
            .header("accept", "application/json")
            .body(Bytes::new())
            .context(error::RequestSnafu)?;
        let mut response = self.send_signed(request).await?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.context(error::TransportSnafu)? {
            ensure!(
                chunk.len() <= *remaining,
                error::ListingTooLargeSnafu {
                    limit: LISTING_LIMIT
                }
            );
            *remaining -= chunk.len();
            body.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&body).context(error::DecodeListingSnafu)?)
    }
}
