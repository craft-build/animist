# F05 — History change feed

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

Applications that persist history incrementally (craft's append-only JSONL
sessions) can't tell what changed. `HistorySnapshot` exposes entries but not
its generation, and there is no record of edits. Craft (11) diffs snapshot
shapes (entry count, revision count, active flag) after each run and at
checkpoints. That is correct but O(history) each time, and it can't
distinguish a source change from an external rewrite without deep comparison.

## Goal

A cheap, ordered description of the changes since a known point, so a
persister can write exactly the deltas.

## Design guidance

- Expose `HistorySnapshot::generation()` (or an opaque version token).
- Add a change log, for example `History::changes_since(token) ->
  Result<Changes, HistoryError>`, where `Changes` is an ordered list of
  `Appended { index, entry }`, `Revised { index, message }` and
  `Deactivated { index }`. Indices refer to the post-change entry list;
  document how insertions shift them.
  - Return a specific "too old, take a full snapshot" error when the log no
    longer covers the token. Bound retention explicitly; don't keep an
    unbounded log.
- Alternatively, an async subscription (a `tokio::sync::watch` generation plus
  pull), if that fits the lock design better. Don't add callbacks that run
  under the history lock.
- `from_entries` starts a fresh log.
- Document it in `docs/history.md` → *Atomicity and concurrency*.

## Tests

- Append, alter, compact (insert/replace/remove) and agent commits each produce
  the expected change sequence.
- Insertions report correct indices.
- A stale token returns the specific error.
- Concurrent readers see consistent sequences.

## Craft cleanup after release

11: incremental sync consumes `changes_since`; delete the shape diff, and keep
the full-rewrite fallback for the stale-token error.

## Completion note

Implemented with opaque, history-scoped `HistoryVersion` cursors,
`HistorySnapshot::generation()`, and atomic `History::changes_since()` reads.
The feed retains at most 1024 events, evicts whole commits, and reports
`ChangesTooOld` for snapshot fallback or `ForeignVersion` for another history.
Append, revision, deactivation, insertion ordering, restore, retention,
concurrent readers, and agent commits have public integration coverage.
See `docs/history.md` for replay indices and persistence semantics.

Breaking API addition: exhaustive `HistoryError` matches must handle the two
new variants; release under `feat!:`. Unreleased; Craft cleanup remains pending.

Validation passed: `cargo fmt --check`, strict all-target/all-feature Clippy,
`cargo test`, `cargo test --features bedrock`, `cargo doc --no-deps`, and
`git diff --check`. Review verdict: clean against baseline `e639eb75`.
Landing integrates F04 revision squashing with an `EntryReplaced` event, preserving
exact incremental replay of originals and final revisions. The ten-call agent
integration test replays the feed for serial and concurrent tool completion.
