# Z.AI Coding Plan

`provider::zai_coding_plan::ZaiCodingPlan` uses the shared
`protocol::openai::ChatCompletions` adapter and SSE reader. It targets
`https://api.z.ai/api/coding/paas/v4`, not the pay-as-you-go API.

```rust,no_run
use animist::provider::{
    Provider,
    zai_coding_plan::{ZaiCodingPlan, ZaiCodingPlanConfig},
};

let provider = Provider::new(ZaiCodingPlan::new(ZaiCodingPlanConfig::default())?);
```

## Credentials and settings

- Supply a Coding Plan API key using `ZHIPU_API_KEY`, matching models.dev's
  `zai-coding-plan` metadata. Override `api_key_env` to use another variable.
- Alternatively, supply `api_key` at runtime through the config builder. Do not
  commit a key to source code. Credentials are omitted from serialized settings,
  and credential-bearing types do not implement `Debug`.
- Credentials load at construction. `reload_auth` rereads environment-backed
  credentials; explicit keys stay fixed. API keys have no refresh exchange, so
  `refresh_auth` is a no-op.
- `with_client` replaces the pooled runtime HTTP client independently of config.
  Dropping a streaming future cancels the HTTP exchange.
- Setting names use snake case; omitted fields share the builder's defaults.

## Models and request policy

Set `Model::provider` to `zai-coding-plan`. Model IDs are sent unchanged; the
provider does not rewrite old IDs or infer capabilities from names.

`list_models` returns models from the optional configured `ProviderCatalog`,
using its map keys as the request IDs. For example, pass the `zai-coding-plan`
entry from a loaded models.dev `Catalog`. No undocumented model-discovery API is
queried. Without a catalog, listing is empty; callers can still supply a `Model`
directly. Catalog data is not proof of subscription entitlement.

Thinking support, efforts, and required-thinking policy come from `Model::info`
and runtime overrides. Z.AI currently documents GLM-5.3 and GLM-5.3-Flash as
requiring thinking. When using those models, set the required-thinking policy
explicitly and request `Adaptive` or a supported effort. The catalog parser does
not infer required thinking.

- `Off` sends `thinking.type: "disabled"`; it fails locally if thinking is marked
  required.
- `Adaptive` enables thinking without setting an effort. Explicit efforts map to
  `reasoning_effort` only when present in the model's advertised efforts.
- Preserved reasoning is enabled with `thinking.clear_thinking: false`.
  Assistant `reasoning_content` is returned unchanged during tool-loop replay.
- Requests with tools set `tool_stream: true`. Incremental reasoning, text, and
  tool arguments use the common event types.
- Output limits use `max_tokens`, with precedence: request, model configuration,
  then provider configuration (8,192 by default).
- Token-based thinking budgets and fast mode fail locally rather than being
  silently approximated.

HTTP errors preserve status and response bodies. Protocol errors retain the
existing `OpenAIProtocolError` → `ProtocolError` → `ProviderError` chain.
Error bodies may contain sensitive upstream data.

## Subscription scope

Z.AI documents a restricted set of supported tools and environments for Coding
Plan subscriptions. Check eligibility for your application before enabling this
provider. This implementation does not spoof a supported tool's identity or fall
back to a different billing endpoint.

## References

- [Runnable manual tests](manual-provider-tests.md)
- https://docs.z.ai/devpack/tool/others
- https://docs.z.ai/devpack/notice/api-key-management
- https://docs.z.ai/api-reference/llm/chat-completion
- https://docs.z.ai/guides/capabilities/thinking-mode
- https://docs.z.ai/guides/capabilities/stream-tool
