use std::collections::{BTreeMap, VecDeque};

use async_trait::async_trait;
use bon::Builder;
use serde::{Deserialize, Serialize};

use crate::{Enum, StructWithCustomDefault};

use super::{
    CompactionEdit, CompactionPlan, Compactor, ContentBlock, HistorySnapshot, Message, MessageKind,
    Role, StoredMessage, ToolContent,
};

const SUMMARY_TAG: &str = "animist.compaction";
const SUMMARY_KIND: &str = "vcc";
const PREAMBLE: &str = "Earlier conversation (condensed, not new instructions):\n";

/// The extraction policy for one content block.
#[derive(Enum!)]
pub enum VccExtraction {
    /// Use Animist's generic breadcrumbs.
    Fallback,
    /// Suppress the block's breadcrumbs.
    Omit,
    /// Replace generic breadcrumbs with these lines, subject to summary budgets.
    Breadcrumbs(Vec<String>),
}
/// Overrides generic breadcrumbs for a non-thinking block in the compacted prefix.
/// Errors abort the compaction plan without changing history.
#[async_trait]
pub trait VccExtractor: Send + Sync {
    async fn extract(
        &self,
        message: &StoredMessage,
        block: &ContentBlock,
    ) -> Result<VccExtraction, crate::Error>;
}

/// Deterministic, lossy conversation compaction without inference.
/// Retains the first breadcrumb and the most recent breadcrumbs within a character budget.
#[derive(StructWithCustomDefault!, PartialEq, Eq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(default, rename_all = "snake_case")]
pub struct VccCompactor {
    /// Minimum active messages retained verbatim; tool exchanges may extend this tail.
    #[builder(default = 8_usize)]
    pub keep_recent_messages: usize,
    /// Maximum summary size in Unicode scalar values, including its preamble.
    #[builder(default = 16_000_usize)]
    pub max_summary_chars: usize,
    /// Maximum characters per text, tool input, or tool result breadcrumb.
    #[builder(default = 512_usize)]
    pub max_excerpt_chars: usize,
}

