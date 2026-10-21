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
