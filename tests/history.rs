#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use animist::history::{
    Alteration, CompactionEdit, CompactionPlan, Compactor, ContentBlock, History, HistoryChange,
    HistoryEntry, HistoryError, HistorySnapshot, Message, MessageKind, Role, StoredMessage,
};
use async_trait::async_trait;
use tokio::sync::Notify;

fn message(text: &str) -> Message {
    Message::builder()
        .role(Role::User)
        .content(vec![ContentBlock::Text { text: text.into() }])
        .kind(MessageKind::Turn)
        .build()
}

fn stored(text: &str) -> StoredMessage {
    StoredMessage::builder()
        .timestamp("2026-01-01T00:00:00Z".parse::<jiff::Timestamp>().unwrap())
        .message(message(text))
        .tags(BTreeMap::from([("origin".into(), "test".into())]))
        .build()
}

struct Rewrite(&'static str);

#[async_trait]
impl Alteration for Rewrite {
    async fn call(&self, current: &StoredMessage) -> Result<Option<StoredMessage>, animist::Error> {
        assert_eq!(current.tags()["origin"], "test");
        let mut revised = current.clone();
        *revised.message_mut() = message(self.0);
        Ok(Some(revised))
    }
}

#[tokio::test]
async fn change_feed_preserves_metadata_and_orders_edits_before_shifted_insertions() {
    let history = History::new();
    let start = history.snapshot().await.generation();
    history.add_stored(stored("a")).await;
    history.add_stored(stored("b")).await;
    history.alter(Rewrite("revised")).await.unwrap();
    history
        .compact(Plan(vec![
            CompactionEdit::Insert {
                before: 2,
                message: stored("end"),
            },
            CompactionEdit::Insert {
                before: 0,
                message: stored("first"),
            },
            CompactionEdit::Replace {
                index: 1,
                message: stored("replacement"),
            },
            CompactionEdit::Insert {
                before: 0,
                message: stored("second"),
            },
            CompactionEdit::Remove { index: 0 },
        ]))
        .await
        .unwrap();
    let entry = |text| HistoryEntry::builder().source(stored(text)).build();
    let changes = history.changes_since(&start).await.unwrap();
    assert_eq!(
        changes.changes(),
        &[
            HistoryChange::Appended {
                index: 0,
                entry: entry("a")
            },
            HistoryChange::Appended {
                index: 1,
                entry: entry("b")
            },
            HistoryChange::Revised {
                index: 0,
                message: stored("revised")
            },
            HistoryChange::Revised {
                index: 1,
                message: stored("revised")
            },
            HistoryChange::Revised {
                index: 1,
                message: stored("replacement")
            },
            HistoryChange::Deactivated { index: 0 },
            HistoryChange::Inserted {
                index: 0,
                entry: entry("first")
            },
            HistoryChange::Inserted {
                index: 1,
                entry: entry("second")
            },
            HistoryChange::Appended {
                index: 4,
                entry: entry("end")
            },
        ]
    );
    assert_eq!(changes.generation(), history.snapshot().await.generation());
    assert_eq!(
        history
            .changes_since(&changes.generation())
            .await
            .unwrap()
            .changes(),
        []
    );
    let before_noop = changes.generation();
    history.compact(Plan(vec![])).await.unwrap();
    assert_eq!(before_noop, history.snapshot().await.generation());
    assert!(
        history
            .compact(Plan(vec![CompactionEdit::Remove { index: 99 }]))
            .await
            .is_err()
    );
    assert_eq!(before_noop, history.snapshot().await.generation());
}

#[tokio::test]
async fn change_cursors_expire_at_whole_commit_boundaries_and_restores_start_fresh() {
    let history = History::new();
    let start = history.snapshot().await.generation();
    history
        .compact(Plan(vec![
            CompactionEdit::Insert {
                before: 0,
                message: stored("first of pair"),
            },
            CompactionEdit::Insert {
                before: 0,
                message: stored("second of pair"),
            },
        ]))
        .await
        .unwrap();
    let after_pair = history.snapshot().await.generation();
    for _ in 0..1022 {
        history.add_stored(stored("a")).await;
    }
    assert_eq!(
        history.changes_since(&start).await.unwrap().changes().len(),
        1024
    );
    history.add_stored(stored("overflow")).await;
    assert!(matches!(
        history.changes_since(&start).await,
        Err(HistoryError::ChangesTooOld { .. })
    ));
    let retained = history.changes_since(&after_pair).await.unwrap();
    assert_eq!(retained.changes().len(), 1023);
    assert!(matches!(
        retained.changes().first(),
        Some(HistoryChange::Appended { index: 2, .. })
    ));
    let before_large_commit = history.snapshot().await.generation();
    history.alter(Rewrite("large")).await.unwrap();
    assert!(matches!(
        history.changes_since(&before_large_commit).await,
        Err(HistoryError::ChangesTooOld { .. })
    ));
    let snapshot = history.snapshot().await;
    assert_eq!(
        history
            .changes_since(&snapshot.generation())
            .await
            .unwrap()
            .changes(),
        []
    );
    let restored = History::from_entries(snapshot.entries().to_vec());
    assert!(matches!(
        restored.changes_since(&snapshot.generation()).await,
        Err(HistoryError::ForeignVersion { .. })
    ));
    assert_eq!(
        restored
            .changes_since(&restored.snapshot().await.generation())
            .await
            .unwrap()
            .changes(),
        []
    );
    history.add_stored(stored("after oversized")).await;
    assert_eq!(
        history
            .clone()
            .changes_since(&snapshot.generation())
            .await
            .unwrap()
            .changes()
            .len(),
        1
    );
}

#[tokio::test]
async fn concurrent_change_readers_receive_consistent_cursors_and_complete_commits() {
    let history = History::new();
    let reader = |history: History| async move {
        let mut version = history.snapshot().await.generation();
        let mut next_index = 0;
        while next_index < 100 {
            let changes = history.changes_since(&version).await.unwrap();
            assert_eq!(changes.changes().len() % 2, 0, "a commit was split");
            for change in changes.changes() {
                let HistoryChange::Appended { index, .. } = change else {
                    panic!("unexpected non-append event: {change:?}");
                };
                assert_eq!(*index, next_index);
                next_index += 1;
            }
            version = changes.generation();
            tokio::task::yield_now().await;
        }
    };
    let writer = async {
        for pair in 0..50 {
            history
                .compact(Plan(vec![
                    CompactionEdit::Insert {
                        before: pair * 2,
                        message: stored("first of pair"),
                    },
                    CompactionEdit::Insert {
                        before: pair * 2,
                        message: stored("second of pair"),
                    },
                ]))
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(reader(history.clone()), reader(history.clone()), writer);
    })
    .await
    .unwrap();
}

struct Plan(Vec<CompactionEdit>);

#[async_trait]
impl Compactor for Plan {
    async fn call(&self, _history: &HistorySnapshot) -> Result<CompactionPlan, animist::Error> {
        Ok(CompactionPlan::builder().edits(self.0.clone()).build())
    }
}

#[tokio::test]
async fn revisions_and_originals_survive_deactivation_and_restore() {
    let history = History::new();
    history.add_stored(stored("original")).await;
    history.alter(Rewrite("first")).await.unwrap();
    history.alter(Rewrite("second")).await.unwrap();
    assert_eq!(history.current().await, vec![message("second")]);
    assert_eq!(history.unaltered().await, vec![message("original")]);
    history
        .compact(Plan(vec![CompactionEdit::Remove { index: 0 }]))
        .await
        .unwrap();
    assert_eq!(history.current().await, Vec::new());
    assert_eq!(history.unaltered().await, Vec::new());
    assert_eq!(history.verbatim().await, vec![message("original")]);

    let snapshot = history.snapshot().await;
    let entry = &snapshot.entries()[0];
    assert!(!entry.is_active());
    assert_eq!(entry.revisions(), &[stored("first"), stored("second")]);
    assert_eq!(entry.latest(), &stored("second"));
    assert_eq!(entry.verbatim(), &stored("original"));
    let encoded = serde_json::to_string(snapshot.entries()).unwrap();
    let restored = History::from_entries(serde_json::from_str(&encoded).unwrap());
    assert_eq!(restored.snapshot().await.entries(), snapshot.entries());
    restored.alter(Rewrite("ignored")).await.unwrap();
    assert_eq!(restored.snapshot().await.entries(), snapshot.entries());
}

#[tokio::test]
async fn conversation_plan_summarizes_ranges_and_preserves_snapshot_order() {
    let history = History::default();
    history.add_stored(stored("a")).await;
    history.add_stored(stored("b")).await;
    history.add_stored(stored("c")).await;
    let before = history.snapshot().await;
    history
        .compact(Plan(vec![
            CompactionEdit::Remove { index: 0 },
            CompactionEdit::Remove { index: 1 },
            CompactionEdit::Insert {
                before: 0,
                message: stored("summary"),
            },
            CompactionEdit::Insert {
                before: 0,
                message: stored("context"),
            },
            CompactionEdit::Replace {
                index: 2,
                message: stored("revised c"),
            },
            CompactionEdit::Insert {
                before: 3,
                message: stored("end"),
            },
        ]))
        .await
        .unwrap();
    assert_eq!(
        history.current().await,
        vec![
            message("summary"),
            message("context"),
            message("revised c"),
            message("end"),
        ]
    );
    assert_eq!(
        history.verbatim().await,
        vec![
            message("summary"),
            message("context"),
            message("a"),
            message("b"),
            message("c"),
            message("end"),
        ]
    );
    assert_eq!(before.entries().len(), 3);
    assert!(before.entries().iter().all(HistoryEntry::is_active));
}

#[tokio::test]
async fn invalid_plans_are_rejected_without_partial_changes() {
    let history = History::new();
    history.add_stored(stored("a")).await;
    let original = history.snapshot().await;
    for invalid in [
        CompactionEdit::Remove { index: 1 },
        CompactionEdit::Insert {
            before: 2,
            message: stored("invalid"),
        },
        CompactionEdit::Replace {
            index: 0,
            message: stored("duplicate"),
        },
    ] {
        let error = history
            .compact(Plan(vec![CompactionEdit::Remove { index: 0 }, invalid]))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            HistoryError::InvalidIndex { .. } | HistoryError::DuplicateEdit { .. }
        ));
        assert_eq!(history.snapshot().await.entries(), original.entries());
    }
    history
        .compact(Plan(vec![CompactionEdit::Remove { index: 0 }]))
        .await
        .unwrap();
    assert!(matches!(
        history
            .compact(Plan(vec![CompactionEdit::Replace {
                index: 0,
                message: stored("inactive")
            }]))
            .await,
        Err(HistoryError::InactiveEntry { index: 0, .. })
    ));
}

