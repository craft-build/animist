# Define cancellation-safe partial inference results

Status: implemented. `InferenceExchange` retains bounded display output,
protocol-validated closed content, and latest usage across cancellation and
errors. The agent exposes interrupted attempts without automatic replay or tool
execution. See `docs/providers-and-protocols.md` and `docs/agents.md`.

The original implementation prompt follows for historical context.

Implement in Animist without changing Craft or Revelry. Read the agent, provider,
protocol, and history contracts before editing.

## Problem

Provider::stream_message returns assembled content only on success. Dropping its
future cancels transport and loses assembly state. ProviderEvent deltas do not
contain every reasoning signature/opaque payload needed for faithful replay.
The agent currently retains display events but commits no incomplete inference.

## Goal

Define an explicit partial-result contract that distinguishes displayable partial
text from replay-safe structured content and never invents signatures.

## Requirements

- Compare an explicit cancellable exchange result with an owned exchange object;
  choose one coherent contract across implemented adapters.
- Keep stream state request-owned and bounded. No detached assembly tasks.
- Preserve complete tool calls and opaque reasoning/signature data when valid.
- Never execute incomplete arguments or replay invalid partial thinking.
- Specify cancellation/error usage reporting, failed-attempt accounting, retry
  attempt identity, and handling already-visible text across recovery.
- Keep concrete protocol errors and upstream data intact.
- Maintain pending-result safety for tools already started by the agent.

## Validation

Use deterministic SSE fixtures for cancellation between text fragments, inside
tool JSON, before/after signatures, redacted thinking, failures after deltas, and
usage-only endings. Verify dropping the exchange ends transport-owned work.
Run cargo test and cargo check --all-features.
