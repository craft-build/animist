# Providers and protocols

## Status and scope

The catalog and public provider interface are implemented. OpenAI's provider
supports Chat Completions and Responses; Anthropic's provider supports Messages.
Synthetic, Z.AI Coding Plan, Ollama, llama.cpp, and oMLX reuse the OpenAI Chat
Completions adapter.
The optional Amazon Bedrock provider composes the native Anthropic/OpenAI
adapters with AWS credential resolution and SigV4 signing.
See [OpenAI usage](openai.md), [Anthropic usage](anthropic.md),
[Synthetic usage](synthetic.md), and [Z.AI Coding Plan usage](zai-coding-plan.md).
See [Amazon Bedrock usage](bedrock.md) for AWS routing and authentication.
See [Ollama usage](ollama.md) for local inference and compatibility policy.
See [llama.cpp usage](llama-cpp.md) for single-model and router deployments.
See [oMLX usage](omlx.md) for local inference and explicit thinking control.
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

### Embeddings

`provider::EmbeddingProvider` is a separate clonable handle for batched text
embeddings. Its models, requests, responses, and usage do not depend on chat
messages or thinking settings. Synthetic, llama.cpp, Ollama, and oMLX implement
both interfaces; applications can construct both handles from the same concrete
provider `Arc`.

`protocol::EmbeddingProtocol` translates embedding requests and assembles
non-streaming results. `protocol::openai::Embeddings` implements this contract
using private Serde wire types and validates vector indices and dimensions.
Models can pin an optional expected output width independently of requesting
server-side resizing. Unknown widths are not inferred from model names.
The existing `Protocol` contract remains dedicated to conversational generation.
Protocol errors retain the existing concrete-to-protocol-to-provider layering.

Embedding model IDs are currently explicit. Existing chat discovery semantics
are unchanged; mixed-operation catalogs and embedding pricing are future work.

### Decision models

`provider::DeciderProvider` provides typed choice, score, and boolean questions
independently of chat and embeddings. `LlamaCpp` implements it using the
TypeSafe-compatible `/v1/systemone` endpoint on direct servers and routers.
`protocol::DeciderProtocol` is the non-streaming adapter contract;
`protocol::system_one::SystemOne` owns wire translation and answer validation.
`SystemOneError` is transparently linked through `ProtocolError`.

Applications retain named answers, model probabilities, confidence, score
legends, and usage. They own thresholds and action execution. Models are selected
by explicit IDs rather than inferred from names. See [decision models](deciders.md)
for API examples, media support, and model-dependent serving requirements.

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

History replay is independent of thinking settings for the next generation.
Adapters must reject reasoning they cannot represent, including unsigned
thinking without a supported replay field. Redacted payloads are not disposable.
Any lossy history conversion must be an explicit caller operation.

### Structural replay validation

`protocol::validate_replay(messages, tools)` checks complete inference views.
The agent and all built-in conversation adapters use this same read-only
validator. Custom protocol implementations can call it directly.
`ReplayError` links transparently through `ProtocolError` and `ProviderError`,
or directly through `AgentError` for agent-side preparation failures.

- Tool call IDs must be nonempty and unique across the supplied view; each call
  needs exactly one later result. Results with empty IDs, no preceding call, or
  a prior result are rejected. Results may arrive out of call order.
- Tool calls and reasoning are assistant-only; results are user-only.
  Tool call names and advertised declaration names must be nonempty.
  Declaration names must be unique.
- `loaded_tools` names must be nonempty and unique within each result. These
  names and historical call names need not appear in current declarations:
  old exchanges do not authorize new execution. There is no deferred-declaration
  representation or dynamic activation contract today; built-in adapters still
  reject all nonempty activation metadata.
- An explicit interrupted/not-completed result closes a call structurally.
  A bare unresolved call does not authorize inference, retries, or execution.

Errors retain call IDs and zero-based `ReplayLocation { message, block }`
positions. Duplicate errors include the first occurrence; declaration and
activation-name errors include their list indices. The earliest unresolved call
is reported when multiple results are missing.

The validator does not require alternating roles, immediate result adjacency,
or declaration visibility for historical calls. Observations and request-only
text/image context can be interleaved with complete exchanges. It does not
interpret, reserialize, or remove signatures, redacted reasoning, tool thought
signatures, or multimodal data. Protocol-specific signature, image, input-shape,
and wire-representation checks remain in the existing adapters; structural
validity is not a guarantee of acceptance by every provider.

### Tool-result representations

Tool results use the shared non-recursive `history::ToolContent` representation.
Anthropic Messages emits text/image blocks inside `tool_result.content` and
validates inline image media types just as it does for user images. Responses
emits `input_text`/`input_image` parts inside `function_call_output.output`.
Both preserve part order and support remote URLs and inline images.
Chat Completions supports text-only tool results and returns
`UnsupportedChatToolImage` with the call ID for images; it never promotes them
to unrelated user messages. Single text results retain the OpenAI string wire
shape; empty or multiple text parts use arrays.

The common `is_error` flag is retained in execution/history and sent natively
by Anthropic. OpenAI's function-output wire formats have no equivalent flag;
their output carries the supplied content without inventing a wire field.
All implemented adapters reject nonempty `loaded_tools`: deferred activation
is not implemented, and activation metadata must not disappear silently.
Concrete protocol errors remain linked through `ProtocolError`/`ProviderError`.

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
