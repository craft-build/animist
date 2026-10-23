use animist::history::{
    CompactionEdit, Compactor, ContentBlock, History, HistoryEntry, Message, MessageKind, Role,
    StoredMessage, VccCompactor, VccExtraction, VccExtractor, VccWithExtractor,
};
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, Notify};

fn message(role: Role, content: Vec<ContentBlock>) -> Message {
    Message::builder()
        .role(role)
        .content(content)
        .kind(MessageKind::Turn)
        .build()
}

fn text(role: Role, text: &str) -> Message {
    message(role, vec![ContentBlock::Text { text: text.into() }])
}

fn call(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: "arbitrary_tool".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        thought_signature: Some("opaque-signature".into()),
    }
}

fn result(id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.into(),
        content: "test failed".into(),
        is_error: true,
        loaded_tools: vec![],
    }
}

fn summary(messages: &[Message]) -> &str {
    let ContentBlock::Text { text } = &messages[0].content[0] else {
        panic!("missing summary")
    };
    text
}

async fn conversation() -> History {
    let history = History::new();
    history.add(&text(Role::User, "Implement login")).await;
    history
        .add(&message(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    thinking: "private reasoning".into(),
                    signature: Some("private signature".into()),
                },
                call("a"),
                call("b"),
            ],
        ))
        .await;
    history
        .add(&message(Role::User, vec![result("b"), result("a")]))
        .await;
    history.add(&text(Role::Assistant, "Fixed login")).await;
    history.add(&text(Role::User, "Now add tests")).await;
    history
}

#[tokio::test]
async fn compacts_with_native_history_and_preserves_sources_and_tail() {
    let history = conversation().await;
    let before = history.snapshot().await;
    let compactor = VccCompactor::builder()
        .keep_recent_messages(1_usize)
        .build();
    let first = compactor.call(&before).await.unwrap();
    let second = compactor.call(&before).await.unwrap();
    // Planning is deterministic apart from the generated summary timestamp.
    assert_eq!(first.edits.len(), second.edits.len());
    for (first, second) in first.edits.iter().zip(&second.edits) {
        match (first, second) {
            (
                CompactionEdit::Insert {
                    before: a,
                    message: first,
                },
                CompactionEdit::Insert {
                    before: b,
                    message: second,
                },
            ) => {
                assert_eq!(a, b);
                assert_eq!(first.message(), second.message());
                assert_eq!(first.tags(), second.tags());
            }
            _ => assert_eq!(first, second),
        }
    }
    history.compact(compactor).await.unwrap();
    let current = history.current().await;
    assert_eq!(current.len(), 2);
    assert_eq!(current[1], *before.entries()[4].latest().message());
    let summary = summary(&current);
    for expected in [
        "User: Implement login",
        "arbitrary_tool",
        "src/main.rs",
        "error",
        "Fixed login",
    ] {
        assert!(summary.contains(expected), "{expected}");
    }
    assert!(!summary.contains("private"));
    assert!(!summary.contains("opaque-signature"));
    let after = history.snapshot().await;
    assert_eq!(
        after.entries()[0].latest().tags()["animist.compaction"],
        "vcc"
    );
    for (original, retained) in before.entries().iter().zip(&after.entries()[1..]) {
        assert_eq!(original.verbatim(), retained.verbatim());
        assert_eq!(original.revisions(), retained.revisions());
    }
    assert!(after.entries()[1..5].iter().all(|entry| !entry.is_active()));
    assert_eq!(after.entries()[5], before.entries()[4]);
}

#[tokio::test]
async fn repeated_compaction_merges_tagged_summary_without_nesting_preambles() {
    let history = conversation().await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build(),
        )
        .await
        .unwrap();
    let unchanged = history.snapshot().await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(unchanged.entries(), history.snapshot().await.entries());
    history.add(&text(Role::Assistant, "Tests added")).await;
    history.add(&text(Role::User, "Ship it")).await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build(),
        )
        .await
        .unwrap();
    let current = history.current().await;
    let summary = summary(&current);
    assert!(summary.contains("Implement login"));
    assert!(summary.contains("Tests added"));
    assert_eq!(summary.matches("Earlier conversation").count(), 1);
    assert_eq!(current[1], text(Role::User, "Ship it"));
}

#[tokio::test]
async fn parallel_tool_exchange_crossing_prompt_extends_tail() {
    let history = conversation().await;
    history
        .add(&message(Role::Assistant, vec![call("c"), call("d")]))
        .await;
    history.add(&message(Role::User, vec![result("c")])).await;
    history
        .add(&text(Role::User, "Additional instructions"))
        .await;
    history.add(&message(Role::User, vec![result("d")])).await;
    history.add(&text(Role::Assistant, "Done")).await;
    let original = history.current().await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(3_usize)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(&history.current().await[1..], &original[4..]);
}

