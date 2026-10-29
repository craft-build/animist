# F06 — Let authorization answer a call

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

`ToolDecision` is `Run | Deny { reason } | Stop { reason }`. A layer that
already knows the answer (craft's dedup cache returning an identical recent
read, or a policy that returns a canned result) can only `Deny`, which marks
the result `is_error` and reads as a refusal. Craft (05) serves dedup hits
inside an executor decorator instead. That hides them from the layer pipeline
and puts cache policy in the wrong place.

## Goal

A decision variant that completes the call with a supplied, model-visible
`ToolOutput` without running the executor. `post_tool_exec` still sees it.

## Design guidance

- For example `ToolDecision::Respond { output: ToolOutput }`. It
  short-circuits later layers exactly like `Deny`.
  - Decide whether it counts toward `RunStats::tool_calls` (probably not as
    "executed") and toward `max_tool_calls` (yes, it is admitted).
  - Decide which events fire: no `ToolStarted`, but `ToolFinished` with the
    output.
- This is a breaking change to an exhaustive enum: mark it `feat!` and update
  `docs/agents.md` → *Layers*.

## Tests

- A responded call never reaches the executor.
- Its output (including `is_error = false`) lands in the result slot in call
  order.
- `post_tool_exec` sees it.
- Later layers' `pre_tool_exec` are skipped.
- Stats and limits behave as documented.

## Craft cleanup after release

05 and 07: move dedup into `GuardrailLayer` (or a `DedupLayer`) as
`Respond`, and remove it from `HarnessExecutor`.

## Completion note

_(fill in when done)_
