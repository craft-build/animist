# History

`History` is a shared handle: cloning it shares live state. `History::new()` and
`Default` create an empty history. `snapshot()` returns an independent owned view,
including inactive entries, originals, and revisions with their metadata.

## Entries and views

Each entry retains its original `StoredMessage`, a sequence of stored revisions,
and an independent activation flag. Removing an entry deactivates it without
discarding its original or revisions.

| View | Content | Entries |
| --- | --- | --- |
| `current()` | Latest revision, or original when unrevised | Active |
| `unaltered()` | Original | Active |
| `verbatim()` | Original | All |

Revisions contain `StoredMessage`, preserving each version's content, timestamp,
and tags. Replacing a message never changes its original or earlier revisions.
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

## Atomicity and concurrency

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
