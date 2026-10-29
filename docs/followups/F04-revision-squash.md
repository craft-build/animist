# F04 — Squash tool-result revisions

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

Before executing tools, `Agent` commits an interrupted-placeholder result
message. It then appends a **revision of the whole message** each time a call
finishes (`run_inner`, `CompactionEdit::Replace { index: result_index, .. }`
per completion). With N calls, the entry holds N+1 copies of a growing message.
For large outputs (file reads, test logs), in-memory history and every
persisted session grow quadratically per step.

Neither `Alteration` nor `Compactor` can remove revisions: `Replace` appends,
and `Remove` only deactivates. Craft (11) prunes at persistence, storing the
source plus the latest revision. Memory still grows.

## Goal

Agent-written tool-result entries end a step holding only what is needed for
recovery and audit: the placeholder source and the final result. Intermediate
progress revisions must not accumulate.

## Design guidance (choose one, and justify it in docs)

1. **The agent squashes on completion.** After the last result of a step
   commits, the agent replaces the entry's intermediate revisions with the
   final one (a crate-internal operation under the same generation check).
   Crash and cancel safety are unchanged, because the placeholder source
   stays.
2. **A general API.** For example `CompactionEdit::Squash { index }`, which
   keeps the source and the latest revision. It is validated like the other
   edits, and the agent uses it internally at step end, while applications can
   use it for their own revision policies.
3. Revise in place while the step is in flight (one working revision), and
   commit a single revision. Re-check this against the "dropping the run leaves
   explicit interrupted results" guarantee.

Whichever you choose:

- the snapshot and serialization formats stay readable;
- `docs/history.md` states exactly which revisions are retained;
- revisions that applications create (alterations, compaction `Replace`) are
  never squashed implicitly.

## Tests

- A step with 10 calls leaves its result entry with source + 1 revision.
- The source is still the placeholder, and the final revision holds all
  results in call order.
- Cancel mid-step keeps the placeholder semantics.
- Application revisions are untouched.
- (If option 2) the validation errors for squashing inactive or out-of-range
  entries.

## Craft cleanup after release

11: drop the persistence-time pruning, and remove any between-run history
rebuild added for memory.

## Completion note

Implemented option 1: an agent-only completion squash retains the placeholder
source and final stored revision before `StepFinished`, including when all calls
close after cancellation or failure. The internal operation checks entry ownership
and the history generation; application changes to the in-flight result conflict
rather than being pruned. Application alterations and compaction replacements on
other entries remain untouched. No public API or serialization changes.

`docs/history.md` specifies completion, drop, and cancellation retention. Tests
cover ten calls across serial waves and concurrent execution, call-order results,
placeholder and metadata preservation, serialization round trips, partial-drop
recovery, cooperative cancellation, and application revisions.

Validation passed: `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`,
`cargo test --features bedrock`, and `cargo doc --no-deps`. The live catalog
test remains ignored. Full-diff review returned a clean verdict with no
medium-or-higher findings.

Unreleased; Craft persistence cleanup remains a post-release task.
