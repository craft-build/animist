# History

`History` is a shared handle: cloning it shares live state. `History::new()` and
`Default` create an empty history. `snapshot()` returns an independent owned view,
including inactive entries, originals, and revisions with their metadata.

## Entries and views

### At-depth system instructions

`Role::System` represents instruction authority at a specific history position.
Use text blocks only; replay validation rejects images, thinking, calls, and
results in system messages with `ReplayError::NonTextSystemContent` and the
original message/block location. `MessageKind` does not determine authority.

```rust
use animist::history::{ContentBlock, Message, MessageKind, Role};

let instruction = Message::builder()
    .role(Role::System)
    .kind(MessageKind::Observation)
    .content(vec![ContentBlock::Text { text: "Keep the moon red.".into() }])
    .build();
```

Place this message in the slice passed to `stream_message()` or insert a stored
message with `CompactionEdit::Insert { before, message }`. The separate `system`
argument remains supported and precedes history instructions. See
[protocol placement limitations](providers-and-protocols.md#system-message-placement).

System messages serialize with `"role": "system"` using the existing message
shape. Existing `"user"` and `"assistant"` messages remain readable with no
migration. Older consumers that only understand those roles cannot read system
messages. Update exhaustive `Role` matches in downstream Rust code.

`Alteration` receives system entries just like other active messages.
Custom compactors may insert, replace, or explicitly remove them with the usual
revision semantics; the manager does not enforce an instruction-retention policy.
Custom transforms must preserve instruction authority and placement unless an
intentional caller policy changes them.

Each entry retains its original `StoredMessage`, a sequence of stored revisions,
and an independent activation flag. Removing an entry deactivates it without
discarding its original or revisions.

| View | Content | Entries |
| --- | --- | --- |
| `current()` | Latest revision, or original when unrevised | Active |
| `unaltered()` | Original | Active |
| `verbatim()` | Original | All |

Revisions contain `StoredMessage`, preserving each version's content, timestamp,
and tags. Application replacements never change the original or earlier revisions.
`HistoryEntry::latest()` also exposes the latest stored revision of inactive
entries for inspection. The history views return `Message` for inference; use
snapshot entries to inspect metadata.

`add()` timestamps a new message. `add_stored()` accepts caller-supplied metadata.
For persistence, serialize `snapshot().entries()` and deserialize into
`Vec<HistoryEntry>`, then restore with `History::from_entries()`. The live lock and
generation are runtime state, not persisted settings.

The entry serialization shape replaces the earlier tagged
`Active`/`Inactive`/`Altered` representation; no legacy-format migration is
provided.

### Agent tool-result revision retention

Before tools execute, the agent stores a source message with an interrupted
placeholder for every call. Each completion appends a progress revision, with
results kept in call order. Once every call has a result (including errors,
cancellations, and skipped calls), the agent squashes its intermediate revisions,
retaining exactly the placeholder source and one final stored revision, with
their timestamps and tags unchanged. Squashing precedes `StepFinished` and
step-end callbacks, including when a tool failure ends the run.

This is an agent-only completion policy, not a general compaction edit: it avoids
expanding the public edit API for a runtime-owned lifecycle. Application-created
revisions from alterations and compaction `Replace` are never implicitly
squashed, including on older tool-result entries. If an application changes the
in-flight result entry, the agent reports a conflict instead of pruning it.
Squashing commits under the history generation check, just like other edits.

Dropping the run before completion leaves the placeholder source and all progress
revisions committed so far. The latest revision keeps completed results and
explicit interrupted placeholders for unfinished calls. Cooperative cancellation
that finishes closing every call retains the source and one final revision.
There is no snapshot or serialization format change; already captured snapshots
and older persisted entries retain their existing revision sequences.

### Tool-result content compatibility

`ContentBlock::ToolResult::content` is now `ToolContents` rather than `String`.
It serializes as an ordered array of tagged `ToolContent` blocks:

```json
[
  {"type": "text", "text": "Screenshot"},
  {"type": "image", "source": {"type": "url", "url": "https://example.com/image.png"}}
]
```

Deserialization also accepts the legacy string form and converts it to a single
text block, including empty strings. Saving loaded history migrates originals
and revisions to arrays; older readers that require strings cannot read these
new saves. Upgrade readers before writing the new format and retain a backup
if rollback is needed. There is no lossless downgrade for image results.
This does not migrate the older entry-level tagged representation above.

The Rust API is source-breaking for direct field construction: use
`content: text.into()` or `content: vec![ToolContent::Text { text }].into()`.
Images, errors, and activation metadata remain intact in stored originals and
revisions. Compaction only changes them when explicitly requested by the caller.
VCC emits omission markers for tool-result images in its lossy summary, while
retaining the full removed sources and revisions; protected tail content is
unchanged.

## Alteration

`Alteration::call()` receives the latest active `StoredMessage`, including its
timestamp and tags. Return `Ok(None)` to keep it, `Ok(Some(message))` to append a
stored revision, or an error to abort the entire pass. Inactive entries are
skipped. For content-only or tag-only changes, clone the input and edit through
`message_mut()` or `tags_mut()` to retain the other fields. A new timestamp is
explicitly supplied by constructing a new stored revision; it is not generated
by the manager.

## Compaction

`Compactor::call()` receives the whole `HistorySnapshot`. Both LLM-driven
summarization and dedicated algorithms use the same contract: return a
`CompactionPlan` rather than mutating live history.

All edit indices refer to the original snapshot's `entries()` slice, including
inactive entries. They are not indices into a filtered active view and do not
shift as edits are processed.

- `Remove { index }` deactivates an active entry.
- `Replace { index, message }` appends a `StoredMessage` revision to an active entry,
  including the replacement's timestamp and tags.
- `Insert { before, message }` creates an active entry before a snapshot index.
  Use the snapshot length to append.
- Entries omitted from the plan are kept.

Existing entries may be replaced or removed at most once per plan. Multiple
insertions at the same position are permitted and retain plan order. Out-of-range
indices, duplicate edits, and modifications to inactive entries are rejected.

To summarize a range, remove its active entries and insert a summary at the
desired snapshot position. The source entries remain available for inspection;
the summary is a new entry with its own original, so it also appears in
`verbatim()`. Compactors may use tags to mark generated summaries.

The manager validates structural edits, not provider-specific replay rules.
Compactors must preserve valid tool-call/result relationships and any reasoning
payloads required by the selected protocol, or intentionally rewrite the whole
related exchange.

`History::from_entries()` restores data without performing inference validation.
Use `protocol::validate_replay(&history.current().await, &tools)` to check an
active view explicitly; the agent performs this check before inference and again
after request transformations. Inactive originals and superseded revisions are
not inference input and are not rejected for obsolete or incomplete exchanges.
Errors identify zero-based message/block positions in the supplied view, not
indices into the full snapshot.

Validation never compacts, removes, revises, or fabricates content. A compaction
plan that removes only one side of a tool exchange may commit as a history edit
but will fail replay validation before inference. Repair requires an explicit
caller edit; source messages and revisions remain available for that decision.

### VCC

`VccCompactor` implements deterministic, no-LLM compaction using the latest active
stored messages:

```rust
use animist::history::{History, VccCompactor};

async fn compact(history: &History) -> Result<(), animist::history::HistoryError> {
    history.compact(VccCompactor::builder()
        .keep_recent_messages(8_usize)
        .max_summary_chars(16_000_usize)
        .max_excerpt_chars(512_usize)
        .build()).await
}
```

These are also the defaults. Configuration supports Serde; omitted fields use
the same defaults as the builder.

The compactor prefers a user-turn boundary before the protected recent messages,
falling back to a completed tool-cycle boundary for single-prompt agentic chains.
It never splits a tool ID's calls and results, or removes an unresolved call or
orphan result. At least one message is retained even when `keep_recent_messages`
is zero. A short history or a history without a safe boundary is left unchanged.

The first active system message is a hard retention boundary. VCC may compact a
safe dialogue prefix before it, but retains that instruction and all following
messages verbatim, including with a custom extractor. It never extracts,
truncates, summarizes, or relabels system instructions. Consequently an early
system instruction may prevent VCC compaction entirely; use a deliberate custom
compactor if a different instruction-retention policy is required.

The removed prefix becomes an assistant text summary with prompt, response,
tool-input, and tool-result breadcrumbs in conversation order. Whitespace is
collapsed, excerpts are truncated, and images become omission markers. Thinking
and signatures are not copied into the summary; retained messages are untouched.
The first breadcrumb is retained while older subsequent breadcrumbs are evicted
to fit the summary budget. This is lossy extraction, not semantic summarization:
it does not infer decisions, understand tool-specific schemas, or guarantee a
token count or size reduction for short inputs. Callers decide when to compact
and whether heavier summarization is needed.

Generated summaries carry the reserved tag `animist.compaction = "vcc"`.
Successive passes merge their breadcrumbs without nesting summaries. Removed
sources, revisions, and prior summaries remain in the snapshot as inactive
entries. Character budgets count Unicode scalar values, not bytes or tokens;
the total budget includes the preamble and newlines. An excerpt budget of zero,
or a total budget too small for any breadcrumb, produces no edits.

#### Harness-specific extraction

Implement `VccExtractor` to interpret harness-specific tools or metadata, then
attach it with `VccCompactor::with_extractor`. The returned
`VccWithExtractor<E>` implements `Compactor`; it owns the runtime extractor
separately from the serializable settings.

```rust
use animist::history::{
    ContentBlock, StoredMessage, VccCompactor, VccExtraction, VccExtractor,
};
use async_trait::async_trait;

struct HarnessExtractor;

#[async_trait]
impl VccExtractor for HarnessExtractor {
    async fn extract(
        &self,
        message: &StoredMessage,
        block: &ContentBlock,
    ) -> Result<VccExtraction, animist::Error> {
        match block {
            ContentBlock::ToolUse { name, input, .. } if name == "edit_file" => {
                Ok(match input["path"].as_str() {
                    Some(path) => VccExtraction::Breadcrumbs(vec![format!("Modified {path}")]),
                    None => VccExtraction::Fallback,
                })
            }
            ContentBlock::Text { .. }
                if message.tags().contains_key("harness_noise") => Ok(VccExtraction::Omit),
            _ => Ok(VccExtraction::Fallback),
        }
    }
}

let compactor = VccCompactor::default().with_extractor(HarnessExtractor);
```

`VccExtraction::Fallback` uses generic extraction. `Breadcrumbs(lines)` replaces
the block's generic breadcrumbs in the supplied order; `Omit` suppresses the block.
Every returned string is whitespace-normalized and truncated as one breadcrumb,
then subject to the same total summary budget. An extractor cannot change cut
boundaries, edit plans, or retention policy.

Extraction runs sequentially in message/block order, only for non-thinking
blocks in the compacted prefix. It receives the latest `StoredMessage`, including
its timestamp and tags. Inactive entries, protected tail messages, and existing
generated summaries are not passed to it. Prior extracted breadcrumbs merge
normally on later passes. Tool-use blocks may contain thought signatures and
messages may contain sibling thinking blocks; extractors must not copy these
opaque payloads into their breadcrumbs.

Extractor errors propagate through `HistoryError::Transformation` with no
history edits committed. Async extraction holds no history lock; cancellation
and concurrent mutations have the same semantics as other compaction callbacks.
For multiple tool handlers, compose them within one extractor.

## Atomicity and concurrency

`HistorySnapshot::generation()` returns an opaque `HistoryVersion`. Save the
snapshot entries and cursor together, then call `history.changes_since(&cursor)`.
The returned `HistoryChanges` contains an ordered `changes()` slice and a
`generation()` cursor captured under the same read lock. Persist the whole
response before advancing the cursor; later mutations belong to the next read.
An unchanged history returns no events and the same cursor.

Replay `HistoryChange` events in order: `Appended` adds an entry at the end,
`Revised` appends a stored revision, `Deactivated` clears activation without
removing data, and `Inserted` inserts an entry. Each index refers to the entry
list at that individual event, not the final snapshot or the compaction plan.
An insertion shifts entries at and after its index right by one. Within a
compaction commit, revisions and deactivations appear in plan order first,
then insertions in ascending snapshot position, preserving plan order for ties.
Insertion indices include the shifts from preceding insertion events.
Insertions at the end emit `Appended`, including those committed by the agent.

`EntryReplaced` replaces the entire entry at its index without shifting entries.
Agent tool-result squashing emits this event after progress revisions, preserving
the original placeholder and only the final revision. Replace the persisted entry
rather than appending a revision, so incremental replay matches a full snapshot.

The feed retains at most 1024 events, evicting whole commits. A single commit
larger than that is not retained and clears older events. Retention bounds event
count, not bytes: stored message payload sizes remain caller-controlled.
`HistoryError::ChangesTooOld` means the cursor predates retained coverage;
take and persist a full snapshot, then resume from its cursor. Readers never see
part of a commit, and reads do not consume events for other readers.
Failed, cancelled, conflicting, and empty edits produce no events or new cursor.
Agent commits use the same feed as direct history mutations.

Cursors are runtime-only, scoped to one history and its cloned handles.
`from_entries` starts a fresh feed with no events for restored entries.
Passing a cursor from any other history, including a restored copy, returns
`HistoryError::ForeignVersion`. Cursors are neither serializable nor comparable
across histories for ordering; only equality is meaningful.
Adding these error variants breaks exhaustive downstream `HistoryError` matches.

Both operations snapshot under a short read lock, await callbacks without a
history lock, validate/build the result, and commit under a short write lock.
Callbacks can read the same history without deadlocking.

Any concurrent mutation, including an append, makes the snapshot stale and
returns `HistoryError::Conflict`. The caller decides whether to retry; expensive
LLM work is never retried implicitly. Even an empty plan is checked for conflict.

Callback errors, invalid plans, conflicts, and cancellation before commit apply
none of the operation's edits. Other callers' changes and external side effects
performed by callbacks are not rolled back. Callback errors retain their
original source under `HistoryError::Transformation`.
