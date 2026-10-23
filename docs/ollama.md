# Ollama

`provider::ollama::Ollama` connects to a local Ollama server using the shared
OpenAI Chat Completions adapter. No new protocol implementation is needed.

## Why a specialized provider?

Ollama's [OpenAI compatibility API](https://docs.ollama.com/api/openai-compatibility)
is a subset, not a drop-in replacement for Animist's native OpenAI policy:

- Output limits use `max_tokens`, not `max_completion_tokens`.
- Thinking deltas and assistant history use `reasoning`, not `reasoning_content`.
- Assistant history uses string content so Ollama preserves reasoning even on
  turns without tool calls.
- Local requests require no API key. OpenAI's priority service tier is unsupported.
- Thinking can default to enabled on the server, so `ThinkingConfig::Off`
  explicitly sends `reasoning_effort: "none"` even without catalog metadata.

These differences justify a thin provider; SSE framing, tool calls, usage, and
response assembly remain shared with the other Chat Completions providers.

## Construction

Start Ollama and pull a model before inference. No `OPENAI_API_KEY` is needed:

```rust
use animist::provider::{
    Provider,
    ollama::{Ollama, OllamaConfig},
};

let provider = Provider::new(Ollama::new(OllamaConfig::default()));
```

The default base URL is `http://localhost:11434/v1`. Set `base_url` for another
local or self-hosted server, including `/v1`. Configuration supports Serde with
the same defaults as its builder. `with_client` injects a runtime HTTP client.
Authentication hooks are no-ops. Direct authenticated Ollama cloud access is not
part of this local-server provider.

## Models and request policy

`list_models` calls `/v1/models` and returns the server's model IDs. Construct
models with provider `"ollama"` and the exact returned ID, including its tag.
Optional `catalog` metadata is attached only to matching IDs; unknown
capabilities, context windows, and pricing are not inferred from model names.
Native `/api/show` capability discovery is not implemented.

Output limits use request options first, then model metadata. Without either,
Ollama chooses the limit. Zero limits are rejected. Fast mode and token-based
thinking budgets are unsupported.

`Adaptive` leaves thinking to the server. Explicit effort values must be
advertised in model metadata and are sent unchanged, including `Max` as `"max"`.
Configure metadata to match the model's actual supported efforts: Ollama's
interpretation is model-specific. Models marked as requiring thinking reject
`Off` locally.

## Embeddings

`Ollama` also implements `EmbeddingProviderImpl` using the OpenAI-compatible
`/v1/embeddings` endpoint, not the legacy native `/api/embeddings` endpoint.
Pull an embedding model first, then construct an `EmbeddingModel` with provider
`"ollama"` and its exact ID/tag.

Use `EmbeddingProvider::new(Ollama::new(config))`, or construct chat and embedding
handles from the same `Arc<Ollama>` to share the HTTP client. No API key is needed.
Batched text and optional requested dimensions use the shared adapter; results
are validated and returned in input order. Dimension reduction depends on the
server and model, and mismatched output widths return errors.

Set `EmbeddingModel::expected_dimensions` to pin the required output width.
This validates responses even when request dimensions are omitted and does not
send a resizing instruction. Conflicting explicit requests fail before HTTP.
The server owns input limits, truncation, and normalization behavior.

Embedding IDs and dimensions are explicit; native capability discovery and
mixed-operation catalogs remain unimplemented.
See the [Synthetic embedding example](synthetic.md#embeddings) for shared types.

## Content and limitations

Text, function tools, tool results, thinking deltas, usage, and stop reasons use
the shared streaming interface. Preserve complete assistant messages when
replaying history; unsigned thinking is sent back as `reasoning` independently
of the next request's thinking setting. Signed or redacted reasoning from other
protocols is rejected rather than discarded.

For vision models, use base64 image sources; Ollama's Chat Completions image
decoder does not fetch remote image URLs. Native controls such as `num_ctx` and
`keep_alive`, model management, and Responses are not exposed by this provider.
Configure context size on the server or through a Modelfile.

## Validation

`cargo test --test ollama` tests public construction, unauthenticated routing,
model discovery, token limits, thinking/tool replay, and errors against a local
HTTP fixture. It does not require a running Ollama server or download models.
Live server-version and model-specific compatibility need a separate smoke test.
`cargo test --test embedding` covers embedding requests and error boundaries
against local mock HTTP servers.
