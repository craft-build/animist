# Agents

`agent::Agent` is a conversation-scoped runtime. Build one with a runtime `Model`,
an optional structured `Prompt`, a `ToolRegistry`, and an optional `AgentLayer`
linked list. Configuration is serializable; model handles, tools, layers, and
credentials are not. `AgentConfig::default()`, its builder, and deserializing `{}`
use identical limits.

```rust,no_run
use animist::{
    agent::{Agent, AgentConfig, RunOptions},
    history::{ContentBlock, Message, MessageKind, Role, StoredMessage},
    provider::Model,
};

async fn example(model: Model) -> Result<(), animist::agent::AgentError> {
    let agent = Agent::builder()
        .model(model)
        .config(AgentConfig::builder().max_steps(16_u32).build())
        .build();
    let input = StoredMessage::builder()
        .timestamp(jiff::Timestamp::now())
        .message(Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::Text { text: "Hello".into() }])
            .build())
        .build();
    let report = agent.run(input, RunOptions::default()).await?;
    println!("{:?}", report.status);
    agent.shutdown().await?;
    Ok(())
}
```

## Construction and lifecycle

- Each agent permits one active operation. Overlapping runs or shutdown return
  `AlreadyRunning`, rather than waiting or interleaving history.
- Startup runs lazily once. A failed startup rolls back initialized list nodes
  with shutdown callbacks; retrying a later run may initialize again.
- `shutdown()` is explicit, terminal, and idempotent. Asynchronous cleanup cannot
  run in `Drop`. Do not detach work inside layers, tools, or providers.
- Startup, shutdown, and finalization hooks must complete on their own. They are
  not raced against run cancellation, so resource cleanup can finish.
- Per-run request options are distinct from execution limits. Sampling controls
  beyond the existing provider options are a separate follow-up.

## Layers

Implement `LayerImpl` using `async_trait`, then wrap it with `Layer::new`.
`AgentLayer::builder().layer(handle).next(Box::new(next)).build()` constructs the
list. All hooks execute head-to-tail, including completion and shutdown.

| Hook | Scope |
| --- | --- |
| `startup` | Once per agent initialization |
| `turn_start` | Transform submitted input and metadata |
| `prepare_request` | Transform each owned inference request, including retries |
| `response` | Transform a fully assembled response |
| `pre_tool_exec` | Authorize an individual call, including child calls |
| `post_tool_exec` | Transform a model-visible tool outcome |
| `recover` | Explicitly request a bounded provider retry |
| `step_end` | Append durable user messages at a step boundary; request continuation |
| `turn_end` | Observe every admitted run's terminal status |
| `shutdown` | Release initialized resources |

Transformations receive the previous layer's output. Denial, stop, and respond
decisions short-circuit authorization; later layers cannot re-approve it. A
`Respond` decision completes the call with its supplied `ToolOutput` instead of
executing: the call is admitted against `max_tool_calls`, emits `ToolFinished`
without `ToolStarted`, and does not count as an executed call in `RunStats`.
Post-tool transformations still see denied, responded, and unknown-tool results.
Completion and shutdown attempt every node even when an earlier node fails. The
first cleanup error is returned; if both the run and its completion fail, both
errors are retained.

Request transformations do not rewrite stored history. Use them for volatile
context, budgeted request views, and selecting a fallback model. Layers may remove
tool declarations, but cannot add, modify, or restore declarations removed by a
preceding layer. Response transformations intentionally affect
replayed content; preserve opaque reasoning/signatures required by the protocol.
Raw provider deltas are not retrospectively transformed.

Before every inference attempt, the agent validates both the active durable
conversation and the final layer-produced request view using
`protocol::validate_replay`. This includes restored history and custom providers
that do not use the built-in adapters. Malformed views fail with
`AgentError::Replay` before invoking the provider; they are not provider retries.
The submitted input remains recorded if later request preparation fails.