struct FailOnSecond;

#[async_trait]
impl Alteration for FailOnSecond {
    async fn call(&self, current: &StoredMessage) -> Result<Option<StoredMessage>, animist::Error> {
        if current.message() == &message("b") {
            return Err(animist::Error::Other {
                source: Box::new(std::io::Error::other("failed transformation")),
            });
        }
        Ok(Some(stored("modified")))
    }
}

#[tokio::test]
async fn callback_failure_preserves_source_and_commits_nothing() {
    let history = History::new();
    history.add(message("a")).await;
    history.add(message("b")).await;
    let error = history.alter(FailOnSecond).await.unwrap_err();
    let HistoryError::Transformation {
        source: animist::Error::Other { source },
    } = error
    else {
        panic!("unexpected error")
    };
    assert!(source.downcast_ref::<std::io::Error>().is_some());
    assert_eq!(history.current().await, vec![message("a"), message("b")]);
}

struct PausedCompactor {
    history: History,
    entered: Arc<Notify>,
    resume: Arc<Notify>,
}

struct PausedAlteration {
    history: History,
    entered: Arc<Notify>,
    resume: Arc<Notify>,
}

#[async_trait]
impl Alteration for PausedAlteration {
    async fn call(&self, current: &StoredMessage) -> Result<Option<StoredMessage>, animist::Error> {
        assert_ne!(self.history.current().await, Vec::new());
        if current.message() == &message("b") {
            self.entered.notify_one();
            self.resume.notified().await;
        }
        Ok(Some(stored("modified")))
    }
}

