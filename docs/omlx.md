# oMLX

`provider::omlx::Omlx` composes the shared OpenAI Chat Completions adapter with
oMLX authentication, model discovery, and thinking policy.

## Construction

```rust
use animist::provider::{
    Provider,
    omlx::{Omlx, OmlxConfig},
};

let provider = Provider::new(Omlx::new(OmlxConfig::default())?);
```

The default URL is `http://localhost:8000/v1`. Configure `base_url` to use another
server or reverse-proxy prefix, including `/v1`. `with_client` injects a runtime
HTTP client without adding it to serializable settings.

No credentials are read or sent by default. Keyless inference must be permitted
by the server. For authenticated inference, configure `api_key_env("OMLX_API_KEY")`
and supply the variable through the process environment. Explicit `api_key`
values take precedence. Configured credentials must be valid and nonempty;
failure never silently downgrades to keyless inference.

Configuration supports Serde with builder-consistent defaults. Keys are never
serialized, and configuration and provider do not implement `Debug`.
`reload_auth` re-reads environment credentials and retains the old credential on
failure. Explicit keys stay fixed; `refresh_auth` is a no-op.

## Model discovery

`list_models` calls `/v1/models`. Construct models with provider `"omlx"` and the
exact returned ID, including aliases or exposed profile suffixes such as
`my-model:thinking`.

Matching optional `catalog` metadata supplies capabilities, pricing, and output
limits. A positive `max_model_len` from the server overrides the catalog context
window because it describes the effective serving limit. Missing or null values
retain catalog metadata or remain unknown. Invalid or out-of-range context
windows return a typed decoding error.

Discovery does not infer thinking support, efforts, model tiers, or prices from
names. Configure model metadata or overrides to advertise thinking support before
requesting a budget or explicit effort. Missing prices are not evidence of free
inference.

## Thinking and request policy

OpenAI-compatible effort names are not reliable enable/disable switches in oMLX:
the server forwards them to templates and may remap `"none"` to `"low"`.

- `Off` sends `enable_thinking: false` and `thinking_budget: 0`, overriding a
  default server budget without relying on effort aliases. Models marked as
  requiring thinking reject `Off` locally.
- `Adaptive` leaves enablement, budget, and effort to server/profile defaults.
- An advertised effort sends `enable_thinking: true` and the effort unchanged,
  including `"max"`. Effort semantics remain template-specific.
- `Budget(n)` sends `enable_thinking: true` and native `thinking_budget: n`.
  Zero requests immediate termination of thinking. Budget enforcement depends on
  the engine, model, and output format.

The server's forced per-model settings can override request template controls.
Some models always reason; no request field can guarantee disabling their
internal reasoning. Grammar-constrained output can also limit budget support.

Output limits use `max_tokens`: request options first, then model metadata,
otherwise server defaults. Zero limits and fast mode are rejected; OpenAI
priority service tiers are not applicable.

## Streaming and history

Text, user images, function tools, tool results, usage, stop reasons, and thinking
deltas use the shared adapter. Complete unsigned thinking history is replayed as
`reasoning_content`, independently of the next request's thinking configuration.
Signed/redacted reasoning from incompatible protocols is rejected, not dropped.
The model's template ultimately controls how replayed reasoning is rendered.

Responses, Anthropic Messages, embeddings, reranking, server-side tools, model
management, and cache administration are not exposed by this provider.
Model loading, eviction, and caching remain managed by oMLX.

See [upstream oMLX](https://github.com/jundot/omlx) for server setup and policy.

## Validation

`cargo test --test omlx` exercises public construction, discovery, authentication,
request policy, streamed tool/reasoning replay, and error boundaries against
local HTTP/SSE fixtures. It does not run a live oMLX engine or download models.
Server-version and model/template compatibility require a separate smoke test.