#[tokio::test]
async fn falls_back_to_complete_cycle_in_single_prompt_chain() {
    let history = History::new();
    history.add(&text(Role::User, "Work")).await;
    for id in ["a", "b", "c"] {
        history.add(&message(Role::Assistant, vec![call(id)])).await;
        history.add(&message(Role::User, vec![result(id)])).await;
    }
    history
        .add(&message(Role::Assistant, vec![call("pending")]))
        .await;
    let original = history.current().await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(2_usize)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(&history.current().await[1..], &original[5..]);
}

#[tokio::test]
async fn unresolved_calls_and_orphan_results_are_not_removed() {
    for block in [call("missing"), result("missing")] {
        let history = History::new();
        history.add(&text(Role::User, "Work")).await;
        history.add(&message(Role::Assistant, vec![block])).await;
        history.add(&text(Role::User, "Continue")).await;
        history.add(&text(Role::Assistant, "Done")).await;
        let original = history.snapshot().await;
        history
            .compact(
                VccCompactor::builder()
                    .keep_recent_messages(1_usize)
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(original.entries(), history.snapshot().await.entries());
    }
}

#[tokio::test]
async fn budgets_are_unicode_safe_and_keep_first_and_recent_breadcrumbs() {
    let history = History::new();
    history.add(&text(Role::User, "Original goal 日本語")).await;
    for _ in 0..20 {
        history
            .add(&text(Role::Assistant, &"é🦀 ".repeat(100)))
            .await;
    }
    history.add(&text(Role::Assistant, "Latest outcome")).await;
    history.add(&text(Role::User, "Next")).await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .max_summary_chars(200_usize)
                .max_excerpt_chars(40_usize)
                .build(),
        )
        .await
        .unwrap();
    let current = history.current().await;
    let summary = summary(&current);
    assert!(summary.chars().count() <= 200);
    assert!(summary.contains("Original goal 日本語"));
    assert!(summary.contains("Latest outcome"));
    assert!(summary.contains('…'));
}

#[tokio::test]
async fn large_history_shrinks_and_image_payloads_are_not_copied() {
    let history = History::new();
    history
        .add(&message(
            Role::User,
            vec![
                ContentBlock::Text {
                    text: "Analyze image".into(),
                },
                ContentBlock::Image {
                    source: animist::history::ImageSource::Base64 {
                        media_type: "image/png".into(),
                        data: "sensitive-image-payload".repeat(1000),
                    },
                },
            ],
        ))
        .await;
    history
        .add(&text(Role::Assistant, &"Repeated output ".repeat(10_000)))
        .await;
    history.add(&text(Role::User, "Continue")).await;
    let before = serde_json::to_vec(&history.current().await).unwrap().len();
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build(),
        )
        .await
        .unwrap();
    let current = history.current().await;
    assert!(serde_json::to_vec(&current).unwrap().len() < before);
    assert!(summary(&current).contains("Image omitted"));
    assert!(!summary(&current).contains("sensitive-image-payload"));
}

#[tokio::test]
async fn uses_latest_active_revisions_and_snapshot_indices() {
    let timestamp = jiff::Timestamp::now();
    let stored = |text_value| {
        StoredMessage::builder()
            .timestamp(timestamp)
            .message(text(Role::User, text_value))
            .build()
    };
    let history = History::from_entries(vec![
        HistoryEntry::builder()
            .source(stored("inactive"))
            .active(false)
            .build(),
        HistoryEntry::builder()
            .source(stored("original"))
            .revisions(vec![stored("revised")])
            .build(),
        HistoryEntry::builder().source(stored("answer")).build(),
        HistoryEntry::builder()
            .source(
                StoredMessage::builder()
                    .timestamp(timestamp)
                    .message(text(Role::User, "tail"))
                    .tags(BTreeMap::from([("custom".into(), "value".into())]))
                    .build(),
            )
            .build(),
    ]);
    let before = history.snapshot().await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build(),
        )
        .await
        .unwrap();
    let current = history.current().await;
    assert!(summary(&current).contains("revised"));
    assert!(!summary(&current).contains("original"));
    assert!(!summary(&current).contains("inactive"));
    let after = history.snapshot().await;
    assert_eq!(after.entries()[0], before.entries()[0]);
    assert_eq!(after.entries()[4], before.entries()[3]);
    assert_eq!(
        after.entries()[2].revisions(),
        before.entries()[1].revisions()
    );
}