Temporary context may be inserted between calls and results, and consecutive
messages need not alternate roles. Request-only trimming must retain complete
tool exchanges or remove them together; a missing result is not silently repaired.
An interrupted execution's explicit error result is structurally complete.
Historical tool names need not be currently registered or advertised.
Tool declaration membership in the frozen registry remains a separate execution
constraint. See the shared replay rules in `docs/providers-and-protocols.md`.

### Step boundaries

`step_end` runs head-to-tail after a step's tool results are fully committed
and before termination is decided. Each layer receives a `StepOutcome` — step
index, the assembled assistant message, its stop reason, and the terminal
status the run takes if nothing continues it (`None` when the step's tool
calls keep the run going). It returns a `StepAction`: durable `Role::User`
messages to append, typically `MessageKind::Observation`, and whether to
continue a run that would otherwise terminate. Messages from all layers append
in layer order, in one atomic commit.

Appends are validated before anything commits: `Role::User`, no tool-use or
tool-result blocks, and a replay-valid resulting view. Rejections are specific
errors (`InvalidRole`, `ToolInputMessage`, `Replay`) and commit nothing. The
commit uses the same optimistic exchange path as model exchanges, comparing
against the post-tool snapshot, so an external edit that snapshot does not
reflect — including one made inside another `step_end` hook — fails
with `HistoryChanged` and retains the uncommitted messages in the error.
Appended messages are emitted as `MessagesAppended` events so surfaces can
render them, and remain durable even when the run terminates right after.

A continuation is honored from `Completed`, `MaxTokens`, and `Paused` only;
requesting one from `Refused` or `ContextWindowExceeded` fails with
`UnsupportedContinuation` before any message commits. Each continuation
consumes a step from `max_steps`; a request with no budget left ends the run
with `StepLimit` while appended messages still commit. Terminal statuses stay
distinct — the runtime never continues or synthesizes a prompt itself.

## Typed tools

Implement `tool::ToolImpl` with typed deserializable `Args`, an `Output` implementing
`IntoToolOutput`, and a concrete error type. Native tools may generate JSON Schema
with their preferred library; schema generation is not an Animist dependency.
Serde performs argument decoding; Animist does not implement arbitrary JSON Schema
validation. Business constraints belong in typed argument validation/tool execution.

`ToolRegistry::register` erases a typed implementation behind the shared
`ToolExecutor` handle. Runtime-named tools such as MCP adapters can instead implement
`ToolExecutorImpl` and use `register_executor`. Both paths freeze the declaration
used for inference alongside the executable handle. Duplicate/empty names fail.

`ToolDefinition` re-exports the existing provider declaration, not an executable
trait. Tool execution remains outside providers and protocols.

`ToolContext` exposes the call, parent identity, run cancellation, statistics, and
progress. Its `dispatch` method executes child calls through the same registry,
layers, cancellation, and nesting limit; `dispatch_all` runs a set of child calls
under the top-level wave rules. Neither exposes raw implementations.
Child calls are internal execution details; their results are incorporated by the
parent tool rather than inserted as unmatched model tool results.

Unknown tools, argument-decoding failures, and concrete execution failures become
error results the model can correct. `ToolFailed` and `RunReport::tool_failures`
retain original structured errors behind `Arc`s, even without an event observer;
only the model-facing result renders them as text. Layer
errors and explicit stop decisions terminate the run. Tool implementations are
never retried automatically. Execution is sequential by default.

Executor panics that unwind become `ToolError::Panicked { message }`: an `is_error`
result, a `ToolFailed` event, and a retained `ToolFailure`. Siblings finish and the
run continues to the next step. This boundary covers typed argument decoding,
execution, and output conversion, including erased executors, without spawning.
String and `&str` payloads retain their text; other payloads render as
`non-string panic payload`. The panic hook still runs; aborting panics cannot be caught.
Recovery does not roll back tool side effects or repair tool-owned state.

Layer hooks run outside their call's executor boundary, so their panics unwind
rather than becoming tool failures. If a child hook panics during a parent's
`dispatch` or `dispatch_all`, that unwind crosses the parent executor boundary
and becomes a `Panicked` failure of the parent.

