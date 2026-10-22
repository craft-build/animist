# Amazon Bedrock

Enable the optional `bedrock` Cargo feature to use `provider::bedrock::Bedrock`.
The default build does not enable AWS dependencies.

```toml
[dependencies]
animist = { version = "0.1", features = ["bedrock"] }
```

This provider uses AWS's credential provider chain and SigV4 signer, `reqwest`
for HTTP, and Animist's existing protocol adapters and SSE reader. It does not
implement Converse or wrap `InvokeModel`/AWS EventStream.

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

Selection is explicit, never inferred from model names. Confirm that your model
supports the chosen API in the chosen region. Requests to unsupported APIs are
not retried through a different protocol or endpoint.

Set `Model::provider` to `amazon-bedrock`. Supply the exact model ID or inference
profile ID required by that API; IDs are sent unchanged in the request body.
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

`list_models` returns the optional configured `ProviderCatalog`, using its map
keys as model IDs. With no catalog it returns an empty list; a caller can still
supply a `Model` directly. Runtime does not implement the OpenAI Models API, so
this provider never calls `/models`. There is no automatic control-plane model
discovery in this implementation.

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
no overall generation timeout. There is currently no custom HTTP-client
injection API. AWS authentication settings, runtime HTTP state, and serializable
provider settings remain separate.

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
- https://docs.rs/aws-config/
- https://docs.rs/aws-sigv4/