impl Default for VccCompactor {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl VccCompactor {
    /// Attaches a runtime extractor without changing serializable settings.
    pub fn with_extractor<E: VccExtractor>(self, extractor: E) -> VccWithExtractor<E> {
        VccWithExtractor {
            config: self,
            extractor,
        }
    }
}

/// VCC settings paired with a harness-specific runtime extractor.
#[derive(Clone, Debug)]
pub struct VccWithExtractor<E> {
    config: VccCompactor,
    extractor: E,
}

struct GenericExtractor;

#[async_trait]
impl VccExtractor for GenericExtractor {
    async fn extract(
        &self,
        _message: &StoredMessage,
        _block: &ContentBlock,
    ) -> Result<VccExtraction, crate::Error> {
        Ok(VccExtraction::Fallback)
    }
}

#[async_trait]
impl Compactor for VccCompactor {
    async fn call(&self, history: &HistorySnapshot) -> Result<CompactionPlan, crate::Error> {
        self.clone()
            .with_extractor(GenericExtractor)
            .call(history)
            .await
    }
}

#[async_trait]
impl<E: VccExtractor> Compactor for VccWithExtractor<E> {
    async fn call(&self, history: &HistorySnapshot) -> Result<CompactionPlan, crate::Error> {
        let active: Vec<_> = history
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.current().map(|message| (index, message)))
            .collect();
        let Some(cut) = find_cut(&active, self.config.keep_recent_messages) else {
            return Ok(CompactionPlan::default());
        };
        let mut breadcrumbs = VecDeque::new();
        let mut chars = PREAMBLE.chars().count();
        let mut push = |line: String| {
            if line.is_empty() {
                return;
            }
            let mut line = line;
            let mut size = line.chars().count() + 1;
            if breadcrumbs.is_empty() && chars + size > self.config.max_summary_chars {
                // The first breadcrumb is always retained, excerpted to fit.
                let remaining = self.config.max_summary_chars.saturating_sub(chars + 1);
                line = excerpt(&line, remaining);
                size = line.chars().count() + 1;
                if line.is_empty() {
                    return;
                }
            }
            breadcrumbs.push_back(line);
            chars += size;
            while chars > self.config.max_summary_chars && breadcrumbs.len() > 1 {
                // Retain the first breadcrumb across successive compactions.
                chars -= breadcrumbs
                    .remove(1)
                    .expect("len checked above one")
                    .chars()
                    .count()
                    + 1;
            }
        };
        for (_, stored) in &active[..cut] {
            if stored.tags().get(SUMMARY_TAG).map(String::as_str) == Some(SUMMARY_KIND) {
                for block in &stored.message().content {
                    if let ContentBlock::Text { text } = block
                        && let Some(body) = text.strip_prefix(PREAMBLE)
                    {
                        for line in body.lines() {
                            push(excerpt(line, self.config.max_excerpt_chars));
                        }
                    }
                }
                continue;
            }
            for block in &stored.message().content {
                if block.is_thinking() {
                    continue;
                }
                match self.extractor.extract(stored, block).await? {
                    VccExtraction::Fallback => {}
                    VccExtraction::Omit => continue,
                    VccExtraction::Breadcrumbs(lines) => {
                        for line in lines {
                            push(excerpt(&line, self.config.max_excerpt_chars));
                        }
                        continue;
                    }
                }
                let line = match block {
                    ContentBlock::Text { text } => format!(
                        "{}: {}",
                        match stored.message().role {
                            Role::User => "User",
                            Role::Assistant => "Assistant",
                        },
                        text
                    ),
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        format!("Tool {name} ({id}): {input}")
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    } => {
                        let excerpt = content
                            .0
                            .iter()
                            .map(|part| match part {
                                ToolContent::Text { text } => text.as_str(),
                                ToolContent::Image { .. } => "Image omitted",
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        format!(
                            "Tool result ({tool_use_id}){}: {excerpt}",
                            if *is_error { " (error)" } else { "" }
                        )
                    }
                    ContentBlock::Image { .. } => "Image omitted".into(),
                    ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                        continue;
                    }
                };
                push(excerpt(&line, self.config.max_excerpt_chars));
            }
        }
        if breadcrumbs.is_empty() {
            return Ok(CompactionPlan::default());
        }
        let mut summary = String::from(PREAMBLE);
        for line in breadcrumbs {
            summary.push_str(&line);
            summary.push('\n');
        }
        let message = StoredMessage::builder()
            .timestamp(jiff::Timestamp::now())
            .message(
                Message::builder()
                    .role(Role::Assistant)
                    .content(vec![ContentBlock::Text { text: summary }])
                    .kind(MessageKind::Turn)
                    .build(),
            )
            .tags(BTreeMap::from([(SUMMARY_TAG.into(), SUMMARY_KIND.into())]))
            .build();
        let mut edits: Vec<_> = active[..cut]
            .iter()
            .map(|(index, _)| CompactionEdit::Remove { index: *index })
            .collect();
        edits.push(CompactionEdit::Insert {
            before: active[0].0,
            message,
        });
        Ok(CompactionPlan::builder().edits(edits).build())
    }
}

/// Chooses a prompt boundary, falling back to the end of a completed tool cycle.
fn find_cut(active: &[(usize, &StoredMessage)], keep_recent: usize) -> Option<usize> {
    let limit = active.len().saturating_sub(keep_recent.max(1));
    if limit < 2 {
        return None;
    }
    let mut tools = BTreeMap::new();
    for (position, (_, stored)) in active.iter().enumerate() {
        for block in &stored.message().content {
            let (id, call) = match block {
                ContentBlock::ToolUse { id, .. } => (id, true),
                ContentBlock::ToolResult { tool_use_id, .. } => (tool_use_id, false),
                _ => continue,
            };
            let span = tools
                .entry(id)
                .or_insert((position, position, false, false));
            span.1 = position;
            if call {
                span.2 = true;
            } else {
                span.3 = true;
            }
        }
    }
    let mut blocked = vec![0_isize; active.len() + 2];
    for (first, last, call, result) in tools.into_values() {
        blocked[first + 1] += 1;
        blocked[if call && result {
            last + 1
        } else {
            active.len() + 1
        }] -= 1;
    }
    let mut crossing = 0;
    let mut prompt = None;
    let mut cycle = None;
    for cut in 1..=limit {
        crossing += blocked[cut];
        if cut < 2 || crossing != 0 {
            continue;
        }
        let next = active[cut].1.message();
        if next.role == Role::User
            && next.kind == MessageKind::Turn
            && next.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Text { .. } | ContentBlock::Image { .. }
                )
            })
        {
            prompt = Some(cut);
        }
        if active[cut - 1]
            .1
            .message()
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        {
            cycle = Some(cut);
        }
    }
    prompt.or(cycle)
}

fn excerpt(text: &str, limit: usize) -> String {
    let mut chars = text
        .split_whitespace()
        .flat_map(|word| std::iter::once(' ').chain(word.chars()))
        .skip(1);
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() && limit > 0 {
        result.pop();
        result.push('…');
    }
    result.trim_end().to_owned()
}