#[tokio::test]
async fn empty_short_and_disabled_budgets_are_noops() {
    for compactor in [
        VccCompactor::default(),
        VccCompactor::builder()
            .keep_recent_messages(usize::MAX)
            .build(),
        VccCompactor::builder()
            .keep_recent_messages(0_usize)
            .max_summary_chars(0_usize)
            .build(),
        VccCompactor::builder()
            .keep_recent_messages(0_usize)
            .max_excerpt_chars(0_usize)
            .build(),
    ] {
        let history = conversation().await;
        let before = history.snapshot().await;
        history.compact(compactor).await.unwrap();
        assert_eq!(before.entries(), history.snapshot().await.entries());
    }
    let history = History::new();
    assert!(
        VccCompactor::default()
            .call(&history.snapshot().await)
            .await
            .unwrap()
            .edits
            .is_empty()
    );
}

#[test]
fn config_defaults_round_trip() {
    let defaults = VccCompactor::default();
    assert_eq!(defaults, VccCompactor::builder().build());
    assert_eq!(
        defaults,
        serde_json::from_str::<VccCompactor>("{}").unwrap()
    );
    assert_eq!(
        defaults,
        serde_json::from_str(&serde_json::to_string(&defaults).unwrap()).unwrap()
    );
}

struct HarnessExtractor {
    seen: Arc<Mutex<Vec<StoredMessage>>>,
}

#[async_trait]
impl VccExtractor for HarnessExtractor {
    async fn extract(
        &self,
        message: &StoredMessage,
        block: &ContentBlock,
    ) -> Result<VccExtraction, animist::Error> {
        assert!(!block.is_thinking());
        self.seen.lock().await.push(message.clone());
        Ok(match block {
            ContentBlock::ToolUse { input, .. } => VccExtraction::Breadcrumbs(vec![format!(
                "Modified {}: added login validation",
                input["path"].as_str().unwrap()
            )]),
            ContentBlock::ToolResult { .. } => VccExtraction::Omit,
            _ => VccExtraction::Fallback,
        })
    }
}

#[tokio::test]
async fn harness_extraction_overrides_omits_and_falls_back_without_reprocessing_summaries() {
    let history = conversation().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let compactor: VccWithExtractor<HarnessExtractor> = VccCompactor::builder()
        .keep_recent_messages(1_usize)
        .build()
        .with_extractor(HarnessExtractor { seen: seen.clone() });
    history.compact(compactor).await.unwrap();
    let current = history.current().await;
    let body = summary(&current);
    assert!(body.contains("Modified src/main.rs: added login validation"));
    assert!(body.contains("User: Implement login"));
    assert!(body.contains("Assistant: Fixed login"));
    assert!(!body.contains("arbitrary_tool"));
    assert!(!body.contains("test failed"));
    assert_eq!(seen.lock().await.len(), 6);
    assert_eq!(current[1], text(Role::User, "Now add tests"));

    seen.lock().await.clear();
    history.add(&text(Role::Assistant, "Tests added")).await;
    history.add(&text(Role::User, "Ship")).await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build()
                .with_extractor(HarnessExtractor { seen: seen.clone() }),
        )
        .await
        .unwrap();
    let current = history.current().await;
    assert!(summary(&current).contains("Modified src/main.rs"));
    let seen = seen.lock().await;
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].message(), &text(Role::User, "Now add tests"));
    assert_eq!(seen[1].message(), &text(Role::Assistant, "Tests added"));
}

struct MetadataExtractor;

#[async_trait]
impl VccExtractor for MetadataExtractor {
    async fn extract(
        &self,
        message: &StoredMessage,
        _block: &ContentBlock,
    ) -> Result<VccExtraction, animist::Error> {
        assert_eq!(message.tags()["harness"], "craft");
        assert_eq!(message.message(), &text(Role::User, "latest"));
        Ok(VccExtraction::Breadcrumbs(vec![
            "custom goal".into(),
            "é🦀\n ".repeat(100),
            "recent".into(),
        ]))
    }
}

