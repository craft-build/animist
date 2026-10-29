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

_(fill in when done)_
