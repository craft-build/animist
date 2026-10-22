mod message;
mod stored;

use async_trait::async_trait;
pub use message::{ContentBlock, ImageSource, Message, MessageKind, Role};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::history::stored::{Altered, HistoryEntry, StoredMessage};

#[async_trait]
pub trait Alteration {
    async fn call(&self, message: &StoredMessage) -> Option<StoredMessage>;
}

pub enum CompactorAction {
    Keep,
    Remove,
    Alter(StoredMessage),
}

#[async_trait]
pub trait Compactor {
    async fn call(&self, message: &StoredMessage) -> Result<CompactorAction, crate::Error>;
}

pub trait HistoryAlteration: AsyncFn(&StoredMessage) -> Option<StoredMessage> {}
pub trait HistoryCompactor: AsyncFn(&StoredMessage) -> Result<bool, crate::Error> {}

#[derive(Debug, Clone)]
pub struct History {
    history: Arc<RwLock<Vec<HistoryEntry>>>,
}

impl History {
    pub async fn current(&self) -> Vec<Message> {
        let ptr = self.history.read().await;
        ptr.iter()
            .filter_map(HistoryEntry::current)
            .map(StoredMessage::message)
            .cloned()
            .collect()
    }

    pub async fn unaltered(&self) -> Vec<Message> {
        let ptr = self.history.read().await;
        ptr.iter()
            .filter_map(HistoryEntry::unaltered)
            .map(StoredMessage::message)
            .cloned()
            .collect()
    }

    pub async fn verbatim(&self) -> Vec<Message> {
        let ptr = self.history.read().await;
        ptr.iter()
            .map(HistoryEntry::verbatim)
            .map(StoredMessage::message)
            .cloned()
            .collect()
    }

    pub async fn add(&self, message: &Message) {
        let mut ptr = self.history.write().await;
        ptr.push(HistoryEntry::Active(
            StoredMessage::builder()
                .timestamp(jiff::Timestamp::now())
                .message(message.clone())
                .build(),
        ));
    }

    pub async fn alter(&self, method: impl Alteration) {
        let mut ptr = self.history.write().await;
        for entry_ptr in ptr.iter_mut() {
            if let Some(entry) = entry_ptr.current()
                && let Some(altered) = method.call(entry).await
            {
                if let HistoryEntry::Altered(inner) = entry_ptr {
                    inner.alter(&altered);
                } else {
                    *entry_ptr = HistoryEntry::Altered(
                        Altered::builder()
                            .source(entry.clone())
                            .alterations(vec![altered])
                            .build(),
                    );
                };
            }
        }
    }

    pub async fn compact(&self, method: impl Compactor) -> Result<(), crate::Error> {
        let mut ptr = self.history.write().await;
        for entry_ptr in ptr.iter_mut() {
            if let Some(entry) = entry_ptr.current() {
                match method.call(entry).await? {
                    CompactorAction::Remove => *entry_ptr = HistoryEntry::Inactive(entry.clone()),
                    CompactorAction::Alter(message)
                        if let HistoryEntry::Altered(altered) = entry_ptr =>
                    {
                        altered.alter(&message)
                    }
                    CompactorAction::Alter(message) => {
                        *entry_ptr = HistoryEntry::Altered(
                            Altered::builder()
                                .source(entry.clone())
                                .alterations(vec![message])
                                .build(),
                        )
                    }
                    CompactorAction::Keep => {}
                };
            }
        }
        Ok(())
    }
}
