# Providers and protocols

## Status and scope

The catalog and public provider interface are implemented. OpenAI's provider
supports Chat Completions and Responses; Anthropic's provider supports Messages.
Synthetic and Z.AI Coding Plan reuse the OpenAI Chat Completions adapter.
See [OpenAI usage](openai.md), [Anthropic usage](anthropic.md),
[Synthetic usage](synthetic.md), and [Z.AI Coding Plan usage](zai-coding-plan.md).
Other adapters below remain future work.
The public `protocol::Protocol` async trait defines the shared adapter contract.

## Decision

Separate protocols from providers through composition. Keep Animist's public
provider interface and common message types as the application-facing boundary.

- A **protocol** defines wire representations, request/response translation, and
  stream assembly.
- A **provider** defines authentication, endpoint selection, available models,
  and provider-specific or model-specific policy.
- **Transport** carries the exchange using HTTP/SSE or an appropriate SDK.
  It does not need to be identical across protocols.

A provider may support several protocols, and several providers may reuse one
protocol adapter.

Initial protocol boundaries to consider:

- OpenAI Chat Completions
- OpenAI Responses
- Anthropic Messages
- Bedrock Converse
- Gemini, when implementing direct Google support

These are independent adapters, not variants forced into a single universal
request schema or lifecycle.

### Protocol contract

`protocol::Protocol` is `Send + Sync` and uses `async-trait`. It translates a
borrowed `ProtocolInput` into an associated request type, then asynchronously
assembles an associated transport response into common events and a result.
Associated options carry protocol-specific settings resolved by provider policy.
The contract does not require JSON, HTTP, or SSE for future implementations.

OpenAI's stateless `ChatCompletions` and `Responses` implementations live under
`protocol::openai`; Anthropic's `Messages` lives under `protocol::anthropic`,
each alongside its private Serde wire types.
The shared HTTP SSE reader lives in `protocol::sse` and reports `SseError`.
Their mutable stream state belongs to each assembly future; no shared handle or
lock is needed. Providers own routing and transport and invoke the selected
adapter through the trait.

Protocols return `ProtocolError`, which transparently wraps concrete errors such
as `openai::OpenAIProtocolError` and `anthropic::AnthropicProtocolError`.
The provider boundary returns `ProviderError`,
with a transparent `ProtocolError` variant. Structured failure variants preserve
source errors and protocol context without depending on provider error types.

## Responsibilities

### Protocol adapters

Own:

- Protocol-specific request, response, content, usage, and streaming event types.
- Translation between wire types and Animist messages, tools, options, and results.
- Stream state: content-block ordering, tool-call identifiers, partial tool-input
  JSON, reasoning data and signatures, usage, and stop reasons.

Share stream assembly across providers using the same protocol rather than
duplicating it in each provider. Keep wire types internal initially; expose them
only when there is a concrete consumer.

### Providers

Own:

- Credential loading, refresh, and reload.
- Endpoints, routing, and protocol selection.
- Model availability and provider-specific capability or request policy.
- Explicit compatibility differences from the selected protocol.

Provider metadata in the catalog is descriptive input, not automatic provider
construction or credential discovery.

### Compatibility and common types

"OpenAI-compatible" and "Anthropic-compatible" are baselines, not guarantees of
identical behavior. Prefer small, explicit compatibility settings or narrow
translation hooks justified by real providers. Avoid provider-name checks
scattered throughout parsers and speculative collections of flags.

Do not turn Animist's public message types into one vendor's wire schema.
Preserve opaque reasoning data and signatures needed to replay conversations.
When a protocol exposes a genuine gap in the common types, extend them
deliberately rather than silently discarding required data.

Keep request work owned by the streaming future so dropping it cancels the
request. Errors return through the existing result path; an additional error
event is not required merely because partial events were already emitted.

## Implementation sequence

1. Implement one real provider and its concrete protocol adapter together.
2. Place wire types, translation, and stream assembly in the protocol module.
3. Keep authentication, endpoints, and model policy in the provider.
4. Add a second provider using that protocol to validate the reuse boundary.
5. Extend the protocol contract or extract shared transport machinery only when
   concrete implementations demonstrate the need.

Test protocol translation and stream assembly with deterministic fixtures,
including interleaved tool calls, partial JSON, reasoning signatures, usage,
completion, and failures. Test provider authentication and compatibility policy
separately. Live-provider smoke tests supplement, rather than replace, fixtures.

The guiding rule is **protocol-first boundaries, not protocol-first framework
building**.
