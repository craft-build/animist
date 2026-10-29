# F02 — Parallel child dispatch

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

`ToolContext::dispatch` serializes every child of a parent through
`child_gate` (`src/agent/tool.rs`, the `child_gate` field and its lock in
`dispatch`). Top-level calls get bounded, conflict-aware waves
(`max_concurrent_tools` + `ToolScheduling`), but an orchestration tool like
craft's `batch` can't run independent children concurrently. Craft (10) ships
`batch` serial.

## Goal

An API for an exclusive parent to dispatch a set of child calls that runs them
under the same scheduling rules as top-level waves. Results come back in call
order; events and revisions may arrive in completion order.

## Design guidance

- For example: `ToolContext::dispatch_all(calls: Vec<ToolCall>) -> Vec<Result<ToolOutput>>`,
  or a `dispatch` that stays serial plus a new concurrent variant. Keep
  `dispatch`'s current contract unless you deliberately break it.
- Reuse the top-level wave builder:
  - contiguous conflict-free waves in call order;
  - conflicting calls are barriers;
  - claims come from `ToolExecutor::scheduling` and the
    `ToolSchedulingPolicy`;
  - the bound is `max_concurrent_tools`, or a separate child bound in
    `AgentConfig`. Decide which.
- Deadlock analysis: today's rule ("children inherit the parent's exclusive
  reservation; resource-claimed tools must not dispatch") exists to avoid
  parent/child permit deadlocks. Document why concurrent children of one
  exclusive parent stay deadlock-free. Nested orchestrators inside a concurrent
  child set must stay serial or be rejected.
- Budgets: every child counts against `max_child_calls`, `max_tool_calls` and
  `max_tool_depth` as today. Cancellation, `Stop` and fatal errors drop sibling
  futures, as in top-level waves.
- No spawned tasks: poll owned futures (`FuturesUnordered`), as the top-level
  scheduler does.
- Update `docs/agents.md` → *Bounded scheduling*.

## Tests

- Independent children overlap (barrier-instrumented tools, no timing).
- Conflicting children serialize in call order.
- Results come back in call order.
- A denied child affects only itself.
- `Stop` from one child drops its siblings and terminates.
- Limits are enforced.
- A nested orchestrator in a concurrent set is serial or rejected, as
  documented.

## Craft cleanup after release

10: switch `batch` to the concurrent API, un-ignore the `needs animist F02`
test, and revisit whether `batch` is still worth keeping.

## Completion note

Implemented `ToolContext::dispatch_all(calls: Vec<ToolCall>) ->
Result<Vec<ToolOutput>>` (branch `f02-parallel-child-dispatch`):

- The top-level wave builder was extracted to `Agent::wave_claims` and is
  shared by `run_inner` and the new `Agent::execute_child_waves`; children
  run in contiguous conflict-free waves bounded by `max_concurrent_tools`
  (no separate child bound was added, since an exclusive parent always runs
  alone in its top-level wave).
- Results arrive in call order; per-child failures (denial, unknown tool,
  decode/execution errors) are `is_error` outputs that affect only that
  child. Cancellation, `Stop`, and fatal child errors drop in-flight
  siblings and skip later waves, returning the typed error; child waves do
  not cancel `tool_halt` themselves, keeping the parent's outcome
  deterministic — `Stop` and cancellation escalate through the existing
  run-wide mechanisms.
- Nested `dispatch_all` is rejected with the new `NestedConcurrentDispatch`
  error (an exhaustive public enum variant, so the commit is `feat!:`);
  nested orchestrators stay serial through `dispatch`. Deadlock freedom is
  documented in `docs/agents.md` → *Bounded scheduling*: children inherit
  the exclusive reservation, claim no resources any wave waits on, and only
  the parent takes its child gate.
- `max_child_calls` now reserves the whole batch upfront; children still
  count against `max_tool_calls` and `max_tool_depth`.
- Tests cover overlap (barrier rendezvous, no timing), conflict
  serialization in call order, call-order results, denial isolation, `Stop`
  sibling drops, child/run budgets, and nested rejection. Craft cleanup
  (switch `batch`, un-ignore the gated test) remains until release.
