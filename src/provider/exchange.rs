use std::{future::Future, pin::Pin};

use flume::{Receiver, Sender};

use crate::history::ContentBlock;

use super::{ProviderEvent, StreamResponse, TokenUsage};

const RETENTION_LIMIT: usize = 16 * 1024 * 1024;

struct RetainedSize {
    limit: usize,
    bytes: usize,
}

impl std::io::Write for RetainedSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::ErrorKind::OutOfMemory.into());
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Partial output is protocol-specific; tool calls still need explicit results.
#[derive(crate::Struct!)]
pub struct PartialResponse {
    /// Retained visible text, truncated at the retention limit.
    pub display_text: String,
    /// Retained reasoning text, truncated at the retention limit.
    pub display_thinking: String,
    /// Closed content blocks retained for partial recovery.
    pub content: Vec<ContentBlock>,
    /// Latest cumulative usage, when any checkpoint arrived.
    pub usage: Option<TokenUsage>,
    /// True once output was dropped to stay under the retention limit.
    pub retention_limited: bool,
}

/// Events yielded by an `InferenceExchange` while an attempt streams.
#[derive(Debug)]
pub enum ExchangeEvent {
    /// Forwarded transport event; see `ProviderEvent`.
    Provider(ProviderEvent),
    /// Final assembled response, or the first provider error.
    Finished(super::Result<StreamResponse>),
}

/// Owns transport work and bounded partial output without spawning a task.
pub struct InferenceExchange<'a> {
    future: Option<Pin<Box<dyn Future<Output = super::Result<StreamResponse>> + Send + 'a>>>,
    events: Receiver<ProviderEvent>,
    partial: PartialResponse,
    retained: usize,
    result: Option<super::Result<StreamResponse>>,
}

impl std::fmt::Debug for InferenceExchange<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceExchange")
            .field("partial", &self.partial)
            .field("retained", &self.retained)
            .finish_non_exhaustive()
    }
}

impl<'a> InferenceExchange<'a> {
    /// The producer must keep all request work in its returned future.
    pub fn new<F, Fut>(producer: F) -> Self
    where
        F: FnOnce(Sender<ProviderEvent>) -> Fut,
        Fut: Future<Output = super::Result<StreamResponse>> + Send + 'a,
    {
        let (tx, events) = flume::bounded(1);
        Self {
            future: Some(Box::pin(producer(tx))),
            events,
            partial: PartialResponse::default(),
            retained: 0,
            result: None,
        }
    }

    /// Snapshot of the output retained so far.
    pub fn partial(&self) -> &PartialResponse {
        &self.partial
    }

    /// Drops transport, then retains any checkpoint queued before cancellation.
    pub fn cancel(&mut self) -> &PartialResponse {
        self.future = None;
        self.result = None;
        while let Ok(event) = self.events.try_recv() {
            self.retain(&event);
        }
        &self.partial
    }

    /// Cancellation-safe: dropping `next` leaves this exchange resumable.
    pub async fn next(&mut self) -> Option<ExchangeEvent> {
        loop {
            if let Ok(event) = self.events.try_recv() {
                self.retain(&event);
                return Some(ExchangeEvent::Provider(event));
            }
            let Some(future) = self.future.as_mut() else {
                let result = self.result.take()?;
                if let Ok(response) = &result {
                    self.partial.usage = Some(response.usage.clone());
                }
                return Some(ExchangeEvent::Finished(result));
            };
            tokio::select! {
                biased;
                event = self.events.recv_async(), if !self.events.is_disconnected() => {
                    if let Ok(event) = event {
                        self.retain(&event);
                        return Some(ExchangeEvent::Provider(event));
                    }
                }
                result = future => {
                    self.future = None;
                    self.result = Some(result);
                }
            }
        }
    }

    fn retain(&mut self, event: &ProviderEvent) {
        match event {
            ProviderEvent::Usage { usage } => self.partial.usage = Some(usage.clone()),
            ProviderEvent::ContentComplete { block } => {
                let mut size = RetainedSize {
                    limit: RETENTION_LIMIT.saturating_sub(self.retained),
                    bytes: 0,
                };
                if !self.partial.retention_limited
                    && serde_json::to_writer(&mut size, block).is_ok()
                {
                    self.retained += size.bytes;
                    self.partial.content.push(block.clone());
                } else {
                    self.partial.retention_limited = true;
                }
            }
            ProviderEvent::TextDelta { text } | ProviderEvent::ThinkingDelta { text } => {
                let mut length = text
                    .len()
                    .min(RETENTION_LIMIT.saturating_sub(self.retained));
                while !text.is_char_boundary(length) {
                    length -= 1;
                }
                self.retained += length;
                self.partial.retention_limited |= length != text.len();
                match event {
                    ProviderEvent::TextDelta { .. } => {
                        self.partial.display_text.push_str(&text[..length]);
                    }
                    _ => self.partial.display_thinking.push_str(&text[..length]),
                }
            }
            _ => {}
        }
    }
}
