# llama.cpp

`provider::llama_cpp::LlamaCpp` uses the shared OpenAI Chat Completions adapter
for both single-model and router-mode `llama-server` deployments.

## Construction

```rust
use animist::provider::{
    Provider,
    llama_cpp::{LlamaCpp, LlamaCppConfig},
};

let provider = Provider::new(LlamaCpp::new(
    LlamaCppConfig::builder()
        .base_url("http://localhost:8080/v1")
        .build(),
)?);
```

The default URL is `http://localhost:9931/v1`, matching current upstream's port;
set `base_url` to match your server version and launch arguments. Include `/v1`
and any reverse-proxy prefix. `with_client` injects a runtime HTTP client.

Authentication is optional: no credentials are read or sent by default. For a
server started with `--api-key`, explicitly configure `api_key_env("LLAMA_API_KEY")`
and supply that variable in the process environment. A configured variable must
exist and be nonempty; failures never silently downgrade to unauthenticated
requests. Explicit `api_key` values take precedence.

Configuration supports Serde with builder-consistent defaults. API keys are
omitted from serialization, and configuration and provider do not implement
`Debug`. `reload_auth` re-reads environment credentials, retaining the old
credential on failure. Explicit keys stay fixed; `refresh_auth` is a no-op.

## Routing and models

Construct `Model` values with provider `"llama-cpp"` and the exact model ID or
preset name returned by `list_models`. Discovery calls `/v1/models` and attaches
matching optional `catalog` metadata. Unknown capabilities and context windows
are not inferred from model names, and missing prices do not imply free usage.
Router loading status and native metadata are not exposed by this provider.

The request's ordinary `model` field selects the router destination; no separate
router provider or protocol is necessary. With upstream's default autoload
enabled, inference also loads an unloaded model. With `--no-models-autoload`,
load the model outside Animist before inference. The provider does not retry
load failures or expose lifecycle APIs, per-request autoload, or status events.

## Embeddings

`LlamaCpp` also implements `EmbeddingProviderImpl` through the shared
OpenAI-compatible `/v1/embeddings` adapter. Construct an `EmbeddingModel` with
provider `"llama-cpp"` and the exact embedding model ID or router preset name.
The ordinary `model` field routes the request; no separate embedding server
configuration is needed in Animist.

The selected server/worker must run with `--embeddings` and a pooling type other
than `none`. Configure these options for the embedding model's router preset,
not for chat workers. Model loading and autoload policy remain server-owned.

Use `EmbeddingProvider::new(LlamaCpp::new(config)?)`, or build chat and embedding
handles from the same `Arc<LlamaCpp>` to share authentication and the HTTP client.
Batched text is sent unchanged; vector results are validated and reordered by
input index.

Current upstream does not implement the OpenAI `dimensions` request field, so
Animist rejects explicit dimension requests rather than sending an ignored
option. Set `EmbeddingModel::expected_dimensions` to a positive known native
width to validate output without requesting resizing. The server owns pooling
and normalization; Animist does not modify returned vectors.

Embedding model IDs and dimensions remain explicit, not inferred from names or
chat catalog metadata. See the [Synthetic embedding example](synthetic.md#embeddings)
for the shared request/response API.

## Decision models

`LlamaCpp` implements `DeciderProviderImpl` using `/v1/systemone`. Direct servers
and routers use the same adapter and ordinary `model` field. Chat, embedding,
and decision handles can share the same `Arc<LlamaCpp>`.

Choose a native decision model and construct `DeciderModel` with provider
`"llama-cpp"`. The server supports named choice, score, and yes/no probability
questions; these are not chat completions. Native decision support is advertised
through `architecture.output_modalities: ["decisions"]`, potentially alongside
`"text"`. Animist's decision model IDs remain explicit.

See [decision models](deciders.md) for examples, answer validation, optional
inline media, model-dependent limits, and cancellation behavior.

## Request policy

- Output limits use `max_tokens`: request options first, then model metadata,
  otherwise server defaults. Zero limits are rejected.
- `Off` sends `reasoning_effort: "none"` even without capability metadata.
  Models marked as requiring thinking reject it locally.
- `Adaptive` leaves server/template defaults unchanged. Explicit efforts must
  appear in model metadata and are forwarded unchanged, including `"max"`.
- `Budget(n)` sends native `reasoning_budget_tokens` for a model marked as
  supporting thinking. Zero requests immediate end of thinking; values above
  `i32::MAX` are rejected. No Anthropic-specific minimum budget is imposed.
- OpenAI priority service tiers are unsupported; fast mode is rejected.

Thinking controls depend on the model's chat template and server configuration.
Configure the server to enable thinking where needed. Use an extracting
reasoning format such as `--reasoning-format deepseek` for structured thinking;
`none` leaves thoughts inside ordinary text. Native budget enforcement requires
a template with recognized thinking-end tags.

## Streaming and history

Text, user images, function tools, tool results, usage, stop reasons, and
`reasoning_content` deltas use the shared Chat Completions adapter. Tool support
requires a compatible model/template and Jinja enabled on the server.

Unsigned thinking history is replayed as `reasoning_content`, even when thinking
for the next request is off. Signed/redacted reasoning from incompatible
protocols is rejected rather than discarded. Server-side template policy still
determines which historical reasoning is included in the rendered prompt.

Responses, native sampling controls, and model management are out of scope.
See [upstream server documentation](https://github.com/ggml-org/llama.cpp/tree/master/tools/server)
for server configuration and supported extensions.

## Validation

`cargo test --test llama_cpp` tests the public API against local HTTP/SSE
fixtures without downloading models or starting `llama-server`. Live
server-version, template, autoload, and model-specific behavior require a
separate smoke test.
`cargo test --test embedding` additionally tests embedding routing, authentication,
dimension validation, and HTTP errors for all three local providers.
