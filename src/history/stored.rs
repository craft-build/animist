use std::collections::BTreeMap;

use super::Message;
use crate::Struct;
use bon::Builder;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct StoredMessage {
    timestamp: Timestamp,
    message: Message,
    #[builder(default)]
    tags: BTreeMap<String, String>,
}

impl StoredMessage {
    pub fn timestamp(&self) -> &Timestamp {
        &self.timestamp
    }

    pub fn message(&self) -> &Message {
        &self.message
    }

    pub fn tags(&self) -> &BTreeMap<String, String> {
        &self.tags
    }

    pub fn tags_mut(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.tags
    }
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
pub struct Altered {
    source: StoredMessage,
    alterations: Vec<StoredMessage>,
}

impl Altered {
    pub fn source(&self) -> &StoredMessage {
        &self.source
    }

    pub fn get(&self) -> &StoredMessage {
        self.alterations.last().unwrap_or(&self.source)
    }

    pub fn alter(&mut self, message: &StoredMessage) {
        self.alterations.push(message.clone());
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HistoryEntry {
    Active(StoredMessage),
    Inactive(StoredMessage),
    Altered(Altered),
}

impl HistoryEntry {
    pub fn current(&self) -> Option<&StoredMessage> {
        match self {
            Self::Active(entry) => Some(entry),
            Self::Altered(entry) => Some(entry.get()),
            _ => None,
        }
    }

    pub fn unaltered(&self) -> Option<&StoredMessage> {
        match self {
            Self::Active(entry) => Some(entry),
            Self::Altered(entry) => Some(entry.source()),
            _ => None,
        }
    }

    pub fn verbatim(&self) -> &StoredMessage {
        match self {
            Self::Active(entry) => entry,
            Self::Altered(entry) => entry.source(),
            Self::Inactive(entry) => entry,
        }
    }
}
