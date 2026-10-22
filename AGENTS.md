# Animist agent guidelines

Animist is a shared Rust crate for tools including Craft, an AI coding harness,
and Revelry, an AI roleplay application. These rules apply to all AI work in this
repository.

## Async-native code

- Use Tokio and async-compatible crates wherever possible.
- Use `tokio::io`, `tokio::fs`, and other async APIs instead of blocking standard
  library I/O and filesystem operations.
- Do not add `spawn_blocking`, `block_on`, or other blocking code without explicit
  user approval.

## Comments and documentation

- Avoid narrative comments.
- Keep documentation comments short, succinct, and focused on intent.
- Limit each documentation comment to a maximum of five lines.

## Error handling

- Use `snafu` for error handling.
- Use its `ResultExt` and `OptionExt` trait providers for `.context(...)` where
  appropriate.
- Keep errors within their owning layer. Protocol implementations return
  protocol errors, not provider errors; use `#[snafu(transparent)]` to link
  concrete protocol errors to `ProtocolError` and then `ProviderError`.
- Define variants for specific failure conditions, with source errors and
  relevant context fields. Do not replace these with generic `message: String`
  errors or helper functions that manufacture them. Preserve upstream error
  payloads as data rather than flattening them into local error messages.

## Traits and shared handles

- Make traits async using `async-trait`.
- Use `arc-handle` when a trait needs a clonable shared handle, as with providers.
  Multiple implementations alone do not require shared ownership: stateless
  protocol adapters only need `Send + Sync`.
- Keep implementation bodies in their trait methods rather than forwarding to
  free functions with the same inputs and behavior.

## Rust structure and abstractions

- Do not introduce helpers solely to shorten expressions. Inline trivial field
  access, `.context(...)`, channel sends, and builder construction at their use
  sites, even when formatting wraps them across lines.
- Extract helpers for meaningful algorithms, policies, or boundaries, not merely
  to turn a method call into a free-function call.
- Put reusable operations on the type that owns the data, such as image URL
  conversion on `ImageSource`, rather than in protocol-root helper functions.

## Providers and protocols

- Before implementing providers or protocols, read
  [the provider and protocol design](docs/providers-and-protocols.md).
- Separate protocol wire types, translation, and stream assembly from provider
  authentication, endpoint selection, and model policy.
- Build concrete adapters alongside real providers; do not introduce a universal
  protocol framework before implementations demonstrate the need.
- Implement the existing `protocol::Protocol` contract. Keep the protocol root
  vendor-neutral and group related adapters under their protocol family, such
  as `protocol::openai::{chat_completions, responses}`.
- Define Serde wire structs and tagged enums for known request shapes instead
  of assembling requests with `json!` or mutating `serde_json::Value`. Reserve
  arbitrary JSON for genuinely open-ended schemas, metadata, and opaque payloads.
- Reuse the shared SSE reader rather than duplicating it in vendor modules.
  Keep mutable stream state owned by each request future so dropping it cancels
  the exchange.
- Avoid redundant stored copies of stream data. Distinguish unread bytes,
  framing metadata, and assembled output, and bound buffering explicitly.

## Construction patterns

- Use the existing `Struct!` and `Enum!` derive aliases for new types where their
  derived traits fit.
- Use `bon::Builder` with `#[builder(on(_, into))]` for structs intended for
  builder construction.
- Prefer builder construction over struct literals when a builder is available,
  including nested objects.
- If a derive alias is unsuitable, preserve the other useful traits rather than
  dropping them all. Add a focused alias where useful; custom defaults must agree
  with builder defaults.
- Configuration types should support Serde with stable `snake_case` names and
  consistent defaults for omitted fields. Keep runtime clients separate from
  serializable settings, omit credentials from serialization, and avoid deriving
  `Debug` on credential-bearing types.

## Catalog data and runtime policy

- Keep catalog metadata separate from runtime model overrides and provider
  behavior.
- Map source capabilities conservatively; do not infer tiers, required thinking,
  or provider-specific support from model names.
- Preserve source metadata that Animist cannot yet represent. Missing pricing
  is not evidence of free inference.

## Public API completeness

- When exposing a type through a public signature or field, ensure downstream
  crates can name it through a public module or re-export.
- Validate public API changes with integration tests that exercise the API from
  outside the crate.
