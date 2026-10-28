//! Revision-preserving conversation history with atomic asynchronous rewrites.
//! See `docs/history.md` for view, edit-plan, and concurrency semantics.

mod message;
mod stored;
mod vcc;

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use bon::Builder;
pub use message::{
    ContentBlock, ImageSource, Message, MessageKind, Role, ToolContent, ToolContents,
};
use snafu::{OptionExt, ResultExt, Snafu, ensure};
pub use stored::{HistoryEntry, StoredMessage};
use tokio::sync::RwLock;
pub use vcc::{VccCompactor, VccExtraction, VccExtractor, VccWithExtractor};

use crate::Struct;

#[derive(Debug, Snafu)]
pub enum HistoryError {
    #[snafu(display("History changed while the transformation was running"))]
    Conflict,
    #[snafu(display("History entry index {index} is out of bounds for {len} entries"))]
    InvalidIndex { index: usize, len: usize },
    #[snafu(display("History entry {index} is inactive"))]
    InactiveEntry { index: usize },
    #[snafu(display("History entry {index} is edited more than once"))]
    DuplicateEdit { index: usize },
    #[snafu(display("History transformation failed: {source}"))]
    Transformation { source: crate::Error },
}

/// Revises the latest message and metadata without modifying earlier versions.
#[async_trait]
pub trait Alteration: Send + Sync {
    async fn call(&self, message: &StoredMessage) -> Result<Option<StoredMessage>, crate::Error>;
}

/// All indices refer to the supplied snapshot, including inactive entries.
#[derive(Debug, Clone, PartialEq)]
pub enum CompactionEdit {
    Remove {
        index: usize,
    },
    Replace {
        index: usize,
        message: StoredMessage,
    },
    /// Inserts before an index, or appends at the snapshot length.
    Insert {
        before: usize,
        message: StoredMessage,
    },
}

/// Omitted entries are kept; each existing entry may be edited at most once.
#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct CompactionPlan {
    #[builder(default)]
    pub edits: Vec<CompactionEdit>,
}

/// Plans a conversation-level rewrite without holding the history lock.
#[async_trait]
pub trait Compactor: Send + Sync {
    async fn call(&self, history: &HistorySnapshot) -> Result<CompactionPlan, crate::Error>;
}

/// An owned, consistent view; indices remain stable for this snapshot only.
#[derive(Debug, Clone)]
pub struct HistorySnapshot {
    entries: Vec<HistoryEntry>,
    generation: u64,
}

impl HistorySnapshot {
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    fn into_entries(self, plan: CompactionPlan) -> Result<Vec<HistoryEntry>, HistoryError> {
        let len = self.entries.len();
        let mut edited = BTreeSet::new();
        let mut insertion_count = 0;
        for edit in &plan.edits {
            let index = match edit {
                CompactionEdit::Insert { before, .. } => {
                    ensure!(
                        *before <= len,
                        InvalidIndexSnafu {
                            index: *before,
                            len
                        }
                    );
                    insertion_count += 1;
                    continue;
                }
                CompactionEdit::Remove { index } | CompactionEdit::Replace { index, .. } => *index,
            };
            let entry = self
                .entries
                .get(index)
                .context(InvalidIndexSnafu { index, len })?;
            ensure!(entry.is_active(), InactiveEntrySnafu { index });
            ensure!(edited.insert(index), DuplicateEditSnafu { index });
        }

        let mut entries = self.entries;
        let mut insertions = Vec::with_capacity(insertion_count);
        for edit in plan.edits {
            match edit {
                CompactionEdit::Remove { index } => entries[index].deactivate(),
                CompactionEdit::Replace { index, message } => entries[index].alter(message),
                CompactionEdit::Insert { before, message } => {
                    insertions.push((before, HistoryEntry::builder().source(message).build()));
                }
            }
        }
        if insertions.is_empty() {
            return Ok(entries);
        }
        insertions.sort_by_key(|(before, _)| *before);
        let mut insertions = insertions.into_iter().peekable();
        let mut result = Vec::with_capacity(len + insertion_count);
        for (index, entry) in entries.into_iter().enumerate() {
            while let Some((_, inserted)) = insertions.next_if(|(before, _)| *before == index) {
                result.push(inserted);
            }
            result.push(entry);
        }
        result.extend(insertions.map(|(_, entry)| entry));
        Ok(result)
    }
}