#[tokio::test]
async fn extractor_receives_latest_metadata_and_cannot_bypass_budgets() {
    let stored = |message| {
        StoredMessage::builder()
            .timestamp(jiff::Timestamp::now())
            .message(message)
            .tags(BTreeMap::from([("harness".into(), "craft".into())]))
            .build()
    };
    let history = History::from_entries(vec![
        HistoryEntry::builder()
            .source(stored(text(Role::User, "inactive")))
            .active(false)
            .build(),
        HistoryEntry::builder()
            .source(stored(text(Role::User, "original")))
            .revisions(vec![stored(text(Role::User, "latest"))])
            .build(),
        HistoryEntry::builder()
            .source(stored(text(Role::User, "latest")))
            .build(),
        HistoryEntry::builder()
            .source(stored(text(Role::User, "tail")))
            .build(),
    ]);
    let before = history.snapshot().await;
    history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .max_summary_chars(120_usize)
                .max_excerpt_chars(20_usize)
                .build()
                .with_extractor(MetadataExtractor),
        )
        .await
        .unwrap();
    let current = history.current().await;
    let body = summary(&current);
    assert!(body.chars().count() <= 120);
    assert!(body.contains("custom goal"));
    assert!(body.contains("recent"));
    assert!(body.contains('…'));
    assert!(body.lines().skip(1).all(|line| line.chars().count() <= 20));
    let after = history.snapshot().await;
    assert_eq!(before.entries()[0], after.entries()[0]);
    assert_eq!(
        before.entries()[1].revisions(),
        after.entries()[2].revisions()
    );
    assert_eq!(before.entries()[3], after.entries()[4]);
}

#[derive(Debug, snafu::Snafu)]
#[snafu(display("Harness extraction failed"))]
struct ExtractionFailed;

struct FailingExtractor;

#[async_trait]
impl VccExtractor for FailingExtractor {
    async fn extract(
        &self,
        _message: &StoredMessage,
        block: &ContentBlock,
    ) -> Result<VccExtraction, animist::Error> {
        if matches!(block, ContentBlock::ToolUse { .. }) {
            return Err(animist::Error::Other {
                source: Box::new(ExtractionFailed),
            });
        }
        Ok(VccExtraction::Breadcrumbs(vec![
            "uncommitted breadcrumb".into(),
        ]))
    }
}

#[tokio::test]
async fn extraction_errors_preserve_source_and_commit_nothing() {
    let history = conversation().await;
    let before = history.snapshot().await;
    let error = history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build()
                .with_extractor(FailingExtractor),
        )
        .await
        .unwrap_err();
    let animist::history::HistoryError::Transformation {
        source: animist::Error::Other { source },
    } = error
    else {
        panic!("extraction error lost its source");
    };
    assert!(source.downcast_ref::<ExtractionFailed>().is_some());
    assert_eq!(before.entries(), history.snapshot().await.entries());
}

struct PausedExtractor {
    started: Arc<Notify>,
    resume: Arc<Notify>,
    pause_next: AtomicBool,
}

#[async_trait]
impl VccExtractor for PausedExtractor {
    async fn extract(
        &self,
        _message: &StoredMessage,
        _block: &ContentBlock,
    ) -> Result<VccExtraction, animist::Error> {
        if self.pause_next.swap(false, Ordering::SeqCst) {
            self.started.notify_one();
            self.resume.notified().await;
        }
        Ok(VccExtraction::Fallback)
    }
}

#[tokio::test]
async fn extraction_is_cancellable_without_committing_or_holding_history_lock() {
    let history = conversation().await;
    let before = history.snapshot().await;
    let started = Arc::new(Notify::new());
    let extractor = PausedExtractor {
        started: started.clone(),
        resume: Arc::new(Notify::new()),
        pause_next: AtomicBool::new(true),
    };
    let task = tokio::spawn({
        let history = history.clone();
        async move {
            history
                .compact(
                    VccCompactor::builder()
                        .keep_recent_messages(1_usize)
                        .build()
                        .with_extractor(extractor),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), history.snapshot())
        .await
        .unwrap();
    assert_eq!(before.entries(), snapshot.entries());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(before.entries(), history.snapshot().await.entries());
}

#[tokio::test]
async fn concurrent_append_rejects_extractor_plan_as_stale() {
    let history = conversation().await;
    let before = history.snapshot().await;
    let started = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let extractor = PausedExtractor {
        started: started.clone(),
        resume: resume.clone(),
        pause_next: AtomicBool::new(true),
    };
    let task = tokio::spawn({
        let history = history.clone();
        async move {
            history
                .compact(
                    VccCompactor::builder()
                        .keep_recent_messages(1_usize)
                        .build()
                        .with_extractor(extractor),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        history.add(&text(Role::User, "Concurrent prompt")),
    )
    .await
    .unwrap();
    resume.notify_one();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap(),
        Err(animist::history::HistoryError::Conflict)
    ));
    let after = history.snapshot().await;
    assert_eq!(before.entries(), &after.entries()[..before.entries().len()]);
    assert_eq!(
        after.entries().last().unwrap().latest().message(),
        &text(Role::User, "Concurrent prompt")
    );
}
