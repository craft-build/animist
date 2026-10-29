//! Shared HTTP status handling for provider transports.

use snafu::ResultExt;

use super::{Result, error};

/// Cap on failed-response bodies buffered into errors, in bytes.
pub(crate) const ERROR_BODY_LIMIT: usize = 64 * 1024;

/// Buffers an `ERROR_BODY_LIMIT`-byte prefix, then drops the remaining body.
/// Invalid UTF-8 is replaced and the rendered prefix stays within the same limit.
/// The flag marks truncation or a limit reached without proving completeness.
async fn read_error_body(mut response: reqwest::Response) -> Result<(String, bool)> {
    let content_length = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok());
    let mut bytes = Vec::with_capacity(ERROR_BODY_LIMIT);
    let mut body_truncated = false;
    while bytes.len() < ERROR_BODY_LIMIT {
        let Some(chunk) = response.chunk().await.context(error::TransportSnafu)? else {
            break;
        };
        let remaining = ERROR_BODY_LIMIT - bytes.len();
        body_truncated |= chunk.len() > remaining;
        bytes.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
    }
    body_truncated |= bytes.len() == ERROR_BODY_LIMIT && content_length != Some(bytes.len());
    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    let mut limit = ERROR_BODY_LIMIT.min(body.len());
    while !body.is_char_boundary(limit) {
        limit -= 1;
    }
    body_truncated |= limit < body.len();
    body.truncate(limit);
    Ok((body.into_boxed_str().into_string(), body_truncated))
}

/// Passes successful responses through; otherwise returns `Http` with a bounded body.
pub(super) async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let (body, body_truncated) = read_error_body(response).await?;
    error::HttpSnafu {
        status,
        body,
        body_truncated,
    }
    .fail()
}
