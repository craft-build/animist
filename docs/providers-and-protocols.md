# Providers and protocols

## Status and scope

The catalog and public provider interface are implemented. OpenAI's provider
supports Chat Completions and Responses; Anthropic's provider supports Messages.
Synthetic, Z.AI Coding Plan, Ollama, llama.cpp, and oMLX reuse the OpenAI Chat
Completions adapter.
The optional Amazon Bedrock provider composes the native Anthropic/OpenAI
adapters and Bedrock Converse with AWS credential resolution and SigV4 signing.
It optionally discovers foundation models and paginated inference profiles
through signed control-plane GET requests.
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
With the `bedrock` feature, `protocol::bedrock::converse::Converse` implements the
same contract using private Converse wire types and a bounded AWS EventStream
decoder with prelude/message CRC32 checks. It preserves signed and redacted
reasoning, tool calls, and inline image inputs/results. Model-specific controls
are explicit additional request fields, not inferred protocol dialects.
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

### Sampling controls

`provider::RequestOptions` is the single source of truth for optional
`temperature`, `top_p`, `frequency_penalty`, `presence_penalty`, and
`stop_sequences`. Agents use it through `RunOptions::request` and
`StepRequest::options`; layers can adjust the same type during preparation.
Existing thinking, fast mode, and output-limit behavior is unchanged.

```rust
use animist::{agent::RunOptions, provider::RequestOptions};

let options = RunOptions::builder()
    .request(RequestOptions::builder()
        .temperature(0.7)
        .stop_sequences(vec!["END".into()])
        .build())
    .build();
```

Omitted controls are not sent; explicit zero values and empty stop lists are
preserved when supported. The settings remain runtime-only, not serializable.
Builder and `Default` both leave sampling omitted, thinking off, and fast false.

| Boundary | Accepted controls and local policy |
| --- | --- |
| Chat Completions | Temperature `[0, 2]`, top-p `[0, 1]`, both penalties `[-2, 2]`, up to four stops. |
| Responses | Temperature `[0, 2]` and top-p `[0, 1]`. Explicit penalties and stops, including an empty list, are unsupported. |
| Anthropic Messages | Temperature `[0, 1]` or top-p `[0, 1]`, and stops. Penalties are unsupported. The adapter rejects setting both temperature and top-p, or setting either with enabled/adaptive thinking. |
| Bedrock Converse | Temperature `[0, 1]`, top-p `[0, 1]`, and up to four stops. Penalties and portable thinking/fast controls are unsupported; model-specific settings are explicit additional request fields. |
| Z.AI Coding Plan | Temperature `[0, 1]`, top-p `[0.01, 1]`, and at most one stop. Penalties are unsupported. |

All numbers must be finite; stop strings must be nonempty. Synthetic, Ollama,
llama.cpp, and oMLX share the Chat Completions contract. Bedrock follows its
selected adapter. Z.AI narrows that contract in its provider, using its documented
single-stop restriction rather than assuming full OpenAI compatibility.

These are conservative adapter contracts, not promises about every model or
server version. No model-specific sampling capability is inferred from names or
thinking metadata. Additional model restrictions remain server-validated, and
HTTP/API failures retain their existing upstream payloads. Protocol validation
uses public `SamplingError` variants through `ProtocolError::Sampling` and
`ProviderError::Protocol`; provider-specific restrictions use `ProviderError`.
Adding request fields and error variants affects struct literals and exhaustive
matches in downstream crates.

