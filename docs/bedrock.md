# Amazon Bedrock

Enable the optional `bedrock` Cargo feature to use `provider::bedrock::Bedrock`.
The default build does not enable AWS dependencies.

```toml
[dependencies]
animist = { version = "0.1", features = ["bedrock"] }
```

This provider uses AWS's credential provider chain and SigV4 signer, `reqwest`
for HTTP, and Animist's protocol adapters. Native Anthropic/OpenAI surfaces use
SSE; Converse uses a bounded, CRC-checked AWS EventStream decoder. It does not
wrap `InvokeModel`.

## Construction

```rust
use animist::provider::{
    Provider,
    bedrock::{Bedrock, BedrockConfig, Protocol},
};

async fn make_provider() -> animist::provider::Result<Provider> {
    let config = BedrockConfig::builder()
        .region("us-east-1")
        .protocol(Protocol::AnthropicMessages)
        .build();
    Ok(Provider::new(Bedrock::new(config).await?))
}
```

`new` loads AWS configuration asynchronously with `aws-config`. Use AWS profiles,
SSO, workload roles, or the environment-based credentials supported by that
chain. Do not put access keys, secret keys, or session tokens in source code.
`profile` selects a profile; `region` overrides the resolved SDK region.

`from_sdk_config(config, &sdk)` accepts caller-owned AWS configuration, including
custom refreshable credential providers. It does not run default credential
discovery. Its profile has already been selected by the caller; the config's
`profile` field is used only by `new`. `SdkConfig`, `Region`, `Credentials`,
`ProvideCredentials`, `CredentialsFuture`, and `SharedCredentialsProvider` are
re-exported through `provider::bedrock`.

Credentials are resolved through the provider for every request attempt, not
retained as a fixed snapshot. The exact serialized body, URL, headers, region,
and session token are signed using `aws-sigv4`, with signing service `bedrock`.
Animist does not add a separate credential cache or implement cryptography.

`refresh_auth` resolves credentials through the current provider. `reload_auth`
rebuilds and validates the default chain for instances constructed with `new`,
retaining the old provider if validation fails. With injected SDK configuration,
it resolves through the caller's provider instead. Neither method changes the
resolved routing region.

## Protocols and endpoints

The default origin is
`https://bedrock-runtime.{region}.amazonaws.com`.

| Configured protocol | Request path | Shared adapter |
| --- | --- | --- |
| `anthropic_messages` (default) | `/anthropic/v1/messages` | Anthropic Messages |
| `chat_completions` | `/openai/v1/chat/completions` | OpenAI Chat Completions |
| `responses` | `/openai/v1/responses` | OpenAI Responses |
| `converse` | `/model/{id}/converse-stream` | Bedrock Converse |

Selection is explicit, never inferred from model names. Confirm that your model
supports the chosen API in the chosen region. Requests to unsupported APIs are
not retried through a different protocol or endpoint.

Set `Model::provider` to a shared `Provider` handle wrapping `Bedrock`.
Supply the exact model ID or inference profile ID required by that API. Native
APIs send it unchanged in the body; Converse encodes it as a single URL path
segment, including any slash in a model/profile ARN.
For example, some models require a geographic or global inference profile rather
than a foundation model ID. IAM permissions, model access, quotas, and regional
availability remain controlled by AWS.

`endpoint_url` overrides the runtime origin, followed by a supplied SDK endpoint
override, then the default origin. It must not contain a path, credentials, query,
or fragment. HTTPS is required except on loopback for mock tests. Redirects are
disabled. For a partition or PrivateLink endpoint that does not use the default
hostname pattern, provide its runtime origin explicitly.

This implementation supports `bedrock-runtime` with IAM SigV4 authentication,
not `bedrock-mantle`, Bedrock bearer/API keys, or automatic endpoint fallback.

## Models and inference policy

`ListingMode::Catalog` (the default) returns the optional configured
`ProviderCatalog`, using its map keys as model IDs, without resolving credentials.
With no catalog it returns an empty list; a caller can still supply a `Model`
directly. Runtime never calls the OpenAI `/models` endpoint.