### Direct tool tests

Enable the `testing` feature to use `agent::testing::ToolHarness` without a
provider, model request, or history turn:

```rust,ignore
let mut harness = ToolHarness::builder()
    .tools(child_registry)
    .config(AgentConfig::default())
    .build()?;
let cancellation = harness.cancellation();
let events = harness.events();
let output = harness.call(my_typed_tool, serde_json::json!({"path": "a.rs"})).await?;
```

`call` uses the same typed decoding and output conversion as registered tools;
`call_executor` accepts an erased `ToolExecutor`. Both return structured
`ToolError`s directly, including `Arguments` and `Panicked` failures, rather than
rendering them as model-visible output. Roots run serially through mutable harness
access, with generated `harness-N` call identities. `run_id` and `cancellation` can
be supplied to the builder; `run()` exposes the shared run context and statistics.

Root invocation is deliberately direct: it bypasses authorization layers, timeout
enforcement, and run counters. Cancellation is cooperative, so a root can await
`context.run.cancellation.cancelled()`; callers may also drop its invocation
future. Child `dispatch` and `dispatch_all` use the supplied registry, optional
`layers` and `tool_scheduling_policy`, normal cancellation, timeouts, and limits.
The root's declared scheduling claim (with any policy override) controls whether
it may dispatch children. All registered children are visible. Child depth starts
at one, per-parent child budgets reset for each root, and admitted-child budgets,
stop state, statistics, and cancellation persist for the harness's lifetime.
An explicit child `Stop` decision terminates even a root that catches its dispatch
error; subsequent roots fail with the same structured `Stopped` source without
executing. Create a new harness for a fresh run.

`events()` returns a receiver for progress and child execution events. The queue
is unbounded to permit inspecting progress after a call completes; drain it during
long-running tests. Receiver clones compete rather than broadcast. The harness
does not run layer startup, shutdown, turn, or inference hooks; supply layers whose
tool hooks work independently. Its placeholder model must not be used for
inference, and attempting that from a layer panics.

### Bounded scheduling

Set `AgentConfig::max_concurrent_tools` above its default of `1` to opt into
parallel execution. Override `ToolImpl::scheduling` (or `ToolExecutorImpl` for
erased tools) to return `ToolScheduling::Resources { reads, writes }`. Empty
resource lists explicitly claim independence. The default, `Exclusive`, conflicts
with every other call. Shared reads may overlap; any matching write conflicts.
Resource keys are opaque: applications own path normalization and alias handling.

Alternatively, supply an application-owned `ToolSchedulingPolicy` through
`Agent::builder().tool_scheduling_policy(...)`. Its synchronous `scheduling`
method receives the call and the tool's declaration and returns the effective
claim. Policy implementations must conservatively leave unknown behavior
exclusive. Claims cover the entire authorization/execution/result-hook pipeline.

The scheduler forms bounded, contiguous conflict-free waves in model order.
A conflicting call is a barrier; later independent calls do not overtake it.
It polls owned futures without spawning detached tasks. Events and revisions may
arrive in completion order, but model-facing result slots remain in call order.
Layers must be concurrency-safe when parallel execution is enabled.

Only exclusive contexts may dispatch children. `ToolContext::dispatch` runs one
child at a time; `dispatch_all` fans a set out under the same wave rules as
top-level calls: contiguous conflict-free waves in call order, claims from the
children's declarations and `ToolSchedulingPolicy`, bounded by
`max_concurrent_tools`. Results come back in call order while events arrive in
completion order. The child tree inherits the parent's exclusive reservation,
without acquiring another root slot. This stays deadlock-free: an exclusive claim
conflicts with every claim, so the parent executes alone in its top-level wave;
its children claim no resources any wave waits on; and only the parent ever takes
its child gate. Concurrency therefore only ever expands under one running call.
Calling `dispatch_all` from a child context is rejected with a typed error, so
orchestrators inside a concurrent set stay serial through `dispatch`. Sibling
dispatch attempts are capped by `max_child_calls` (default `32`, counting every
attempt in a `dispatch_all` batch upfront); nesting is capped by `max_tool_depth`
(default `8`). Resource-claimed tools must not dispatch children; use an
exclusive orchestration tool instead.

