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

## Traits and shared handles

- Make traits async using `async-trait`.
- For every trait that will have multiple implementations, use the `arc-handle`
  proc macro to handle wrapping the types.

## Providers and protocols

- Before implementing providers or protocols, read
  [the provider and protocol design](docs/providers-and-protocols.md).
- Separate protocol wire types, translation, and stream assembly from provider
  authentication, endpoint selection, and model policy.
- Build concrete adapters alongside real providers; do not introduce a universal
  protocol framework before implementations demonstrate the need.

## Construction patterns

- Use the existing `Struct!` and `Enum!` derive aliases for new types where their
  derived traits fit.
- Use `bon::Builder` with `#[builder(on(_, into))]` for structs intended for
  builder construction.
- Prefer builder construction over struct literals when a builder is available,
  including nested objects.

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
