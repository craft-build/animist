//! Bounded reads for non-streaming response bodies.

use snafu::{ResultExt, Snafu, ensure};

/// Largest non-streaming response body accepted, in bytes.
pub(crate) const BODY_LIMIT: usize = 16 * 1024 * 1024;

/// Non-streaming body read and size-limit failures.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum BodyError {
    #[snafu(display("Failed to read response body: {source}"))]
    ReadBody { source: reqwest::Error },
    #[snafu(display("Response body exceeds {limit} bytes"))]
    BodyTooLarge {
        limit: usize,
        backtrace: snafu::Backtrace,
    },
}

/// Buffers a complete body, failing once it exceeds `BODY_LIMIT`.
pub(crate) async fn read_body(mut response: reqwest::Response) -> Result<Vec<u8>, BodyError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context(ReadBodySnafu)? {
        ensure!(
            bytes.len() + chunk.len() <= BODY_LIMIT,
            BodyTooLargeSnafu { limit: BODY_LIMIT }
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
