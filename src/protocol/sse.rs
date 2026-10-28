use std::ops::Range;

use snafu::{ResultExt, Snafu, ensure};

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum SseError {
    #[snafu(display("Failed to read SSE stream: {source}"))]
    ReadStream { source: reqwest::Error },
    #[snafu(display("Invalid SSE UTF-8: {source}"))]
    InvalidUtf8 { source: std::str::Utf8Error },
    #[snafu(display("SSE event exceeds {limit} bytes"))]
    EventTooLarge { limit: usize },
    #[snafu(display("SSE buffer exceeds {limit} bytes"))]
    BufferTooLarge { limit: usize },
    #[snafu(display("SSE exchange exceeds {limit} bytes"))]
    ExchangeTooLarge { limit: usize },
}

pub(crate) struct Events {
    response: reqwest::Response,
    buffer: Vec<u8>,
    cursor: usize,
    data: Vec<Range<usize>>,
    data_len: usize,
    first_line: bool,
    eof: bool,
    received: usize,
}

impl Events {
    pub(crate) fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            buffer: Vec::new(),
            cursor: 0,
            data: Vec::new(),
            data_len: 0,
            first_line: true,
            eof: false,
            received: 0,
        }
    }

    pub(crate) async fn next(&mut self) -> Result<Option<String>, SseError> {
        loop {
            if let Some(offset) = self.buffer[self.cursor..]
                .iter()
                .position(|b| *b == b'\n' || *b == b'\r')
                .or_else(|| {
                    (self.eof && self.cursor < self.buffer.len())
                        .then_some(self.buffer.len() - self.cursor)
                })
            {
                let end = self.cursor + offset;
                if self.buffer.get(end) == Some(&b'\r') && end + 1 == self.buffer.len() && !self.eof
                {
                    self.read().await?;
                    continue;
                }
                let width = match self.buffer.get(end) {
                    Some(b'\r') if self.buffer.get(end + 1) == Some(&b'\n') => 2,
                    Some(_) => 1,
                    None => 0,
                };
                let mut start = self.cursor;
                if self.first_line {
                    self.first_line = false;
                    if self.buffer[start..end].starts_with(b"\xef\xbb\xbf") {
                        start += 3;
                    }
                }
                let line =
                    std::str::from_utf8(&self.buffer[start..end]).context(InvalidUtf8Snafu)?;
                self.cursor = end + width;
                if line.is_empty() {
                    if let Some(event) = self.dispatch()? {
                        return Ok(Some(event));
                    }
                } else if let Some(data) = line
                    .strip_prefix("data:")
                    .or_else(|| (line == "data").then_some(""))
                {
                    let data = data.strip_prefix(' ').unwrap_or(data);
                    self.data_len += data.len() + 1;
                    ensure!(
                        self.data_len <= 16 * 1024 * 1024,
                        EventTooLargeSnafu {
                            limit: 16 * 1024 * 1024_usize
                        }
                    );
                    self.data.push(end - data.len()..end);
                }
                if self.data.is_empty() {
                    self.buffer.drain(..self.cursor);
                    self.cursor = 0;
                }
            } else if self.eof {
                return self.dispatch();
            } else {
                self.read().await?;
            }
        }
    }

    fn dispatch(&mut self) -> Result<Option<String>, SseError> {
        if self.data.is_empty() {
            return Ok(None);
        }
        let mut event = String::with_capacity(self.data_len);
        for (index, range) in self.data.drain(..).enumerate() {
            if index != 0 {
                event.push('\n');
            }
            event.push_str(std::str::from_utf8(&self.buffer[range]).context(InvalidUtf8Snafu)?);
        }
        self.buffer.drain(..self.cursor);
        self.cursor = 0;
        self.data_len = 0;
        Ok(Some(event))
    }

    async fn read(&mut self) -> Result<(), SseError> {
        match self.response.chunk().await.context(ReadStreamSnafu)? {
            Some(bytes) => {
                self.received = self.received.saturating_add(bytes.len());
                ensure!(
                    self.received <= 16 * 1024 * 1024,
                    ExchangeTooLargeSnafu {
                        limit: 16 * 1024 * 1024_usize
                    }
                );
                self.buffer.extend_from_slice(&bytes);
            }
            None => self.eof = true,
        }
        ensure!(
            self.buffer.len() <= 16 * 1024 * 1024,
            BufferTooLargeSnafu {
                limit: 16 * 1024 * 1024_usize
            }
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn parses_fragmented_multiline_empty_and_unterminated_events() {
        for (body, expected) in [
            (
                b": comment\r\ndata: first\r\ndata: second\r\n\r\n: ignored\r\ndata:\r\n\r\ndata: third\r\rdata: unfinished".as_slice(),
                vec!["first\nsecond", "", "third", "unfinished"],
            ),
            (b"\xef\xbb\xbfdata: \xc3\xa9\n\ndata\n\n".as_slice(), vec!["é", ""]),
            (b"data: [DONE]".as_slice(), vec!["[DONE]"]),
            (b"data: [DONE]\n".as_slice(), vec!["[DONE]"]),
            (b"data: [DONE]\r\n".as_slice(), vec!["[DONE]"]),
            (b"data: [DONE]\r".as_slice(), vec!["[DONE]"]),
            (b"data: first\ndata: second".as_slice(), vec!["first\nsecond"]),
            (b": heartbeat at EOF".as_slice(), vec![]),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert_ne!(socket.read(&mut request).await.unwrap(), 0);
                socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
                for byte in body {
                    socket.write_all(&[b'1', b'\r', b'\n', *byte, b'\r', b'\n']).await.unwrap();
                    tokio::task::yield_now().await;
                }
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            });
            let response = reqwest::get(format!("http://{address}")).await.unwrap();
            let mut events = Events::new(response);
            for expected in expected {
                assert_eq!(events.next().await.unwrap().as_deref(), Some(expected));
            }
            assert_eq!(events.next().await.unwrap(), None);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_utf8_uses_the_shared_error_boundary() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert_ne!(socket.read(&mut request).await.unwrap(), 0);
            let body = b"data: \xff\n\n";
            socket
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(body).await.unwrap();
        });
        let response = reqwest::get(format!("http://{address}")).await.unwrap();
        let error: crate::protocol::ProtocolError =
            Events::new(response).next().await.unwrap_err().into();
        assert!(matches!(
            error,
            crate::protocol::ProtocolError::Sse {
                source: SseError::InvalidUtf8 { .. }
            }
        ));
        task.await.unwrap();
    }
}
