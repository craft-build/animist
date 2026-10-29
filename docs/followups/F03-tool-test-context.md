# F03 — Test context for tools

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

`ToolContext`'s fields are `pub(crate)` and it has no public constructor, so a
`ToolImpl::call` can only be exercised through a full `Agent` run. Craft (06)
tests hundreds of tool cases through a one-shot agent driven by a scripted
provider. That works, but it is slow to write, indirect, and couples tool tests
to agent semantics.

## Goal

A supported way to invoke a `ToolImpl`/`ToolExecutor` directly with a context
whose run, cancellation and progress the caller controls. It may be
feature-gated (for example a `testing` feature) if it shouldn't be part of the
production API.

## Design guidance

- For example: `agent::testing::ToolHarness` that owns a minimal `Agent`/run
  context and exposes:
  - `call(tool, input) -> Result<ToolOutput, ToolError>`;
  - a `Cancellation` handle;
  - a progress receiver;
  - child dispatch into a supplied registry, with optional layers so policy
    can be tested too.
- Keep `ToolContext` construction private if possible. Have the harness build
  it internally, so the invariants (depth, visibility, scheduling) hold.
- Document it in `docs/agents.md` → *Typed tools*.

## Tests

- A typed tool runs with decoded args.
- Argument-decoding errors surface as `ToolError::Arguments`.
- Progress notes are received.
- Cancellation is observed.
- Child dispatch runs through supplied layers and enforces limits.

## Craft cleanup after release

06: re-implement `testing::tool_harness` (and `invoke(..)`) on it; drop the
one-shot scripted agent.

## Completion note

Implemented behind the `testing` feature as `agent::testing::ToolHarness`.
Typed and erased roots return structured `ToolError`s directly, expose shared run
state/cancellation/events, and construct private contexts internally. Child calls
reuse the agent registry, tool layers, scheduling policy, and execution limits.
Root calls intentionally bypass layers, timeouts, and counters; cancellation is
cooperative. The contract and usage are documented under *Typed tools*.

External integration tests cover typed decoding, argument errors, progress,
cooperative cancellation, child registry/layers, depth/call budgets, exclusive
reservations, invalid concurrency, and persistent child stops even when caught by
the root. Validation passed: `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`,
`cargo test --features bedrock`, `cargo test --features testing`, and
`cargo doc --no-deps` (also with `--features testing`). Independent review is clean
after fixing child-stop propagation and adding its regression test.
Not yet released; Craft cleanup remains a downstream followup after release.
