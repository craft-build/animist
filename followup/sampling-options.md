# Add typed sampling controls across agents, providers, and protocols

Implement this task in Animist only. Read AGENTS.md and
docs/providers-and-protocols.md first. Do not migrate Craft or Revelry.

## Goal

Extend provider::RequestOptions with typed optional temperature, top_p,
frequency_penalty, presence_penalty, and stop_sequences. Agent RunOptions and
StepRequest must use the same source of truth, not duplicate these fields.

## Requirements

- Inspect existing concrete provider compatibility rules and wire types before
  editing. Do not assume OpenAI-compatible endpoints accept identical controls.
- Preserve omitted-field behavior and existing thinking/fast/output-limit options.
- Validate ranges and conflicting combinations at the owning provider/protocol
  boundary with specific Snafu variants and retained upstream payloads.
- Translate supported options using Serde wire structs for Chat Completions,
  Responses, and Anthropic Messages, including providers reusing those adapters.
- Unsupported explicit controls must fail clearly rather than disappear silently.
- Keep provider-specific options out of the vendor-neutral agent state machine.
- If settings become serializable, use stable snake_case and matching builder,
  Default, and omitted-field defaults.

## Validation

Add table-driven protocol request fixtures, provider compatibility tests, and an
external-crate agent test asserting that request preparation forwards controls.
Run cargo fmt --all -- --check, cargo test, and cargo check --all-features.
No live credentials or external application edits are required.
