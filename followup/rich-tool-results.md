# Preserve rich tool output through execution, history, and protocols

**Status: implemented.** Shared text/image output now flows through typed tools,
events, history revisions/restoration, compaction, and supported protocol wire
formats. Legacy string content is accepted on load; new history writes arrays.
Chat Completions images and all adapters' deferred activation metadata fail
explicitly. See `tests/rich_tool_results.rs` and the rich-output agent test.
The original task below is retained as historical context.

Work in Animist only. Read AGENTS.md, docs/agents.md, docs/history.md, and
docs/providers-and-protocols.md. Craft's image and MCP tools are reference
consumers, not migration targets.

## Goal

Replace the text-only tool output boundary with a non-recursive ToolContent enum
supporting text and ImageSource. Preserve call IDs, is_error, and loaded_tools.
Reuse the same content representation in persisted tool results where practical.

## Requirements

- Do not accept arbitrary ContentBlock inside tool results: nested tool calls,
  thinking, and results must not become representable.
- Inspect every ContentBlock::ToolResult match, including history compaction,
  VCC, protocol translation, tests, and examples.
- Define and document history Serde compatibility/migration behavior.
- Support images in Anthropic, Chat Completions, and Responses where representable;
  explicitly reject unsupported output instead of discarding/stringifying it.
- Preserve concrete protocol errors through ProtocolError and ProviderError.
- Define whether JSON/structured tool output is a separate supported capability;
  don't force internal native output structs to Serialize just to render text.
- Resolve deferred-tool loaded_tools behavior deliberately; do not advertise
  unsupported activation capabilities.
- Keep UI-only presentation distinct from model-visible output.

## Validation

Add external-crate typed-tool tests for text plus image, protocol request fixtures,
history round trips/revisions, compaction preservation, and explicit unsupported
cases. Run cargo test and cargo check --all-features. No application migration.