`max_tool_calls` (default `1024`) caps admitted calls across the whole run,
including nested, denied, responded, unknown, and hidden calls. Budget/depth errors returned
by `dispatch` and `dispatch_all` remain typed errors for the parent to handle.
The final advertised declaration set of each inference attempt also gates every
model and child call for that step. Registration alone does not grant visibility.

`tool_timeout_ms` defaults to no timeout and covers the executor future, including
time it spends dispatching children, but not its outer authorization/result hooks.
Timeouts terminate the run with `ToolTimeout`; cancellation, stop, or a fatal
pipeline error drops sibling futures and prevents later waves from executing.
Denial only affects the denied call. At the top level, completed results are
retained and unfinished calls have explicit error results; inside `dispatch_all`,
a fatal child error instead drops its in-flight siblings, skips later child
waves, and returns the typed error to the parent. Neither timeout nor
cancellation guarantees that external effects stopped or were rolled back, and
neither retries a tool.
Detached work inside tools is outside this guarantee and must not be used.
Workspace modes, snapshots, permissions UI, and path policy remain in consumers.

Tool output uses `ToolContents`, an ordered collection of `ToolContent::Text`
and `ToolContent::Image { source: ImageSource }`. Conversation blocks such as
calls, results, and reasoning cannot be nested inside output. `String` and
`ToolOutput` implement `IntoToolOutput`; custom native outputs can implement
that trait without implementing Serde:

```rust
use animist::{
    agent::tool::{ToolContent, ToolOutput},
    history::ImageSource,
};

let output = ToolOutput::builder()
    .content(vec![
        ToolContent::Text { text: "Screenshot".into() },
        ToolContent::Image {
            source: ImageSource::from_bytes("image/png", &[1, 2, 3]),
        },
    ])
    .build();
```

Text-only builders still accept `.content("text")` or `.content(String)`.
`ToolContents` wraps a public `Vec<ToolContent>` in `.0`; there is no implicit
lossy text conversion. JSON/structured output is not a separate capability:
tools choose how to render it as text, and no automatic serialization is required.

`is_error`, content order, call identity, and `loaded_tools` survive events,
result revisions, and persisted history. Activation metadata does not activate
or register executors; all current protocol adapters reject nonempty
`loaded_tools` rather than advertise unsupported deferred-tool behavior.
Anthropic Messages and OpenAI Responses support tool-result images; Chat
Completions rejects them. UI presentation belongs in the consumer's event
adapter or message display metadata, not in model-visible content.

## Execution, history, and cancellation

A run records the transformed input, then repeatedly prepares a request, streams
an assembled provider response, and dispatches its complete tool calls. It never
executes partial streamed arguments. Model-step and total retry budgets are
independent. Provider errors propagate unless a layer requests recovery.

`MaxTokens`, `Paused`, `Refused`, and `ContextWindowExceeded` are distinct terminal
statuses, not successful completions or implicit continuation requests. Tool calls
in these responses are closed with not-executed results. The minimal runtime does
not synthesize continuation prompts; a layer may continue a run explicitly from
`Completed`, `MaxTokens`, or `Paused` through `step_end` (see *Step boundaries*),
never from `Refused` or `ContextWindowExceeded`.

Before executing tools, the runtime atomically records the assistant message and
explicit interrupted/not-completed results for every call. Each finished result
revises that stored result message. Cancellation, failure, or dropping the run
therefore cannot leave unmatched calls. Original placeholders and subsequent
revisions remain available in history snapshots.

