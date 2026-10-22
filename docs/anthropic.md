# Anthropic

`provider::anthropic::Anthropic` implements API-key authentication, routing,
model discovery, and request policy. `protocol::anthropic::Messages` implements
the public `Protocol` contract with typed Serde wire messages and stream events.
The protocol does not load credentials or choose endpoints.

## Construction

Supply `ANTHROPIC_API_KEY` in the process environment, not in source code:

```rust
use animist::provider::{
    Provider,
    anthropic::{Anthropic, AnthropicConfig},
};

let provider = Provider::new(Anthropic::new(
    AnthropicConfig::builder().build(),
)?);
```

The default endpoint is `https://api.anthropic.com/v1`; requests use
`x-api-key` and `anthropic-version: 2023-06-01`. Settings support an explicit
`api_key`, alternate `api_key_env`, `base_url`, `version`, `beta` headers,
fallback `max_output_tokens`, and optional catalog metadata. Inject a runtime
HTTP client with `Anthropic::with_client`.

Settings support cloning, equality, Serde, and consistent builder/Serde/Rust
defaults. Explicit keys can be deserialized but are never serialized.
Credential-bearing settings and providers do not implement `Debug`.
`reload_auth` re-reads an environment-sourced key and keeps the previous key
on failure. Explicit keys remain fixed. `refresh_auth` is a no-op for API keys.
OAuth and credentials from other applications are not discovered.

## Models and request policy

`list_models` follows `/models` pagination and rejects missing or repeated
cursors. It starts with matching catalog metadata when supplied, then applies
explicit API token limits and reported thinking, image, and effort capabilities.
Absent API metadata leaves catalog values or defaults intact. Model names do
not determine capabilities, tiers, required reasoning, or pricing. Missing rates
do not imply free inference; monetary usage cost remains unset.

Pass a `Model` whose provider is `"anthropic"`. Required `max_tokens` uses:

1. Request `max_output_tokens`.
2. Model `config.max_output_tokens`.
3. Provider fallback `max_output_tokens`, defaulting to 4096.

Thinking support and required-thinking policy honor runtime overrides:

- `Off` explicitly sends disabled thinking, unless the model requires it.
- `Adaptive` sends adaptive thinking for a thinking-capable model.
- `Budget(n)` sends enabled thinking with a budget of at least 1024 and less
  than `max_tokens`.
- `Effort(level)` sends adaptive thinking and `output_config.effort`. The level
  must be advertised by model metadata and be one of `low`, `medium`, `high`,
  `xhigh`, or `max`.

The shared model metadata does not represent every model's accepted thinking
configuration. Anthropic still validates manual/adaptive mode availability;
the provider does not infer it from model names.

`fast` sends `speed: "fast"` and adds the `fast-mode-2026-02-01` beta header
without duplicating a configured header. An explicit `overrides.fast: false`
rejects the request locally. Model/account availability and billing remain
controlled by Anthropic.

## Messages and streaming

Supported content includes text, user images (URL or base64 JPEG/PNG/GIF/WebP),
assistant thinking with signatures, redacted thinking, function tool calls,
and user tool results including `is_error`.

Thinking text, signature fragments, redacted data, content ordering, and tool
identifiers survive assembly and replay. Thinking replay requires a nonempty
signature. Signatures must come from the same protocol; they are not portable
across vendors. Tool input is assembled from partial JSON and must finish as an
object. Incomplete or malformed arguments return a typed error, including when
the output limit interrupts a tool call.

The shared SSE reader handles framing. The Messages adapter validates stream
lifecycle, content indexes, delta types, signatures, and completion. Unknown
event types are ignored for forward compatibility; unknown content/delta
shapes fail rather than silently losing content. In-stream API error payloads
remain JSON data through `AnthropicProtocolError`, `ProtocolError`, and
`ProviderError`.

Token usage distinguishes uncached input, cache creation, cache reads, and
output. Cumulative usage updates replace previous totals rather than being
added twice. Events honor channel backpressure; consume bounded channels
concurrently. Dropping the streaming future closes the HTTP exchange.

Stop reasons map as follows:

| Anthropic | Shared stop reason |
| --- | --- |
| `end_turn`, `stop_sequence` | `EndTurn` |
| `tool_use` | `ToolUse` |
| `max_tokens` | `MaxTokens` |
| `pause_turn` | `PauseTurn` |
| `refusal` | `Refusal` |
| `model_context_window_exceeded` | `ContextWindowExceeded` |

The last three shared variants are new; downstream exhaustive matches must
handle them. Paused turns and refusals are not reported as ordinary completion.

Server tools, deferred tool references, documents, structured outputs,
compaction, and explicit prompt-cache controls are not exposed by this adapter.
Unsupported replay content is rejected instead of being discarded.

## Validation

`cargo test --test anthropic` runs deterministic local HTTP fixtures without
credentials or paid inference. Coverage includes authentication headers, request
translation, signature/redaction replay, interleaved tools, cumulative usage,
pagination, stop reasons, error chains, and cancellation. Live API acceptance
and account-specific fast/thinking availability require a separate smoke test.
