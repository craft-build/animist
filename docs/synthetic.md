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

## Embeddings

`Synthetic` also implements `EmbeddingProviderImpl`, using the same settings,
authentication, and HTTP client with the OpenAI-compatible `/embeddings` endpoint.
Embedding model IDs are explicit; chat discovery does not list embedding models.

```rust,no_run
use animist::provider::{
    EmbeddingModel, EmbeddingProvider, EmbeddingRequest, Provider,
    synthetic::{Synthetic, SyntheticConfig},
};
use std::sync::Arc;

# async fn example() -> Result<(), animist::provider::ProviderError> {
let synthetic = Arc::new(Synthetic::new(SyntheticConfig::default())?);
let chat = Provider::from_arc(synthetic.clone());
let embeddings = EmbeddingProvider::from_arc(synthetic);
let model = EmbeddingModel::builder()
    .id("hf:nomic-ai/nomic-embed-text-v1.5")
    .provider("synthetic")
    .build();
let request = EmbeddingRequest::builder()
    .input(vec!["search_query: async Rust".into()])
    .build();
let response = embeddings.embed(&model, &request).await?;
let vector = &response.embeddings[0];
# Ok(())
# }
```

Use `EmbeddingProvider::new(Synthetic::new(config)?)` when only embeddings are
needed. `from_arc` lets chat and embedding handles share one live credential
store and connection pool. Both handles support credential reload and refresh.

`EmbeddingRequest::dimensions` optionally requests a positive output dimension;
omitting it leaves the model default unchanged. Inputs are passed unchanged, so
callers supply any model-specific retrieval prefixes. Animist does not truncate,
normalize, or add prefixes.

`EmbeddingModel::expected_dimensions` optionally pins the required output width
using `NonZeroU32`. It is not sent to the server. Conflicting request dimensions
fail before HTTP; response widths must match the pin even with no resizing
request. Leave it unknown when the model's width is not known.

Results contain the server's model ID, vectors in input order, and optional usage.
Absent usage stays absent. Duplicate, missing, or out-of-range indices, empty or
inconsistent vectors, non-finite values, and requested-dimension mismatches return
structured protocol errors. Empty batches and zero dimensions fail before HTTP.
Provider-specific input and batch limits remain enforced by the server.

Embedding requests are non-streaming JSON exchanges. Dropping the request future
cancels the exchange. HTTP failures preserve the status and response body, just
as chat requests do. Embedding pricing and mixed-operation catalog discovery
are not yet represented.

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
- https://dev.synthetic.new/docs/openai/embeddings
- https://dev.synthetic.new/docs/api/models
