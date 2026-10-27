# Validate prepared conversation replay without losing opaque data

Work in Animist only. Read docs/history.md, docs/agents.md, and
docs/providers-and-protocols.md.

## Goal

Define shared structural replay validation used by agent request preparation and
protocol translation, while retaining protocol-specific restrictions in adapters.
Current agent validation checks submitted roles, assistant tool-result misuse,
missing calls, and duplicate/empty tool IDs; it is not a complete replay validator.

## Requirements

- Inspect existing protocol validation and reuse it rather than imposing a
  vendor-specific universal message sequence.
- Cover call/result identity, duplicate/orphan results, interrupted tool cycles,
  invalid role/content combinations, and loaded/deferred tool declarations.
- Preserve signed/redacted reasoning and tool thought signatures.
- Validate restored history and layer-produced request views before inference.
- Compaction remains an explicit caller operation; do not silently drop messages.
- Support request-only context insertion without confusing it with durable history.
- Keep failures structured and locate the offending message/block/call.

## Validation

Add restored-history, compaction, layer-request, and protocol tests including
multimodal and interleaved tool exchanges. Run cargo test and
cargo check --all-features. Do not migrate external applications.