Set `listing_mode(ListingMode::ControlPlane)` to discover foundation model IDs
and inference profile IDs with signed `GET /foundation-models` and
`GET /inference-profiles` requests. Profiles are paginated; IDs are deduplicated
and sorted. Catalog metadata is merged by exact ID over discovered entries;
catalog-only IDs are not added, and profile metadata is not guessed from names.
Unknown metadata uses `ModelInfo::default()`, including unknown prices.

The control-plane origin defaults to `https://bedrock.{region}.amazonaws.com`.
`control_plane_endpoint_url` overrides it independently of runtime and SDK
endpoint overrides, with the same origin validation rules. Listing uses the
same injected HTTP client, credential refresh, signing, retries, and HTTP error
handling as inference. It requires `bedrock:ListFoundationModels` and
`bedrock:ListInferenceProfiles` permissions. Discovery is bounded to 16 MiB of
total JSON response bytes and rejects empty/repeated pagination tokens.

Adding listing settings and `BedrockError` variants is a breaking public API
change for exhaustive matches and direct configuration struct literals.

Catalog metadata is not proof of account access, regional/API availability, or
inference-profile eligibility. It remains separate from runtime policy and is
not modified by inference.

Thinking support, required thinking, and advertised efforts are taken from
`Model::info` and its runtime overrides:

- Messages uses Anthropic's disabled/adaptive/budget settings and effort values.
  Budgets must be at least 1,024 tokens and below the output limit.
- OpenAI-compatible APIs use `reasoning_effort`. Token-based budgets are rejected.
  Requested efforts must appear in the model's metadata and be representable by
  the selected protocol. Availability is still enforced by the service.
- Disabling new thinking does not discard history. Native Messages preserves
  signatures and redacted payloads; Responses preserves its opaque reasoning
  items. Chat Completions does not enable the nonstandard `reasoning_content`
  replay extension. Incompatible history returns protocol errors.
- Output limits use request options, then model configuration, then
  `max_output_tokens` (8,192 by default).
- Fast mode is rejected; Bedrock service-tier semantics are not silently mapped
  to another provider's fast mode.

Messages sends `anthropic-version: 2023-06-01` by default. Configure
`anthropic_version` and `anthropic_beta` when the selected Bedrock model and API
document support for those settings. These headers are included in signing.
Responses uses the shared adapter's `store: false` behavior.

## Converse

Select `Protocol::Converse` explicitly. The `bedrock` feature also exposes the
standalone `protocol::bedrock::converse::{Converse, ConverseOptions}` adapter.
It implements the existing `protocol::Protocol` contract; AWS authentication,
endpoint selection, and retries remain in the provider.

Converse supports text, inline PNG/JPEG/GIF/WebP image inputs, tool calls with
object input, and text/image tool results with native success/error status.
Remote image URLs are rejected rather than fetched. Tool declarations use
`toolSpec`/`inputSchema.json`. Unsupported content, tool thought signatures,
and nonempty `loaded_tools` metadata are rejected, never silently discarded.
Shared structural replay validation runs before translation or signing.

Signed thinking maps to `reasoningContent.reasoningText`, retaining text and
signature unchanged; unsigned thinking is rejected. Redacted reasoning maps
to the opaque base64 `redactedContent` field. These are replay candidates for
Converse, not a guarantee of cross-protocol or cross-model portability.
Keep the original history unmodified when replaying signed reasoning.

Converse has no portable thinking switch. Generic `RequestOptions::thinking`
values other than `Off` are rejected instead of guessing a model's dialect.
Here, `Off` means no portable thinking control is sent; it does not suppress
model-specific fields or alter reasoning history. Supply the selected model's
documented additional settings explicitly through
`BedrockConfig::converse_additional_model_request_fields` (a JSON object), or
`ConverseOptions::additional_model_request_fields` when using the adapter
directly. These settings apply to every Converse request on that provider.
Catalog-required thinking still requires a deliberate runtime policy override
when controls are supplied this way. No settings are inferred from model names.

Portable controls are `maxTokens`, temperature `[0, 1]`, top-p `[0, 1]`, and up
to four nonempty stop sequences. Omitted controls stay omitted; explicit zero
sampling values and empty stop lists survive. Frequency/presence penalties and
fast mode are unsupported. Extra model restrictions remain server-validated.
At-depth system messages are rejected by default; `MoveToPreamble` preserves
instruction authority by appending ordered text blocks to Converse's `system`.

