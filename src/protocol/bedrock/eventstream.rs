use std::collections::BTreeMap;

use bytes::{Buf as _, Bytes};
use snafu::{OptionExt, ResultExt, ensure};

use super::error::{self, EventStreamError};

/// Largest single frame.
const FRAME_LIMIT: usize = 16 * 1024 * 1024;
/// Total wire bytes per exchange; far above any realistic generation.
const EXCHANGE_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct Frame {
    pub headers: BTreeMap<String, Option<String>>,
    pub payload: Bytes,
}

impl Frame {
    pub(super) fn header(&self, name: &'static str) -> Result<&str, EventStreamError> {
        self.headers
            .get(name)
            .and_then(Option::as_deref)
            .context(error::RequiredHeaderSnafu { name })
    }
}

pub(super) struct Frames {
    response: reqwest::Response,
    pending: Bytes,
    buffer: Vec<u8>,
    received: usize,
}

impl Frames {
    pub(super) fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            pending: Bytes::new(),
            buffer: Vec::new(),
            received: 0,
        }
    }

    pub(super) async fn next(&mut self) -> Result<Option<Frame>, EventStreamError> {
        if !self.read_to(12).await? {
            ensure!(
                self.buffer.is_empty(),
                error::TruncatedFrameSnafu {
                    received: self.buffer.len(),
                    expected: 12_usize
                }
            );
            return Ok(None);
        }
        let total = usize::try_from(self.buffer.as_slice().get_u32())
            .expect("HTTP targets have at least 32-bit pointers");
        let headers = usize::try_from(self.buffer[4..].as_ref().get_u32())
            .expect("HTTP targets have at least 32-bit pointers");
        let expected = self.buffer[8..].as_ref().get_u32();
        let actual = crc32fast::hash(&self.buffer[..8]);
        ensure!(
            actual == expected,
            error::CrcSnafu {
                section: "prelude",
                expected,
                actual
            }
        );
        ensure!(
            total >= 16 && headers <= total - 16,
            error::InvalidLengthsSnafu { total, headers }
        );
        ensure!(
            total <= FRAME_LIMIT,
            error::FrameTooLargeSnafu { limit: FRAME_LIMIT }
        );
        ensure!(
            self.read_to(total).await?,
            error::TruncatedFrameSnafu {
                received: self.buffer.len(),
                expected: total
            }
        );
        let expected = self.buffer[total - 4..].as_ref().get_u32();
        let actual = crc32fast::hash(&self.buffer[..total - 4]);
        ensure!(
            actual == expected,
            error::CrcSnafu {
                section: "message",
                expected,
                actual
            }
        );
        let headers_map = decode_headers(&self.buffer[12..12 + headers])?;
        let bytes = Bytes::from(std::mem::take(&mut self.buffer));
        Ok(Some(Frame {
            headers: headers_map,
            payload: bytes.slice(12 + headers..total - 4),
        }))
    }

    async fn read_to(&mut self, length: usize) -> Result<bool, EventStreamError> {
        while self.buffer.len() < length {
            if self.pending.is_empty() {
                let Some(bytes) = self.response.chunk().await.context(error::ReadSnafu)? else {
                    return Ok(false);
                };
                self.received = self.received.saturating_add(bytes.len());
                ensure!(
                    self.received <= EXCHANGE_LIMIT,
                    error::ExchangeTooLargeSnafu {
                        limit: EXCHANGE_LIMIT
                    }
                );
                self.pending = bytes;
            }
            let count = (length - self.buffer.len()).min(self.pending.len());
            self.buffer.extend_from_slice(&self.pending[..count]);
            self.pending.advance(count);
        }
        Ok(true)
    }
}

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Result<&'a [u8], EventStreamError> {
    let prefix = bytes.get(..length).context(error::TruncatedHeaderSnafu)?;
    *bytes = &bytes[length..];
    Ok(prefix)
}

fn decode_headers(mut bytes: &[u8]) -> Result<BTreeMap<String, Option<String>>, EventStreamError> {
    let mut headers = BTreeMap::new();
    while !bytes.is_empty() {
        let length = usize::from(take(&mut bytes, 1)?[0]);
        ensure!(length > 0, error::EmptyHeaderNameSnafu);
        let name = std::str::from_utf8(take(&mut bytes, length)?)
            .context(error::HeaderUtf8Snafu)?
            .to_owned();
        let kind = take(&mut bytes, 1)?[0];
        let value = match kind {
            0 | 1 => None,
            2..=5 | 8 | 9 => {
                let length = match kind {
                    2 => 1,
                    3 => 2,
                    4 => 4,
                    5 | 8 => 8,
                    _ => 16,
                };
                take(&mut bytes, length)?;
                None
            }
            6 | 7 => {
                let length = usize::from(take(&mut bytes, 2)?.get_u16());
                let value = take(&mut bytes, length)?;
                if kind == 7 {
                    Some(
                        std::str::from_utf8(value)
                            .context(error::HeaderUtf8Snafu)?
                            .to_owned(),
                    )
                } else {
                    None
                }
            }
            _ => return Err(error::UnknownHeaderTypeSnafu { kind }.build()),
        };
        ensure!(
            !headers.contains_key(&name),
            error::DuplicateHeaderSnafu { name: &name }
        );
        headers.insert(name, value);
    }
    Ok(headers)
}
