# F07 — Panic-safe tool execution

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

The agent polls tool futures in place (`FuturesUnordered`, no spawning, by
design). A panic inside any tool, executor or layer future therefore unwinds
through `Agent::run`, aborting the whole run and leaving the caller with no
`RunReport`. Harness tools wrap third-party code (MCP clients, parsers,
patchers) where panics happen. Craft (05) wraps each executor in
`catch_unwind`.

## Goal

A panicking tool execution becomes a structured tool failure: an error result
the model can react to, and a `ToolFailure` in the report. The run continues.
The behaviour is documented and tested.

## Design guidance

- Wrap the executor future (`execute`) with `futures_util::FutureExt::catch_unwind`
  plus `AssertUnwindSafe`. No `spawn`, no `spawn_blocking` (AGENTS.md).
- Add a `ToolError::Panicked { message }` variant (a breaking change), with the
  payload rendered when it is `&str`/`String`.
- Decide and document whether layer panics are also caught. Recommendation:
  catch executor panics only. Layer panics are programming errors and keep
  unwinding.
- Update `docs/agents.md` → *Typed tools*.

## Tests

- A panicking tool yields an `is_error` result with the panic text, a
  `ToolFailure` with `Panicked`, sibling calls in the same wave complete, and
  the run proceeds to the next step.
- Layer panic behaviour is as documented.

## Craft cleanup after release

05: drop `catch_unwind` from `HarnessExecutor`.

## Completion note

_(fill in when done)_