The decoder validates both prelude and message CRC32 checksums, header framing
and uniqueness, lengths, and truncated frames. The total exchange, and therefore
each frame and assembled output, is bounded to 16 MiB of wire data. Unread HTTP
bytes, framing state, and assembled content are separate request-owned state.
No background assembly task is spawned; dropping the exchange cancels transport.

Text and reasoning deltas are display-only. `contentBlockStop` validates tool
JSON and reasoning signatures and checkpoints only the contiguous closed
content prefix; a closed later block cannot cross an earlier open block.
Usage metadata arrives after `messageStop` and checkpoints cumulative input,
output, cache-read, and cache-write tokens. Both stop and metadata, followed by
clean end-of-stream, are required for a successful complete response.
`InferenceExchange` retains observed checkpoints when cancelled or failed.

`end_turn`/`stop_sequence`, `tool_use`, `max_tokens`, and
`model_context_window_exceeded` map to the corresponding shared stop reasons;
`guardrail_intervened`/`content_filtered` map to `Refusal`. Unknown stop reasons
and unsupported stream events fail explicitly. EventStream exceptions preserve
their type, headers, and raw payload through
`BedrockProtocolError::Api` → `ProtocolError::Bedrock` → `ProviderError::Protocol`.
Stream errors are never retried.

`Protocol::Converse`, `ProtocolError::Bedrock`, and the new configuration/error
fields are breaking additions for exhaustive matches and direct struct literals.

## Failures, retries, and cancellation

Only HTTP 429 and 503 responses are retried, before stream assembly begins.
`max_attempts` defaults to 3 and must be between 1 and 10. Every attempt resolves
credentials and signs anew. Backoff starts at 200 ms and doubles; integer
`Retry-After` seconds override it. Both are capped at 30 seconds. HTTP-date
`Retry-After` values fall back to exponential backoff.

Transport failures, credential errors, other HTTP statuses, and stream errors
are not automatically retried. Once an HTTP success response is handed to a
protocol adapter, inference is never transparently restarted. Dropping the
request future cancels sending, retry backoff, or stream assembly; no inference
task is detached.

`BedrockError` retains concrete AWS credential/signing errors and HTTP status,
request ID, error type, and raw response bytes. Error bodies are bounded to
64 KiB; oversized responses return `ErrorBodyTooLarge` with the retained prefix.
These errors transparently enter `ProviderError::Bedrock`; protocol errors keep
their existing concrete protocol error chain.

The HTTP client disables redirects and has a 10-second connection timeout, with
no overall generation timeout. `with_client(reqwest::Client)` replaces that
client without changing settings or `max_attempts`. Injected clients **must**
disable redirects with `redirect(reqwest::redirect::Policy::none())`; reqwest
cannot inspect or override an existing client's policy. Client timeouts apply
per attempt, not across retry backoff, and timeout errors are not retried.
AWS authentication settings, runtime HTTP state, and serializable provider
settings remain separate.

## Validation

For live streaming and a local tool-call round trip, use the
[manual Bedrock example](manual-provider-tests.md#amazon-bedrock):

```sh
AWS_PROFILE=my-profile AWS_REGION=us-east-1 \
  cargo run --features bedrock --example bedrock -- --tool-test
```

The example uses Anthropic Messages. Configure AWS credentials and select a
model/inference profile accessible to your account; live requests may incur charges.

```sh
cargo test --features bedrock --all-targets
cargo test --no-default-features --all-targets
cargo clippy --all-features --all-targets
```

Mock tests use fake credentials and loopback endpoints, including verification
of signatures against the bytes actually received. They do not validate live
AWS account permissions or model/API availability. Live inference requires a
separate explicitly configured smoke test.

## References

- https://docs.aws.amazon.com/bedrock/latest/userguide/inference-messages-api.html
- https://docs.aws.amazon.com/bedrock/latest/userguide/inference-chat-completions.html
- https://docs.aws.amazon.com/bedrock/latest/userguide/inference-responses-api.html
- https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html
- https://smithy.io/2.0/aws/amazon-eventstream.html
- https://docs.rs/aws-config/
- https://docs.rs/aws-sigv4/
