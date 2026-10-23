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
