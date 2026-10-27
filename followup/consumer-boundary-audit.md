# Audit downstream compatibility without migrating applications

Review Animist public APIs against ~/Projects/craft-code/agent and
~/Projects/Revelry as read-only references. Do not edit either application.

## Goal

Identify remaining Animist contract gaps before a separately authorized migration.
Provider parity is not required: current users' required providers already exist
in Animist. Do not expand provider coverage without a concrete requirement.

## Audit

- Craft: typed tools, MCP output, errors, history/signature/origin fidelity,
  cancellation, request-only compression, compaction/review inference, mocks.
- Revelry: budgeted prompt views, output transformations, persistence/event
  adapters, generation/summary calls, and embeddings.
- Reuse existing provider::EmbeddingProvider for remote embedding paths. Identify
  the adapter needed for local fastembed separately; do not invent a second
  embedding abstraction or conflate embeddings with agent construction.
- Inventory remaining Rig dependencies by subsystem, distinguishing library gaps
  from application adaptation work.

Deliver a concrete checklist with code references and proposed tests. Any future
application edits require their repositories to be attached and migration scope
explicitly authorized. This audit does not authorize a migration.
