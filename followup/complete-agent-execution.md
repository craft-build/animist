# Complete rich, concurrent, cancellation-safe agent execution

Implement the remaining shared-agent execution capabilities in Animist:
rich tool results, conflict-aware parallel tool scheduling, and replay-safe
partial inference. This is an implementation task, not just a design review.
Do not migrate or edit Craft or Revelry, expand provider coverage, or land/publish
changes without separate authorization.

## Read first

- `AGENTS.md`
- `docs/agents.md`
- `docs/history.md`
- `docs/providers-and-protocols.md`
- `src/agent/`, the provider/protocol contracts, and `tests/agent.rs`
- The focused task prompts:
  - [Rich tool results](rich-tool-results.md)
  - [Tool scheduling](tool-scheduling.md)
  - [Partial inference replay](partial-response-replay.md)
- [Replay validation](replay-validation.md) for related structural constraints.
- [Repository validation](repository-validation.md) for known baseline failures.

Treat these files as context, not proof of the current implementation: inspect
the worktree and applicable nested instructions before making changes.

## Preserve the existing architecture

- Keep the `Layer`/`AgentLayer` linked list and its explicit hook ordering.
- Keep typed native tools, heterogeneous shared executable handles, and provider
  declarations separate. Providers do not execute tools.
- Preserve tool authorization for both model-issued and nested/meta-tool calls.
- Preserve the single-active-run gate, structured errors, atomic history edits,
  pending/interrupted result records, and completed-result revisions.
- Keep serializable configuration separate from runtime handles and credentials.
- Do not add blocking I/O, `spawn_blocking`, or `block_on`.

## 1. Rich tool results

Implement a shared, non-recursive output-content representation supporting text
and `ImageSource`. Carry it through typed output conversion, tool execution,
events/reports, stored history, revisions, compaction/VCC, and protocol translation.
Do not permit nested calls, results, or reasoning inside tool output.

Preserve call identity, errors, and tool-activation metadata. Keep UI presentation
separate from model-visible content. Support images where each implemented
protocol can represent them and explicitly reject unsupported cases instead of
discarding or stringifying content. Specify history serialization compatibility
and any migration path. Do not require native output structs to serialize merely
to render readable text.

## 2. Conflict-aware parallel scheduling

Keep sequential execution as the default. Add opt-in, bounded parallel execution
with explicit exclusivity/resource-conflict policy; unknown policy is exclusive.
Do not infer safety from tool names or bake workspace-specific path rules into
Animist.

Bound active work, total calls, and nested fan-out. Design parent/child permit
handling so meta-tools cannot deadlock their children. All calls must traverse
the same authorization, visibility, cancellation, and result pipeline. A nested
stop decision must stop the run even if the parent ignores its returned error.

Preserve deterministic model-facing result order regardless of completion order.
Concurrent result commits must not overwrite sibling completions or mistake the
runtime's own writes for external history conflicts. Record pending results
before starting side effects. Retain completed results on failure/cancellation;
unfinished results must explicitly represent uncertainty. Never retry
side-effecting tools automatically.

## 3. Replay-safe partial inference

Define a coherent exchange contract across the agent, providers, and implemented
protocol adapters. Compare an explicit cancellable result with an owned exchange
object, explain the choice briefly, and implement the clear winner.

Distinguish displayable partial text from content safe for provider replay.
Preserve complete opaque reasoning/signature data without inventing signatures
or silently dropping redacted payloads. Never execute incomplete tool arguments.
Specify when complete calls from an interrupted inference may be retained or
executed; do not let display deltas alone authorize execution.

Define cancellation/error usage accounting, partial history commit points, and
retry-attempt identity so already-visible output is not duplicated ambiguously.
Keep assembly state request-owned and buffering bounded. Dropping an exchange
must stop transport-owned work; no detached assembly or cleanup workers.
Cooperative cancellation must allow the runtime to finalize its history.

## Implementation workflow

1. Establish the current baseline and identify affected public APIs and consumers.
2. Explain consequential contract choices before implementing them. Ask only if
   a genuine unresolved product decision blocks progress.
3. Implement in cohesive stages, updating tests and documentation with each stage.
   Complete all three capabilities rather than replacing this task with more
   follow-up prompts.
4. Validate their interaction, not just each subsystem in isolation.

## Required evidence

Add external-crate API tests and deterministic protocol fixtures covering:

- Typed text-plus-image output, history serialization/restoration, revisions,
  compaction, and unsupported protocol representations.
- Concurrency bounds, conflict serialization, conservative unknown policy,
  nested dispatch, authorization denial/stop, timeout/cancellation, and stable
  result ordering.
- Parallel completion and cancellation while history is being revised, including
  preservation of finished sibling results and detection of external mutations.
- Cancellation/failure between text fragments, during tool JSON, around reasoning
  signatures/redacted payloads, and after usage reporting.
- Dropping an inference or tool future, resuming the resulting conversation, and
  proving no orphan results, fabricated reasoning, or unapproved tool execution.
- Rich results produced by parallel tools in a run whose next inference is
  cancelled, followed by a valid resumed request.

Run:

```sh
cargo fmt --all -- --check
cargo clippy --lib --test agent --offline -- -D warnings
cargo test --lib --test agent --test history --test decider --offline
cargo test --offline
cargo check --all-features --offline
git diff --check
```

If baseline failures remain, identify them separately from regressions and provide
targeted passing checks. Do not weaken validation or reintroduce obsolete model
construction just to make fixtures compile. Live-provider tests are supplementary
and must not require secrets in source files.

## Deliverable

Leave a coherent implementation, public API integration tests, and updated agent,
history, and provider/protocol documentation. Summarize breaking API or serialized
history changes, the guarantees now provided, exact checks run, and any remaining
limitations. Do not claim complete validation when repository-wide checks remain
blocked.