#[derive(Debug, Default)]
struct HistoryState {
    entries: Vec<HistoryEntry>,
    generation: u64,
}

/// Clones share live state. Use a snapshot to obtain an independent copy.
#[derive(Debug, Clone, Default)]
pub struct History {
    history: Arc<RwLock<HistoryState>>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restores originals, revisions, metadata, and activation from saved entries.
    pub fn from_entries(entries: Vec<HistoryEntry>) -> Self {
        Self {
            history: Arc::new(RwLock::new(HistoryState {
                entries,
                generation: 0,
            })),
        }
    }

    pub async fn snapshot(&self) -> HistorySnapshot {
        let state = self.history.read().await;
        HistorySnapshot {
            entries: state.entries.clone(),
            generation: state.generation,
        }
    }

    /// Latest content of active entries.
    pub async fn current(&self) -> Vec<Message> {
        self.history
            .read()
            .await
            .entries
            .iter()
            .filter_map(HistoryEntry::current)
            .map(StoredMessage::message)
            .cloned()
            .collect()
    }

    /// Original content of active entries.
    pub async fn unaltered(&self) -> Vec<Message> {
        self.history
            .read()
            .await
            .entries
            .iter()
            .filter_map(HistoryEntry::unaltered)
            .map(StoredMessage::message)
            .cloned()
            .collect()
    }

    /// Original content of all entries, including inserted summaries.
    pub async fn verbatim(&self) -> Vec<Message> {
        self.history
            .read()
            .await
            .entries
            .iter()
            .map(HistoryEntry::verbatim)
            .map(StoredMessage::message)
            .cloned()
            .collect()
    }

    pub async fn add(&self, message: &Message) {
        self.add_stored(
            StoredMessage::builder()
                .timestamp(jiff::Timestamp::now())
                .message(message.clone())
                .build(),
        )
        .await;
    }

    pub async fn add_stored(&self, message: StoredMessage) {
        let mut state = self.history.write().await;
        state
            .entries
            .push(HistoryEntry::builder().source(message).build());
        state.generation += 1;
    }

    /// Applies a pass atomically; failure, cancellation, or conflict commits nothing.
    pub async fn alter(&self, method: impl Alteration) -> Result<(), HistoryError> {
        let snapshot = self.snapshot().await;
        let mut edits = Vec::new();
        for (index, entry) in snapshot.entries().iter().enumerate() {
            if let Some(message) = entry.current()
                && let Some(message) = method.call(message).await.context(TransformationSnafu)?
            {
                edits.push(CompactionEdit::Replace { index, message });
            }
        }
        self.commit(snapshot, CompactionPlan::builder().edits(edits).build())
            .await
    }

    /// Validates and atomically commits a plan, rejecting any concurrent mutation.
    pub async fn compact(&self, method: impl Compactor) -> Result<(), HistoryError> {
        let snapshot = self.snapshot().await;
        let plan = method.call(&snapshot).await.context(TransformationSnafu)?;
        self.commit(snapshot, plan).await
    }

    async fn commit(
        &self,
        snapshot: HistorySnapshot,
        plan: CompactionPlan,
    ) -> Result<(), HistoryError> {
        let generation = snapshot.generation;
        let changed = !plan.edits.is_empty();
        let entries = snapshot.into_entries(plan)?;
        let mut state = self.history.write().await;
        ensure!(state.generation == generation, ConflictSnafu);
        if changed {
            state.entries = entries;
            state.generation += 1;
        }
        Ok(())
    }
}
