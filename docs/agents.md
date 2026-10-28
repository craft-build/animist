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
| `turn_end` | Observe every admitted run's terminal status |
| `shutdown` | Release initialized resources |

Transformations receive the previous layer's output. Tool denial/stop
short-circuits authorization; later layers cannot re-approve it. Post-tool
transformations still see denied and unknown-tool results. Completion and shutdown
attempt every node even when an earlier node fails. The first cleanup error is
returned; if both the run and its completion fail, both errors are retained.

Request transformations do not rewrite stored history. Use them for volatile
context, budgeted request views, and selecting a fallback model. Tool declarations
must still match the frozen registry. Response transformations intentionally affect
replayed content; preserve opaque reasoning/signatures required by the protocol.
Raw provider deltas are not retrospectively transformed.

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
layers, cancellation, and nesting limit. It does not expose raw implementations.
Child calls are internal execution details; their results are incorporated by the
parent tool rather than inserted as unmatched model tool results.

Unknown tools, argument-decoding failures, and concrete execution failures become
error results the model can correct. `ToolFailed` and `RunReport::tool_failures`
retain original structured errors behind `Arc`s, even without an event observer;
only the model-facing result renders them as text. Layer
errors and explicit stop decisions terminate the run. Tool implementations are
never retried automatically. Execution is sequential by default and currently the
only scheduling mode; conflict-aware parallel execution is a follow-up.

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
not synthesize continuation prompts.

Before executing tools, the runtime atomically records the assistant message and
explicit interrupted/not-completed results for every call. Each finished result
revises that stored result message. Cancellation, failure, or dropping the run
therefore cannot leave unmatched calls. Original placeholders and subsequent
revisions remain available in history snapshots.

Use the `Cancellation` in `RunOptions` and await the run to finalize cooperative
cancellation. Dropping the run drops owned inference/tool futures but cannot undo
external side effects; pending result text explicitly records this uncertainty.
Providers currently cannot return replay-safe partial signed reasoning, so an
incomplete inference response is not committed to history. Its already-emitted
display deltas remain available.

The agent does not overwrite external history edits. Exchange commits compare
the expected history and use the existing optimistic atomic compaction contract.
Conflicts preserve uncommitted messages in the error payload. Do not mutate the
agent's history concurrently with a run unless prepared to handle that conflict.
Use request-only transformations for temporary context.

## Events and statistics

Supply an optional `flume::Sender<AgentEvent>` through `RunOptions`. Consume bounded
channels concurrently with the run; connected delivery applies backpressure, while
cancellation releases blocked sends. Disconnected observers do not fail the run.
Cancellation may suppress events, including `Finished`; the returned result is
authoritative. Prefer one channel per run, especially when tool IDs may repeat
across conversations.

Provider events carry run/step/attempt identity. Other events report tool start,
progress, structured failure, completion, step completion, retries, and final
status. These events are runtime observations, not a durable persistence backend.

`RunStats` uses atomic counters for completed model steps, inference attempts,
executed tool calls, and the latest reported input size. Usage/cost aggregation
uses a short async lock and records the actual request model before response
layers run. `StatsSnapshot` includes usage by `(provider, model)` and the count of
unpriced completed steps. Cost is a known subtotal when that count is nonzero,
not a claim that missing prices are zero. Token totals saturate at the existing
`TokenUsage` field width. Context size is the latest reported input count, not a
prediction of the next request or cumulative usage.

Counters and usage are not one atomic transaction; live snapshots may observe
adjacent updates. Final reports provide settled statistics. Failed attempts
without a provider usage report cannot be charged locally.

## Follow-ups

See `followup/` for executable task prompts covering sampling,
partial replay, replay validation, scheduling, and application-boundary audits.
Craft and Revelry migrations and additional provider integrations are outside
this workstream.
