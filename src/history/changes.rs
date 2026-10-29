use std::sync::Arc;

use super::{HistoryEntry, StoredMessage};

/// A runtime cursor scoped to one history, including its cloned handles.
#[derive(Debug, Clone)]
pub struct HistoryVersion {
    pub(super) lineage: Arc<()>,
    pub(super) generation: u64,
}

impl PartialEq for HistoryVersion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lineage, &other.lineage) && self.generation == other.generation
    }
}

impl Eq for HistoryVersion {}

/// Replay in order; each index addresses the list at that individual change.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryChange {
    /// Adds an entry at the end of the list.
    Appended { index: usize, entry: HistoryEntry },
    /// Inserts an entry, shifting entries at and after the index right by one.
    Inserted { index: usize, entry: HistoryEntry },
    /// Appends a stored revision without changing the original or activation.
    Revised {
        index: usize,
        message: StoredMessage,
    },
    /// Marks an entry inactive without removing it or shifting indices.
    Deactivated { index: usize },
    /// Replaces a whole entry, including revisions, without shifting indices.
    EntryReplaced { index: usize, entry: HistoryEntry },
}

/// An atomic delta read and the cursor for the next read.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryChanges {
    pub(super) version: HistoryVersion,
    pub(super) changes: Vec<HistoryChange>,
}

impl HistoryChanges {
    /// Cursor after all changes in this response, even when the list is empty.
    pub fn generation(&self) -> HistoryVersion {
        self.version.clone()
    }

    /// Ordered changes since the supplied cursor.
    pub fn changes(&self) -> &[HistoryChange] {
        &self.changes
    }
}