#[tokio::test]
async fn alteration_cancellation_and_conflict_discard_earlier_pending_revisions() {
    for cancel in [true, false] {
        let history = History::new();
        history.add(message("a")).await;
        history.add(message("b")).await;
        let entered = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let alteration = PausedAlteration {
            history: history.clone(),
            entered: Arc::clone(&entered),
            resume: Arc::clone(&resume),
        };
        let task_history = history.clone();
        let task = tokio::spawn(async move { task_history.alter(alteration).await });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert_eq!(history.current().await, vec![message("a"), message("b")]);
        } else {
            tokio::time::timeout(Duration::from_secs(2), history.add(message("c")))
                .await
                .unwrap();
            resume.notify_one();
            assert!(matches!(
                task.await.unwrap(),
                Err(HistoryError::Conflict { .. })
            ));
            assert_eq!(
                history.current().await,
                vec![message("a"), message("b"), message("c")]
            );
        }
        assert!(
            history
                .snapshot()
                .await
                .entries()
                .iter()
                .all(|entry| entry.revisions().is_empty())
        );
    }
}

#[async_trait]
impl Compactor for PausedCompactor {
    async fn call(&self, snapshot: &HistorySnapshot) -> Result<CompactionPlan, animist::Error> {
        assert_eq!(self.history.current().await.len(), snapshot.entries().len());
        self.entered.notify_one();
        self.resume.notified().await;
        Ok(CompactionPlan::builder()
            .edits(vec![CompactionEdit::Remove { index: 0 }])
            .build())
    }
}

