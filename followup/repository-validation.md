# Reconcile existing fixtures with the current runtime Model API

Implement in Animist as a separate cross-cutting maintenance task. Do not change
the agent runtime to restore obsolete provider-name construction or fabricate
Model::default() with a nonfunctional provider.

## Observed baseline failures

The shared-agent workstream ran cargo test --offline and
cargo check --all-features --offline. Existing targets fail to compile:

- tests and examples still pass provider-name strings where Model now owns
  Arc<Provider>, or expect Model::default()/Debug/PartialEq.
- several tests expect PartialEq on types whose derive aliases no longer supply it.
- Bedrock still compares model.provider.as_ref() with a string and passes that
  handle as the provider-mismatch actual name.
- several Anthropic test cases refer to the provider function rather than a
  constructed provider handle.

The provider integration-test errors were reproduced on an untouched HEAD archive
during agent implementation. These are not failures introduced by agent execution.

## Requirements

- Inspect the current public Model/Provider construction boundary and update
  fixtures/examples/docs to real runtime handles.
- Use scripted providers for tests that do not require transport; retain actual
  protocol and authentication checks where those are the subject.
- Add focused derives where equality is appropriate; do not derive Debug on
  credential-bearing runtime types.
- Fix Bedrock's provider identity validation using the current provider API,
  preserving typed mismatch errors.
- Audit README and examples for the same obsolete API usage.
- Keep the new tests/agent.rs integration tests passing.

## Validation

Run cargo fmt --all -- --check, cargo test --offline, and
cargo check --all-features --offline. If local dependency availability prevents
offline validation, report that separately rather than changing dependencies
without cause. No live credentials are necessary.
