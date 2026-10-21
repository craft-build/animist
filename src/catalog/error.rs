use std::path::PathBuf;

use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    #[snafu(display("Failed to fetch catalog from {url}: {source}"))]
    Fetch { url: String, source: reqwest::Error },
    #[snafu(display("Failed to read catalog {}: {source}", path.display()))]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[snafu(display("Invalid models.dev catalog: {source}"))]
    Decode { source: serde_json::Error },
    #[snafu(display("Invalid model {provider}/{model}: {source}"))]
    Model {
        provider: String,
        model: String,
        source: serde_json::Error,
    },
}