#[tokio::test]
async fn callbacks_allow_reads_and_appends_and_stale_commits_conflict() {
    let history = History::new();
    history.add(message("a")).await;
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let task_history = history.clone();
    let compactor = PausedCompactor {
        history: history.clone(),
        entered: Arc::clone(&entered),
        resume: Arc::clone(&resume),
    };
    let task = tokio::spawn(async move { task_history.compact(compactor).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), history.add(message("b")))
        .await
        .unwrap();
    resume.notify_one();
    assert!(matches!(
        task.await.unwrap(),
        Err(HistoryError::Conflict { .. })
    ));
    assert_eq!(history.current().await, vec![message("a"), message("b")]);
}

#[tokio::test]
async fn cancelling_a_callback_commits_nothing() {
    let history = History::new();
    history.add(message("a")).await;
    let entered = Arc::new(Notify::new());
    let task_history = history.clone();
    let compactor = PausedCompactor {
        history: history.clone(),
        entered: Arc::clone(&entered),
        resume: Arc::new(Notify::new()),
    };
    let task = tokio::spawn(async move { task_history.compact(compactor).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(history.current().await, vec![message("a")]);
}

#[tokio::test]
async fn defaults_and_empty_history_insertions_work() {
    let value = serde_json::json!({ "source": stored("a") });
    let entry: HistoryEntry = serde_json::from_value(value).unwrap();
    assert!(entry.is_active());
    assert_eq!(entry.revisions(), Vec::new());
    assert_eq!(entry, HistoryEntry::builder().source(stored("a")).build());
    let mut value = serde_json::to_value(stored("a")).unwrap();
    value.as_object_mut().unwrap().remove("tags");
    assert!(
        serde_json::from_value::<StoredMessage>(value)
            .unwrap()
            .tags()
            .is_empty()
    );

    let history = History::new();
    history.compact(Plan(vec![])).await.unwrap();
    history
        .compact(Plan(vec![CompactionEdit::Insert {
            before: 0,
            message: stored("first"),
        }]))
        .await
        .unwrap();
    assert_eq!(history.current().await, vec![message("first")]);
}

struct Revise {
    expected: StoredMessage,
    replacement: StoredMessage,
}

#[async_trait]
impl Alteration for Revise {
    async fn call(&self, current: &StoredMessage) -> Result<Option<StoredMessage>, animist::Error> {
        assert_eq!(current, &self.expected);
        Ok(Some(self.replacement.clone()))
    }
}

#[tokio::test]
async fn revisions_preserve_metadata_and_callbacks_receive_the_latest_version() {
    let original = stored("original");
    let first = StoredMessage::builder()
        .timestamp("2026-01-02T00:00:00Z".parse::<jiff::Timestamp>().unwrap())
        .message(original.message().clone())
        .tags(BTreeMap::from([("stage".into(), "annotated".into())]))
        .build();
    let mut second = first.clone();
    *second.message_mut() = message("revised");
    second.tags_mut().insert("stage".into(), "altered".into());
    let third = StoredMessage::builder()
        .timestamp("2026-01-03T00:00:00Z".parse::<jiff::Timestamp>().unwrap())
        .message(message("compacted"))
        .tags(BTreeMap::from([("stage".into(), "compacted".into())]))
        .build();
    let history = History::new();
    history.add_stored(original.clone()).await;
    history
        .alter(Revise {
            expected: original.clone(),
            replacement: first.clone(),
        })
        .await
        .unwrap();
    assert_eq!(history.current().await, vec![message("original")]);
    history
        .alter(Revise {
            expected: first.clone(),
            replacement: second.clone(),
        })
        .await
        .unwrap();
    history
        .compact(Plan(vec![CompactionEdit::Replace {
            index: 0,
            message: third.clone(),
        }]))
        .await
        .unwrap();
    let snapshot = history.snapshot().await;
    let entry = &snapshot.entries()[0];
    assert_eq!(entry.current(), Some(&third));
    assert_eq!(entry.revisions(), &[first, second, third]);
    assert_eq!(entry.verbatim(), &original);
    assert_eq!(history.current().await, vec![message("compacted")]);
    assert_eq!(history.unaltered().await, vec![message("original")]);
    history
        .compact(Plan(vec![CompactionEdit::Remove { index: 0 }]))
        .await
        .unwrap();
    let snapshot = history.snapshot().await;
    let restored = History::from_entries(
        serde_json::from_str(&serde_json::to_string(snapshot.entries()).unwrap()).unwrap(),
    );
    assert_eq!(restored.snapshot().await.entries(), snapshot.entries());
}

#[tokio::test]
async fn insert_positions_include_both_boundaries_but_not_beyond_length() {
    let history = History::new();
    history.add_stored(stored("original")).await;
    history
        .compact(Plan(vec![
            CompactionEdit::Insert {
                before: 1,
                message: stored("append"),
            },
            CompactionEdit::Insert {
                before: 0,
                message: stored("prepend"),
            },
            CompactionEdit::Insert {
                before: 1,
                message: stored("append again"),
            },
        ]))
        .await
        .unwrap();
    assert_eq!(
        history.current().await,
        vec![
            message("prepend"),
            message("original"),
            message("append"),
            message("append again"),
        ]
    );
    assert!(matches!(
        history
            .compact(Plan(vec![CompactionEdit::Insert {
                before: 5,
                message: stored("invalid"),
            }]))
            .await,
        Err(HistoryError::InvalidIndex {
            index: 5,
            len: 4,
            ..
        })
    ));
}
