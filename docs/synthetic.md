# Synthetic provider

`provider::synthetic::Synthetic` implements `ProviderImpl`. It uses the existing
`protocol::openai::ChatCompletions` adapter and shared SSE reader, a pooled async
HTTP client, and Synthetic's OpenAI-compatible endpoints.

```rust,no_run
use animist::provider::{Provider, synthetic::{Synthetic, SyntheticConfig}};

let provider = Provider::new(Synthetic::new(SyntheticConfig::default())?);
```

Supply `SYNTHETIC_API_KEY` in the process environment. Alternatively use
`Synthetic::new(SyntheticConfig::builder().api_key(key).build())` with a key supplied
at runtime. Never put credentials in source code. Credentials are loaded at construction;
`reload_auth` reads the environment again. Explicit keys remain fixed.
`refresh_auth` is a no-op; API keys have no refresh exchange. Settings support Serde
with snake-case names and consistent defaults; credentials are omitted on serialization.
Use `with_client` to replace the runtime HTTP client. `base_url` defaults to
`https://api.synthetic.new/openai/v1`, including the protocol path.

## Requests and discovery

- Set `Model::provider` to `synthetic`.
- Model IDs are sent unchanged, including `hf:`, `syn:`, and `infinite:` IDs.
  Prefer Synthetic's recommended aliases to pinning rotating model versions.
- All inference requests stream. There is no overall request timeout imposed by
  this provider, allowing long pauses in Infinite mode. A custom `reqwest::Client`
  may supply application-specific timeouts.
- Dropping the streaming future cancels the request. Event sends apply channel
  backpressure; consume bounded event channels concurrently.
- Text, images, tool calls/results, and `reasoning_content` are translated
  independently of local display and harness metadata. Assistant reasoning is
  preserved for replay during tool loops.
- Requests include `stream_options.include_usage`. Cached prompt tokens are
  separated from uncached input tokens, avoiding double-counting.
- Output limits use the request override, then the model's configured limit, then
  8,192 tokens.
- `list_models` returns current text-generating models. `model_catalog` additionally
  retains complete source metadata, including fields Animist cannot represent.
  Prices are converted from USD per token to USD per million tokens. Missing
  rates retain Animist's zero defaults, not evidence of free inference.
- Tier and required-thinking policy are not inferred from names or effort lists.

## Current compatibility limits

`ThinkingConfig::Effort` maps to `reasoning_effort` when the model advertises that
effort. `Adaptive` leaves the server's default effort unchanged. `Off` sends
`reasoning_effort: "none"` for reasoning-capable models and omits it for other
models. Explicit required-thinking policy rejects `Off`; required thinking is
never inferred from model names or effort lists. Runtime thinking overrides are
honored without changing catalog metadata.

Token-based thinking budgets, unsupported efforts, and fast mode return
configuration errors rather than being approximated.

Opaque thinking signatures, redacted thinking, and tool thought signatures from
other protocols cannot be replayed through Chat Completions and return
configuration errors rather than being silently discarded. Tool-result
`loaded_tools`, message display text, and observation kinds are local metadata.
Tool results become `role: "tool"` messages; Chat Completions has no separate
`is_error` field, so error details must be carried in the result's content.

HTTP failures retain status codes and response bodies in `ProviderError::Http`.
Treat upstream error bodies as potentially sensitive. Stream errors,
invalid tool JSON, unsupported finish reasons, and premature EOF return errors
through `OpenAIProtocolError` and `ProtocolError` into `ProviderError`.
Events emitted before an error remain available.

The SSE reader accepts a final `[DONE]` event without a trailing blank line.
Completion still requires both `[DONE]` and a finish reason. A
`TruncatedChatStream` error includes the finish reason seen before EOF, if any;
complete-looking text alone does not establish that a response finished.

## References

- [Runnable manual tests](manual-provider-tests.md)
- https://dev.synthetic.new/docs/api/overview
- https://dev.synthetic.new/docs/openai/chat-completions
- https://dev.synthetic.new/docs/openai/models
- https://dev.synthetic.new/docs/api/models
