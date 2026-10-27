use std::collections::BTreeMap;

use super::Message;
use crate::Struct;
use bon::Builder;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct StoredMessage {
    timestamp: Timestamp,
    message: Message,
    #[builder(default)]
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

impl StoredMessage {
    pub fn timestamp(&self) -> &Timestamp {
        &self.timestamp
    }

    pub fn message(&self) -> &Message {
        &self.message
    }

    pub fn message_mut(&mut self) -> &mut Message {
        &mut self.message
    }

    pub fn tags(&self) -> &BTreeMap<String, String> {
        &self.tags
    }

    pub fn tags_mut(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.tags
    }
}

/// An original message, its stored revisions, and independent activation state.
#[derive(Debug, Clone, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct HistoryEntry {
    source: StoredMessage,
    #[builder(default)]
    #[serde(default)]
    revisions: Vec<StoredMessage>,
    #[builder(default = true)]
    #[serde(default = "default_active")]
    active: bool,
}

fn default_active() -> bool {
    true
}

impl HistoryEntry {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn revisions(&self) -> &[StoredMessage] {
        &self.revisions
    }

    /// The latest message and metadata, including for inactive entries.
    pub fn latest(&self) -> &StoredMessage {
        self.revisions.last().unwrap_or(&self.source)
    }

    pub fn current(&self) -> Option<&StoredMessage> {
        self.active.then(|| self.latest())
    }

    pub fn unaltered(&self) -> Option<&StoredMessage> {
        self.active.then_some(&self.source)
    }

    /// The original message and metadata, regardless of activation.
    pub fn verbatim(&self) -> &StoredMessage {
        &self.source
    }

    pub(super) fn alter(&mut self, message: StoredMessage) {
        self.revisions.push(message);
    }

    pub(super) fn deactivate(&mut self) {
        self.active = false;
    }
}
