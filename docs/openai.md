# OpenAI

`provider::openai::OpenAi` implements Animist's provider interface. Authentication,
endpoint selection, and model discovery live in the provider. The internal
wire types and helpers live under `protocol::openai`. Its public `ChatCompletions`
and `Responses` adapters implement `protocol::Protocol` for request translation
and stream assembly; they do not read credentials.

## Construction

Supply `OPENAI_API_KEY` in the process environment, not in source code:

```rust
use animist::provider::{
    Provider,
    openai::{OpenAi, OpenAiConfig, Protocol},
};

let provider = Provider::new(OpenAi::new(
    OpenAiConfig::builder()
        .protocol(Protocol::Responses)
        .build(),
)?);
```

Responses is the default. Set `Protocol::ChatCompletions` to use
`/chat/completions`. Protocol selection is explicit, not guessed from model names.
Construct separate providers if an application needs both protocols.

Configuration supports an explicit `api_key`, an alternate `api_key_env`,
`base_url` (including `/v1`), organization and project headers, and optional
catalog metadata. Use `OpenAi::with_client` to inject a runtime HTTP client;
the client is not part of serializable configuration. Alternate endpoints must implement the
selected OpenAI protocol; this is not a universal compatibility layer.

`OpenAiConfig` supports cloning, equality, defaults, and Serde serialization and
deserialization. Missing fields use the same defaults as its builder. Protocol
names serialize as `chat_completions` and `responses`. Explicit API keys may be
deserialized but are always omitted from serialized output; prefer environment
credentials for stored configuration.

`reload_auth` re-reads an environment-sourced key, retaining the old key on
failure. Explicit keys are fixed. `refresh_auth` is a no-op because API keys do
not have an automatic renewal flow. Neither configuration nor provider
implements `Debug`, to avoid logging credentials.

## Models and request policy

`list_models` calls `/models`. If a supplied `ProviderCatalog` has matching
metadata, it is attached to each returned ID. Otherwise capabilities and pricing
remain defaults: missing pricing is not a claim of free inference. The API also
lists non-chat models; applications must select a model suitable for inference.

Pass a `Model` with provider `"openai"`. Output limits use request options first,
then the model's configured limit. `fast` requests OpenAI's `priority` service
tier, unless explicitly disabled by a model override. Availability and billing
are still controlled by OpenAI.

Explicit reasoning efforts must appear in the model's advertised effort list.
Token budgets and the `Adaptive` and `Max` effort *values* are unsupported;
`ThinkingConfig::Adaptive` instead leaves effort to OpenAI and requests a
Responses reasoning summary. `ThinkingConfig::Off` omits reasoning options for
non-reasoning models and requests effort `"none"` for reasoning-capable models.
Models marked as requiring reasoning reject `Off` locally. Some reasoning
models do not support `"none"`; select `Adaptive` or a supported effort for
those models. The provider does not guess required reasoning from model names.

## Content and streaming

Both adapters support text, user images (URL or base64), function tools, tool
results, streamed text, partial tool arguments, usage, and stop reasons.
Responses also emits reasoning summary deltas and preserves the complete opaque
reasoning item in `ContentBlock::Thinking.signature` for stateless replay.
Requests use `store: false` and include encrypted reasoning content. Do not
modify signatures or send them through a different protocol.

Reasoning history is never silently omitted. Responses rejects unsigned thinking
without its original reasoning item. Chat Completions rejects unsigned thinking
unless the composing provider explicitly enables `reasoning_content` replay;
Synthetic and Z.AI Coding Plan enable it, native OpenAI does not. Disabling
thinking for the next generation does not remove prior reasoning from replay.
Switching to an incompatible protocol requires an explicit caller-side history
conversion, not an automatic loss of reasoning.

Chat Completions cannot replay Responses reasoning signatures. Unsupported
signed/redacted content, refusals, provider errors, malformed JSON, and
prematurely terminated streams return errors rather than silently losing data.
Completed tool arguments must be valid JSON, including when output is limited.
Built-in OpenAI tools, audio, video, and structured-output configuration are not
exposed by the current shared provider API.

Usage separates cached input from uncached input; output includes reasoning
tokens reported by OpenAI. Monetary cost remains unset. Incremental events
respect channel backpressure; consume a bounded channel concurrently with the
request. Dropping the streaming future cancels the exchange without a detached
request task.

## Validation

`cargo test --test openai` exercises both protocols against a local HTTP server,
without credentials or paid inference. Live OpenAI compatibility and
account-specific model/service-tier availability require a separate smoke test.
