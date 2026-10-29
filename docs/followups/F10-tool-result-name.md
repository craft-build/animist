# F10 — Tool name on tool results

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

`ContentBlock::ToolResult` carries `tool_use_id` but not the tool name. Every
consumer that groups, renders or compacts results by tool (craft's dedup,
guardrails, read lifecycle, TUI cards, VCC extractor, ACP) must find the
matching `ToolUse` first. After compaction removes the call, or for an orphan
in imported history, the name is gone.

## Goal

Decide whether results should carry the name. Then either add it, or document
the lookup contract and provide the helper in animist.

## Design guidance

- Option 1: an optional `name: Option<String>` field on `ToolResult`,
  serialized with `skip_serializing_if`, filled by the agent, and ignored by
  protocols that don't send it (Anthropic doesn't; OpenAI Responses and Chat
  don't need it).
  - Breaking for direct struct construction.
  - Older histories deserialize with `None`.
- Option 2: no field. Add `HistorySnapshot`/`[Message]` helpers that resolve
  result → call (`tool_call_for(tool_use_id)`), with the orphan case
  documented.
- Recommendation: option 1 if it costs nothing on the wire, because it makes
  history self-describing. Record the reasoning either way in
  `docs/history.md`.

## Tests

- The agent-written results carry the name.
- Legacy histories without it load.
- Protocol adapters ignore the field (snapshot the request bodies).

## Craft cleanup after release

04: remove `history::ext::tool_name_for` or reduce it to a fallback for legacy
entries.

## Completion note

Implemented option 1: optional, omitted-when-absent `ToolResult::name`, populated
from the call by the agent for placeholders and final results. Legacy histories
load without a name. Anthropic, OpenAI, and Bedrock Converse requests omit it.
The decision, legacy lookup fallback, and source-breaking construction/pattern
changes are documented in `docs/history.md`; release with `feat!:`.

Integration coverage checks named success/error/interrupted results, persistence,
legacy omission, and exact provider request bodies. Craft cleanup remains a
post-release task.

Review found no medium-or-higher issues; its formatting note was fixed.
Validation passed: `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`,
`cargo test --features bedrock`, and `cargo doc --no-deps`.