Compatibility references:
[Synthetic](https://dev.synthetic.new/docs/openai/chat-completions),
[Ollama](https://docs.ollama.com/api/openai-compatibility),
[oMLX wire types](https://github.com/jundot/omlx/blob/main/omlx/api/openai_models.py),
and [Z.AI](https://docs.z.ai/api-reference/llm/chat-completion).

### System-message placement

History supports text-only `history::Role::System` messages, distinct from user
observations and assistant dialogue. The existing separate `system` argument
continues to work; no provider automatically demotes history instructions.

| Adapter / providers | Placement policy |
| --- | --- |
| OpenAI Chat Completions | Emits each history instruction as a `system` message at its exact position; the separate preamble remains the first system message. |
| OpenAI Responses | Emits ordered `system` input messages; the separate preamble remains in `instructions`. |
| Synthetic, Z.AI Coding Plan, Ollama, llama.cpp, oMLX | Use the Chat Completions translation, preserving roles and order on the wire. Actual model/chat-template support is server-dependent. |
| Anthropic Messages | Cannot represent at-depth system messages. Rejects any history system message by default, including leading ones, rather than silently relocating it. |
| Amazon Bedrock | Native APIs follow their selected adapter. Converse rejects system history unless `MoveToPreamble` explicitly appends its ordered text blocks to `system`. |

For Anthropic, opt in per request to losing placement while retaining instruction
authority:

```rust
use animist::provider::{RequestOptions, SystemMessagePolicy};

let options = RequestOptions::builder()
    .system_message_policy(SystemMessagePolicy::MoveToPreamble)
    .build();
```

The default is `SystemMessagePolicy::Reject`. On Anthropic, rejection returns
`AnthropicProtocolError::UnsupportedSystemPlacement { message, .. }`, with a
zero-based index into the original request view, transparently wrapped through
`ProtocolError::Anthropic` and `ProviderError::Protocol`, before HTTP transport.
Fallback appends instruction text blocks after the separate preamble in history
and block order, separating nonempty accumulated text with two newlines, and
omits the system entries from dialogue. It does not edit stored history.
OpenAI adapters preserve placement even when this fallback option is selected.

System messages accept text blocks only, enforced by shared replay validation.
Wire preservation does not guarantee that every compatible endpoint or model
supports multiple or interleaved system messages: upstream restrictions remain
API errors, never automatic relocation. The same applies to provider-specific
model restrictions on instruction roles.

Public API additions are `Role::System`, `SystemMessagePolicy`,
`RequestOptions::system_message_policy`, `ReplayError::NonTextSystemContent`,
and the Anthropic placement error. Update exhaustive enum matches and direct
request-option struct literals. Existing serialized user/assistant messages and
the `stream_message()` signature are unchanged. Custom protocols must explicitly
preserve instruction authority/placement or reject unsupported input; custom
providers must forward the policy. See [history](history.md) for edits and VCC's
conservative instruction-retention boundary.

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
Bedrock Converse supports text and inline image result parts in order, with
native success/error status. Remote image URLs are rejected rather than fetched.

The common `is_error` flag is retained in execution/history and sent natively
by Anthropic. OpenAI's function-output wire formats have no equivalent flag;
their output carries the supplied content without inventing a wire field.
All implemented adapters reject nonempty `loaded_tools`: deferred activation
is not implemented, and activation metadata must not disappear silently.
Concrete protocol errors remain linked through `ProtocolError`/`ProviderError`.

### Cancellation-safe partial results

`provider::InferenceExchange` owns a provider or protocol future and a bounded
event channel. Unlike an explicit cancellable-result future, it retains observed
checkpoints when the caller stops polling, without requiring cancellation to
return through the transport result. There are no assembly tasks or background
drainers. Dropping the exchange drops transport; dropping only `next()` leaves
the exchange resumable. Call `cancel()` to drop transport and inspect retained
output. Dropping the exchange itself cannot return a result.

```rust,no_run
use animist::provider::{ExchangeEvent, InferenceExchange, Model, RequestOptions};

async fn example(model: &Model) -> animist::provider::Result<()> {
    let mut exchange = InferenceExchange::new(|tx| async move {
        model.provider.stream_message(
            model, &[], "", &[], &tx, RequestOptions::default(),
        ).await
    });
    while let Some(event) = exchange.next().await {
        match event {
            ExchangeEvent::Provider(event) => println!("{event:?}"),
            ExchangeEvent::Finished(result) => { result?; break; }
        }
    }
    let _partial = exchange.partial();
    Ok(())
}
```

`PartialResponse` separates display-only text/thinking from an ordered prefix of
protocol-validated closed content blocks. Display deltas are never promoted into
replay content. `ProviderEvent::ContentComplete` carries a closed block;
`ProviderEvent::Usage` carries the latest cumulative usage for this attempt,
not an additive increment. A connected event receiver must drain these events
as well as deltas. Custom implementations can emit these checkpoints through the
unchanged provider/protocol method signatures; without them, incomplete output
has only display deltas and unknown usage.

- Anthropic closes blocks at `content_block_stop`, after object-input and
  nonempty-thinking-signature validation. A signature fragment alone does not
  close thinking. Redacted data survives unchanged. Interleaved stops emit only
  the contiguous closed prefix, preserving order.
- Chat Completions closes content at its choice finish reason, not when tool
  JSON first becomes parseable. Unsigned reasoning remains specific to a
  `reasoning_content` replay-capable dialect. Usage-only chunks update usage
  even if cancellation occurs before `[DONE]`.
- Responses closes items at `response.output_item.done`, preserving output-index
  order and the original opaque reasoning item. In-progress/incomplete items
  block checkpointing across that gap. Terminal usage is reported before final output translation;
  valid failure usage is retained without replacing the upstream failure when
  its usage payload is malformed.
- Converse closes blocks at `contentBlockStop` after tool JSON/signature
  validation and emits only the contiguous closed prefix. Signatures alone do
  not close thinking. Redacted bytes are assembled before base64 replay.
  Post-stop metadata reports cumulative usage including cache reads/writes;
  success also requires clean EventStream EOF.

These are replay **candidates**, not a complete inference request or permission
to execute tools. They remain tied to the producing protocol/dialect. A caller
choosing to persist them must explicitly close tool calls with truthful results,
preserve opaque dependencies, and validate the resulting request. No partial
content is automatically executed or committed by the agent.

Each built-in SSE exchange accepts at most 16 MiB of total wire bytes, in
addition to the existing event/framing limits. The exchange retains at most
16 MiB across display strings and serialized closed blocks; if the budget is
exceeded, `retention_limited` is set and no later blocks are retained across the
gap. Usage remains available. Channels apply backpressure; retained output is
not an unbounded event log. A checkpoint is observable only after the producer
enqueues it; cancellation cannot recover unreported wire state or server usage.
Errors retain their concrete protocol/provider variants and upstream payloads
in `ExchangeEvent::Finished`, independently of the partial output.
Converse applies the same 16 MiB total-wire budget to AWS EventStream framing
and validates both CRC32 checksums, length fields, and header structure.

This adds provider event variants and changes `ProviderEvent` equality from
`Eq` to `PartialEq` because usage includes an optional floating-point cost.
Update exhaustive event matches; consumers rendering only deltas can ignore the
two checkpoint variants. The existing `stream_message` and `assemble` calls
remain usable without an exchange, but dropping those futures loses their state.

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
