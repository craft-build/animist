# F09 — Bedrock: injectable client, model listing, Converse

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

This prompt has three independent parts. Land them as separate commits or PRs,
in order.

## Part A — `with_client`

Every other provider exposes `with_client(reqwest::Client)`, which craft uses
for its connect and stream timeouts. `Bedrock` builds its own client and
retries internally (`max_attempts`). Add `Bedrock::with_client`, with the same
semantics as the other providers (redirects stay disabled), and document how it
interacts with `max_attempts`.

Test: an injected client's timeout fires against a stalled local mock.

## Part B — Model listing

`Bedrock::list_models` only returns models from an explicit
`BedrockConfig::catalog`. Craft lists foundation models and inference profiles
through the control plane (`aws-sdk-bedrock`).

- Add an optional listing mode built on SigV4 `GET` requests with the existing
  signer: `ListFoundationModels` and `ListInferenceProfiles` on
  `bedrock.{region}.amazonaws.com`. Merge the results with catalog metadata,
  the way other providers do.
- Keep it async, without adding the full AWS SDK.

Tests: a mock control-plane server with paginated profiles.

## Part C — Converse (larger; confirm scope with the maintainer first)

Some Bedrock models are only available through Converse (`/model/{id}/converse-stream`,
AWS EventStream framing). Following `docs/providers-and-protocols.md`, add a
`protocol::bedrock::converse` adapter and `Protocol::Converse`:

- wire types and translation, covering:
  - text, images, tool use, and tool results (including images);
  - reasoning content with signatures;
  - stop reasons, and usage with cache tokens;
- an EventStream frame decoder (a bounded buffer, with CRC checks);
- replay validation consistent with the other adapters.

Tests: fixture streams for text, tool calls and reasoning; malformed frames;
partial-response checkpoints.

## Craft cleanup after release

02: pass the timeout client, restore Bedrock `max_attempts` policy
deliberately, drop the craft control-plane listing if Part B covers it, and
offer `converse` per model if Part C landed.

## Completion note

Implemented Parts A–C on `feat/f09-bedrock`, unreleased:

- A: injected HTTP client, per-attempt timeout/retry documentation, and a stalled
  loopback timeout test. Craft owns disabling redirects on its injected client.
- B: opt-in signed control-plane foundation/profile discovery, profile
  pagination, exact-ID catalog merging, independent endpoint overrides, and
  bounded discovery with cycle detection.
- C: explicit Converse routing and standalone adapter, typed request translation,
  inline images and image tool results, signed/redacted reasoning replay,
  bounded CRC-checked EventStream assembly, cache usage, stop reasons, and ordered
  partial-response checkpoints. Full scope confirmed by the maintainer.

Converse model-specific thinking controls use explicit additional request fields;
generic thinking controls are rejected rather than guessed from model IDs.
See [the Bedrock contract](../bedrock.md).

Independent review of the full A–C diff returned clean with no medium-or-higher
findings. Validation passed:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo test --features bedrock`
- `cargo test --all-features --all-targets`
- `cargo test --no-default-features --all-targets`
- `cargo doc --no-deps` and `cargo doc --all-features --no-deps`
- `git diff --check`

Bedrock coverage includes 20 provider tests and 10 Converse tests, using fake
credentials and loopback mocks. No live AWS inference or permissions check was
performed. The pre-existing live models.dev catalog test remains ignored.
Craft cleanup remains a post-release task.