Use the `Cancellation` in `RunOptions` and await the run to finalize cooperative
cancellation. Dropping the run drops owned inference/tool futures but cannot undo
external side effects; pending result text explicitly records this uncertainty.
The agent uses an owned `InferenceExchange`. An incomplete inference response
is not committed to history, even when it contains closed signed reasoning or
complete tool calls. Its display deltas and protocol-validated content prefix
are retained separately in `InterruptedAttempt::partial`. No incomplete attempt
authorizes tool execution; the existing pending-result rule for already-started
tools is unchanged.

Cooperative cancellation returns `RunReport::interrupted_attempts`. Recovery and
finalization hooks can inspect `RunContext::interrupted_attempts()`. An inference
failure returns `AgentError::Inference { source, attempt }`, preserving the
original `ProviderError` alongside the last interrupted attempt. This replaces
the agent's former `AgentError::Provider` path for streaming failures; callers
matching that path must update their recovery/error handling.
Dropping the whole run still cancels owned work, but cannot return a partial
report or run asynchronous finalization. Await cooperative cancellation when
recovery data is needed.

Retention is bounded per attempt by the exchange budget and per run by the
configured step/retry limits. Reports hold shared `Arc` handles to interrupted
attempts rather than duplicate their output.

The agent does not overwrite external history edits. Exchange commits compare
the expected history and use the existing optimistic atomic compaction contract.
Conflicts preserve uncommitted messages in the error payload. Do not mutate the
agent's history concurrently with a run unless prepared to handle that conflict.
Use request-only transformations for temporary context, or `step_end` appends
for durable messages; the step-boundary commit spans the same window, so an
edit after the step's last committed result conflicts.

## Events and statistics

Supply an optional `flume::Sender<AgentEvent>` through `RunOptions`. Consume bounded
channels concurrently with the run; connected delivery applies backpressure, while
cancellation releases blocked sends. Disconnected observers do not fail the run.
Cancellation may suppress events, including `Finished`; the returned result is
authoritative. Prefer one channel per run, especially when tool IDs may repeat
across conversations.

Provider events carry run/step/attempt identity. Other events report tool start,
progress, structured failure, completion, step completion, step-boundary message
appends, retries, and final status. These events are runtime observations, not a
durable persistence backend.

Attempt IDs are zero-based and monotonic within a run, across both steps and
retries. `AttemptInterrupted` identifies abandoned output; `Retrying::attempt`
identifies the next inference attempt, not a separate retry counter. Previously
visible text is not retracted or concatenated into the next response. Consumers
must keep attempts separate and explicitly mark abandoned text as superseded
when a retry succeeds. Checkpoint events are not new display deltas.

`RunStats` uses atomic counters for completed model steps, inference attempts,
executed tool calls, and the latest reported input size. Usage/cost aggregation
uses a short async lock and records the actual request model before response
layers run. `StatsSnapshot` includes usage by `(provider, model)` and the count of
unpriced reported attempts. Cost is a known subtotal when that count is nonzero,
not a claim that missing prices are zero. Token totals saturate at the existing
`TokenUsage` field width. Context size is the latest reported input count, not a
prediction of the next request or cumulative usage.

Counters and usage are not one atomic transaction; live snapshots may observe
adjacent updates. Final reports provide settled statistics.
Each completed attempt records its final response usage once. Each interrupted
attempt records its latest checkpoint usage once, including zero-token reports;
incremental checkpoints are never added together. `reported_incomplete_attempts`
and `unreported_incomplete_attempts` distinguish reported from unknown usage.
`unpriced_attempts` counts all unpriced reported attempts; `unpriced_steps`
continues counting only unpriced completed steps. Partial attempts do not
increment `steps`. Unknown usage is not zero-cost inference and cannot be
charged locally.

## Follow-ups

Sampling controls are shared through `RunOptions::request` and
`StepRequest::options`; see [sampling controls](providers-and-protocols.md#sampling-controls).
See `followup/` for application-boundary audits and completed task records.
Craft and Revelry migrations and additional provider integrations are outside
this workstream.
