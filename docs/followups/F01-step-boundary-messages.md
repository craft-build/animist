# F01 — Durable messages at step boundaries

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

A layer cannot add a durable message during a run. `prepare_request` is
request-only, and any external `History` edit makes the next `ExchangeCommit`
fail with `HistoryChanged`. So harnesses can't do any of these inside a run:

- inject a steering or grace prompt;
- nudge after a silent reply;
- deliver a user message queued while the agent works;
- continue after `MaxTokens`.

Craft (07, 08) works around it by stopping the run (`ToolDecision::Stop`) or
letting it finish, then starting a follow-up `Agent::run` with an observation
input. That closes unfinished sibling calls as not executed, splits one logical
turn into several runs (their stats and events need re-aggregating), and loses
the step budget's meaning.

## Goal

A layer hook, run between model steps, that can append durable `User`
messages (typically `MessageKind::Observation`) to history before the next
inference attempt. The hook can also ask the run to continue when it would
otherwise complete.

## Design guidance (shape is yours to decide)

- A new `LayerImpl` hook, for example
  `async fn step_end(&self, agent, run, step: &StepOutcome) -> Result<StepAction>`,
  run head-to-tail after a step's tool results are fully committed, and
  before termination is decided.
  - `StepAction` can carry messages to append, and a request to continue even
    when the step had no tool calls (nudge, `MaxTokens` continuation).
  - Messages from all layers are appended in layer order.
- Appends go through the same optimistic commit path (`ExchangeCommit`
  semantics), so external-edit conflicts are still detected. Validate each
  appended message: `Role::User`, no tool blocks, and `validate_replay` on the
  result.
- A continuation consumes a step from `max_steps`. Terminal statuses
  (`MaxTokens`, `Refused`, …) stay distinct: a layer can opt into continuing
  from `MaxTokens` and `Paused` only, never from `Refused` or
  `ContextWindowExceeded`. Decide this and document it.
- Optional but valuable: the same hook is also a safe point for layer-requested
  history edits (compaction, alteration), because nothing is in flight. If you
  include it, define the conflict semantics explicitly.
- Events: emit a new `AgentEvent` for appended messages, so surfaces can render
  them.
- Update `docs/agents.md` (*Layers* table, *Execution, history, and
  cancellation*).

## Tests

- A grace message appended after step N is in history and in step N+1's
  request.
- A continuation after an empty reply runs another step and respects
  `max_steps`.
- A `MaxTokens` continuation, when opted in.
- An external history edit during the step still gives `HistoryChanged`.
- An invalid appended message (an assistant role, or tool blocks) is rejected
  with a specific error.
- Hook order is head-to-tail, and cancellation during the hook behaves as for
  other hooks.

## Craft cleanup after release

- 07: grace via `step_end` instead of `Stop` + re-run.
- 08: nudge and `MaxTokens` continuation via `step_end`; the driver's
  multi-run aggregation shrinks.
- Consider queued user steering messages in the TUI.

## Completion note

Implemented on branch `f01-step-boundary-messages` (2026-10-27).

- `Layer::step_end(agent, run, outcome: &StepOutcome) -> Result<StepAction>`
  runs head-to-tail after a step's results commit and before termination is
  decided. `StepOutcome` carries the step index, assembled assistant message,
  stop reason, and the terminal status the run takes if nothing continues
  (`None` when tool calls continue the run). `StepAction` carries messages to
  append and a `continue_run` request; messages concatenate in layer order and
  `continue_run` ORs across layers.
- Appends validate `Role::User`, no tool blocks, and `validate_replay` on the
  exact view about to commit, then commit once, atomically, through
  `ExchangeCommit` against a snapshot taken after the last tool-result commit.
  Any history edit that snapshot does not reflect — including one inside a
  `step_end` hook — fails with `HistoryChanged`, committing nothing and
  retaining the messages in the error.
- Continuation is honored from `Completed`, `MaxTokens`, and `Paused` only;
  requesting it from `Refused` or `ContextWindowExceeded` fails with the new
  `UnsupportedContinuation` before any append commits. Continuations consume
  `max_steps`; with the budget exhausted the run ends `StepLimit` while
  appended messages stay durable. Appended messages are emitted as
  `AgentEvent::MessagesAppended { run_id, step, messages }`.
- The optional layer-requested history-edit safe point was not added: appends
  already cover the durable-message case through the same optimistic path, and
  arbitrary compaction/alteration at boundaries can ride the existing
  `History` APIs with the same conflict semantics later.
- Tests: nine in `tests/agent.rs` covering grace delivery into history, events,
  and the next request; empty-reply continuation under two budgets;
  `MaxTokens`/`Paused` opt-in; refused/CWE rejection; `HistoryChanged` on an
  in-hook edit with retained payload; invalid role/tool-block/replay appends;
  head-to-tail order; cancellation. Validation: `fmt --check`, clippy
  `-D warnings`, `cargo test`, `cargo test --features bedrock`, `cargo doc`.
  Review agent verdict: clean (two low findings also addressed).
