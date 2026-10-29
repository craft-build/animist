#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use animist::{
    agent::{
        Agent, AgentConfig, AgentError, AgentEvent, AgentLayer, Cancellation, Layer, LayerImpl,
        Recovery, RunContext, RunOptions, RunStatus, Section, StepAction, StepOutcome, StepRequest,
        tool::{
            IntoToolOutput, ToolCall, ToolContent, ToolContext, ToolDefinition, ToolError,
            ToolImpl, ToolOutput, ToolRegistry,
        },
    },
    history::{ContentBlock, Message, MessageKind, Role, StoredMessage},
    provider::{
        Model, ModelInfo, Provider, ProviderEvent, ProviderImpl, RequestOptions, StopReason,
        StreamResponse, TokenUsage, Tool,
    },
};
use async_trait::async_trait;
use flume::Sender;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

type Reply = animist::provider::Result<StreamResponse>;

struct Script {
    replies: Mutex<VecDeque<Reply>>,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    hanging: bool,
    checkpoints: Vec<ProviderEvent>,
}

#[async_trait]
impl ProviderImpl for Script {
    fn name(&self) -> &'static str {
        "script"
    }

    async fn stream_message(
        &self,
        _model: &Model,
        messages: &[Message],
        _system: &str,
        _tools: &[Tool],
        events: &Sender<ProviderEvent>,
        _options: RequestOptions,
    ) -> Reply {
        self.requests.lock().await.push(messages.to_vec());
        events
            .send_async(ProviderEvent::TextDelta {
                text: "delta".into(),
            })
            .await
            .unwrap();
        for event in &self.checkpoints {
            events.send_async(event.clone()).await.unwrap();
        }
        if self.hanging {
            std::future::pending::<()>().await;
        }
        self.replies
            .lock()
            .await
            .pop_front()
            .expect("script exhausted")
    }

    async fn list_models(&self) -> animist::provider::Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }
    async fn refresh_auth(&self) -> animist::provider::Result<()> {
        Ok(())
    }
    async fn reload_auth(&self) -> animist::provider::Result<()> {
        Ok(())
    }
}

fn message(role: Role, content: Vec<ContentBlock>) -> Message {
    Message::builder()
        .role(role)
        .content(content)
        .kind(MessageKind::Turn)
        .build()
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

fn input() -> StoredMessage {
    StoredMessage::builder()
        .timestamp(jiff::Timestamp::now())
        .message(message(Role::User, vec![text("hello")]))
        .build()
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Reply keeps scripted provider failures uniform"
)]
fn response(content: Vec<ContentBlock>, stop: StopReason) -> Reply {
    Ok(StreamResponse::builder()
        .message(message(Role::Assistant, content))
        .usage(
            TokenUsage::builder()
                .input(10_u32)
                .output(2_u32)
                .cache_creation(0_u32)
                .cache_read(0_u32)
                .build(),
        )
        .stop_reason(stop)
        .input_transformations(Vec::new())
        .build())
}

fn call(id: &str, name: &str, input: serde_json::Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
        thought_signature: None,
    }
}

fn model(replies: Vec<Reply>, hanging: bool, requests: Arc<Mutex<Vec<Vec<Message>>>>) -> Model {
    Model::builder()
        .id("script")
        .provider(Arc::new(Provider::new(Script {
            replies: Mutex::new(replies.into()),
            requests,
            hanging,
            checkpoints: Vec::new(),
        })))
        .info(ModelInfo::default())
        .build()
}

fn agent(replies: Vec<Reply>) -> Agent {
    Agent::builder()
        .model(model(replies, false, Arc::default()))
        .build()
}

#[derive(Deserialize)]
struct EchoArgs {
    text: String,
}

struct Echo(Arc<AtomicUsize>);

struct Screenshot;

struct ScreenshotOutput {
    label: String,
    source: animist::history::ImageSource,
}

impl IntoToolOutput for ScreenshotOutput {
    fn into_tool_output(self) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::builder()
            .content(vec![
                ToolContent::Text { text: self.label },
                ToolContent::Image {
                    source: self.source,
                },
            ])
            .is_error(true)
            .loaded_tools(vec!["deferred".into()])
            .build())
    }
}

#[async_trait]
impl ToolImpl for Screenshot {
    type Args = serde_json::Value;
    type Output = ScreenshotOutput;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("screenshot")
            .description("Capture an image")
            .input_schema(json!({"type":"object"}))
            .build()
    }

    async fn call(
        &self,
        _context: &ToolContext<'_>,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        Ok(ScreenshotOutput {
            label: "Partial capture".into(),
            source: animist::history::ImageSource::from_bytes("image/png", &[1, 2, 3]),
        })
    }
}

#[async_trait]
impl ToolImpl for Echo {
    type Args = EchoArgs;
    type Output = String;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder().name("echo").description("Echo")
            .input_schema(json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})).build()
    }

    async fn call(&self, _context: &ToolContext<'_>, args: EchoArgs) -> Result<String, Infallible> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(args.text)
    }
}

#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent per-test toggles, not a state machine"
)]
struct Trace {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
    deny: bool,
    respond: bool,
    retry: bool,
    fail_end: bool,
}

#[async_trait]
impl LayerImpl for Trace {
    async fn startup(&self, _agent: &Agent) -> animist::agent::Result<()> {
        self.log.lock().await.push(format!("{}:startup", self.name));
        Ok(())
    }
    async fn prepare_request(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        mut request: StepRequest,
    ) -> animist::agent::Result<StepRequest> {
        self.log.lock().await.push(format!("{}:prepare", self.name));
        request.system.push_str(self.name);
        Ok(request)
    }
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
    ) -> animist::agent::Result<animist::agent::ToolDecision> {
        self.log.lock().await.push(format!("{}:pre", self.name));
        Ok(if self.deny {
            animist::agent::ToolDecision::Deny {
                reason: "denied".into(),
            }
        } else if self.respond {
            animist::agent::ToolDecision::Respond {
                output: ToolOutput::builder().content("responded").build(),
            }
        } else {
            animist::agent::ToolDecision::Run
        })
    }
    async fn post_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
        output: ToolOutput,
    ) -> animist::agent::Result<ToolOutput> {
        self.log.lock().await.push(format!("{}:post", self.name));
        Ok(output)
    }
    async fn recover(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _error: &AgentError,
    ) -> animist::agent::Result<Recovery> {
        Ok(if self.retry {
            Recovery::Retry
        } else {
            Recovery::Propagate
        })
    }
    async fn turn_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        status: RunStatus,
    ) -> animist::agent::Result<()> {
        self.log
            .lock()
            .await
            .push(format!("{}:end:{status:?}", self.name));
        if self.fail_end {
            Err(AgentError::Stopped {
                reason: "end failed".into(),
                backtrace: std::backtrace::Backtrace::capture(),
            })
        } else {
            Ok(())
        }
    }
    async fn shutdown(&self, _agent: &Agent) -> animist::agent::Result<()> {
        self.log
            .lock()
            .await
            .push(format!("{}:shutdown", self.name));
        Ok(())
    }
}

fn trace(name: &'static str, log: Arc<Mutex<Vec<String>>>) -> Trace {
    Trace {
        name,
        log,
        deny: false,
        respond: false,
        retry: false,
        fail_end: false,
    }
}

fn layers(first: Trace, second: Trace) -> AgentLayer {
    AgentLayer::builder()
        .layer(Layer::new(first))
        .next(Box::new(
            AgentLayer::builder().layer(Layer::new(second)).build(),
        ))
        .build()
}

struct CaptureRequestOptions(Arc<Mutex<Vec<RequestOptions>>>);

#[async_trait]
impl LayerImpl for CaptureRequestOptions {
    async fn prepare_request(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        request: StepRequest,
    ) -> animist::agent::Result<StepRequest> {
        self.0.lock().await.push(request.options.clone());
        Ok(request)
    }
}

#[tokio::test]
async fn run_sampling_controls_reach_request_preparation_on_every_step() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::default();
    registry
        .register(Echo(Arc::new(AtomicUsize::new(0))))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("echo-1", "echo", json!({"text":"hello"}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(registry)
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(CaptureRequestOptions(std::sync::Arc::clone(
                    &captured,
                ))))
                .build(),
        )
        .build();
    let options = RequestOptions::builder()
        .temperature(0.25)
        .top_p(0.9)
        .frequency_penalty(-0.5)
        .presence_penalty(0.5)
        .stop_sequences(vec!["END".into()])
        .max_output_tokens(128_u32)
        .build();
    let report = agent
        .run(
            input(),
            RunOptions::builder().request(options.clone()).build(),
        )
        .await
        .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(*captured.lock().await, vec![options.clone(), options]);
}

#[tokio::test]
async fn external_api_streams_without_tools_and_preserves_metadata() {
    let agent = agent(vec![response(vec![text("done")], StopReason::EndTurn)]);
    let version = agent.history().snapshot().await.generation();
    let (tx, rx) = flume::bounded(16);
    let mut input = input();
    input.tags_mut().insert("test".into(), "retained".into());
    let report = agent
        .run(input.clone(), RunOptions::builder().events(tx).build())
        .await
        .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.stats.steps, 1);
    assert_eq!(report.stats.attempts, 1);
    assert_eq!(report.stats.context_size, 10);
    assert_eq!(report.stats.unpriced_steps, 1);
    assert_eq!(
        agent.history().snapshot().await.entries()[0].latest(),
        &input
    );
    let changes = agent.history().changes_since(&version).await.unwrap();
    let snapshot = agent.history().snapshot().await;
    assert_eq!(changes.generation(), snapshot.generation());
    assert_eq!(
        changes.changes(),
        &[
            animist::history::HistoryChange::Appended {
                index: 0,
                entry: snapshot.entries()[0].clone(),
            },
            animist::history::HistoryChange::Appended {
                index: 1,
                entry: snapshot.entries()[1].clone(),
            },
        ]
    );
    let events: Vec<_> = rx.try_iter().collect();
    assert!(matches!(events.first(), Some(AgentEvent::Provider { .. })));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished {
            status: RunStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn typed_rich_output_survives_events_history_revisions_and_next_request() {
    let mut tools = ToolRegistry::default();
    tools.register(Screenshot).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("rich", "screenshot", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            std::sync::Arc::clone(&requests),
        ))
        .tools(tools)
        .build();
    let (tx, rx) = flume::unbounded();
    let report = agent
        .run(input(), RunOptions::builder().events(tx).build())
        .await
        .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.stats.tool_calls, 1);
    let output = rx
        .try_iter()
        .find_map(|event| match event {
            AgentEvent::ToolFinished { call, output } => {
                assert_eq!(call.id, "rich");
                Some(output)
            }
            _ => None,
        })
        .unwrap();
    assert!(matches!(output.content.0.as_slice(), [
        ToolContent::Text { text },
        ToolContent::Image { .. }
    ] if text == "Partial capture"));
    let expected = ContentBlock::ToolResult {
        tool_use_id: "rich".into(),
        content: output.content,
        is_error: output.is_error,
        loaded_tools: output.loaded_tools,
    };
    assert!(matches!(&expected, ContentBlock::ToolResult {
        is_error: true, loaded_tools, ..
    } if loaded_tools == &["deferred"]));
    assert_eq!(requests.lock().await[1][2].content, vec![expected.clone()]);
    let snapshot = agent.history().snapshot().await;
    let result = &snapshot.entries()[2];
    assert_eq!(result.revisions().len(), 1);
    assert_eq!(result.latest().message().content, vec![expected.clone()]);
    assert!(matches!(&result.verbatim().message().content[0],
        ContentBlock::ToolResult { is_error: true, content, .. }
        if matches!(content.0.as_slice(), [ToolContent::Text { text }] if text.contains("interrupted"))
    ));
    let entries = serde_json::from_slice(&serde_json::to_vec(snapshot.entries()).unwrap()).unwrap();
    let restored = animist::history::History::from_entries(entries);
    assert_eq!(restored.current().await[2].content, vec![expected]);
}

#[tokio::test]
async fn typed_tools_run_and_request_is_prepared_again() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("1", "echo", json!({"text":"tool output"}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            std::sync::Arc::clone(&requests),
        ))
        .tools(tools)
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert_eq!(report.stats.steps, 2);
    assert_eq!(report.stats.tool_calls, 1);
    assert_eq!(report.stats.usage.input, 20);
    assert_eq!(requests.lock().await[1].len(), 3);
    assert!(
        matches!(&requests.lock().await[1][2].content[0], ContentBlock::ToolResult { content, is_error: false, .. } if content == &"tool output".into())
    );
}

struct ReplayView {
    corrupt: bool,
}

#[async_trait]
impl LayerImpl for ReplayView {
    async fn prepare_request(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        mut request: StepRequest,
    ) -> animist::agent::Result<StepRequest> {
        if self.corrupt {
            request.messages.push(message(
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: "orphan-from-layer".into(),
                    content: "invalid".into(),
                    is_error: false,
                    loaded_tools: vec![],
                }],
            ));
        } else {
            request.messages.insert(
                1,
                Message::builder()
                    .role(Role::User)
                    .kind(MessageKind::Observation)
                    .content(vec![text("ephemeral context")])
                    .build(),
            );
        }
        Ok(request)
    }
}

#[tokio::test]
async fn restored_history_and_layer_views_are_checked_before_custom_provider_inference() {
    use animist::{
        history::{History, HistoryEntry},
        protocol::{ReplayError, ReplayLocation},
    };
    let exchange = vec![
        message(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    thinking: "private".into(),
                    signature: Some("opaque-signature".into()),
                },
                call("old", "removed-tool", json!({})),
            ],
        ),
        message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "old".into(),
                content: vec![ToolContent::Image {
                    source: animist::history::ImageSource::from_bytes("image/png", &[1]),
                }]
                .into(),
                is_error: true,
                loaded_tools: vec![],
            }],
        ),
    ];
    for corrupt in [false, true] {
        let history = History::new();
        for message in &exchange {
            history.add(message.clone()).await;
        }
        let entries: Vec<HistoryEntry> = serde_json::from_slice(
            &serde_json::to_vec(history.snapshot().await.entries()).unwrap(),
        )
        .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::builder()
            .model(model(
                vec![response(vec![text("done")], StopReason::EndTurn)],
                false,
                std::sync::Arc::clone(&requests),
            ))
            .history(History::from_entries(entries))
            .layers(
                AgentLayer::builder()
                    .layer(Layer::new(ReplayView { corrupt }))
                    .build(),
            )
            .build();
        let outcome = agent.run(input(), RunOptions::default()).await;
        if corrupt {
            assert!(matches!(outcome, Err(AgentError::Replay {
                source: ReplayError::OrphanResult { id, location ,
                ..}
            }) if id == "orphan-from-layer" && location == ReplayLocation { message: 3, block: 0 }));
            assert!(requests.lock().await.is_empty());
        } else {
            assert_eq!(outcome.unwrap().status, RunStatus::Completed);
            let requests = requests.lock().await;
            assert_eq!(requests[0][0], exchange[0]);
            assert_eq!(requests[0][1].kind, MessageKind::Observation);
            assert_eq!(requests[0][2], exchange[1]);
        }
        let durable = agent.history().current().await;
        assert_eq!(&durable[..2], exchange.as_slice());
        assert!(
            !serde_json::to_string(&durable)
                .unwrap()
                .contains("ephemeral context")
        );
    }
}

#[tokio::test]
async fn restored_unresolved_calls_fail_without_inference_or_implicit_repair() {
    use animist::{
        history::{History, HistoryEntry},
        protocol::ReplayError,
    };
    let source = StoredMessage::builder()
        .timestamp(jiff::Timestamp::now())
        .message(message(
            Role::Assistant,
            vec![call("unresolved", "removed", json!({}))],
        ))
        .build();
    let saved = serde_json::to_vec(&vec![
        HistoryEntry::builder().source(source.clone()).build(),
    ])
    .unwrap();
    let history = History::from_entries(serde_json::from_slice(&saved).unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(vec![], false, std::sync::Arc::clone(&requests)))
        .history(history)
        .build();
    assert!(matches!(agent.run(input(), RunOptions::default()).await,
        Err(AgentError::Replay { source: ReplayError::MissingResult { id, .. } }) if id == "unresolved"));
    assert!(requests.lock().await.is_empty());
    assert_eq!(
        agent.history().snapshot().await.entries()[0].latest(),
        &source
    );
}

#[tokio::test]
async fn linked_layers_forward_turn_end_and_short_circuit_denial() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut first = trace("a", std::sync::Arc::clone(&log));
    first.deny = true;
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("1", "echo", json!({"text":"never"}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(layers(first, trace("b", std::sync::Arc::clone(&log))))
        .build();
    agent.run(input(), RunOptions::default()).await.unwrap();
    agent.shutdown().await.unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 0);
    let log = log.lock().await;
    assert!(!log.iter().any(|entry| entry == "b:pre"));
    assert!(
        log.windows(2)
            .any(|entries| entries == ["a:end:Completed", "b:end:Completed"])
    );
    assert!(log.ends_with(&["a:shutdown".into(), "b:shutdown".into()]));
    assert!(matches!(
        agent.run(input(), RunOptions::default()).await,
        Err(AgentError::ShutDown { .. })
    ));
}

#[tokio::test]
async fn a_responded_call_skips_the_executor_and_later_pre_hooks() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut first = trace("a", std::sync::Arc::clone(&log));
    first.respond = true;
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let (tx, rx) = flume::unbounded();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("1", "echo", json!({"text":"never"}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(layers(first, trace("b", std::sync::Arc::clone(&log))))
        .build();
    let report = agent
        .run(input(), RunOptions::builder().events(tx).build())
        .await
        .unwrap();
    agent.shutdown().await.unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert_eq!(report.stats.tool_calls, 0);
    let log = log.lock().await;
    assert!(log.contains(&"a:pre".to_owned()));
    assert!(!log.iter().any(|entry| entry == "b:pre"));
    assert!(
        log.windows(2)
            .any(|entries| entries == ["a:post", "b:post"])
    );
    drop(log);
    let events: Vec<_> = rx.try_iter().collect();
    let started = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
        .count();
    assert_eq!(started, 0);
    let finished = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolFinished { call, output } => Some((call.id.as_str(), output)),
            _ => None,
        })
        .unwrap();
    assert_eq!(finished.0, "1");
    assert!(matches!(
        finished.1.content.0.as_slice(),
        [ToolContent::Text { text }] if text == "responded"
    ));
    assert!(!finished.1.is_error);
    assert!(matches!(
        &agent.history().current().await[2].content[0],
        ContentBlock::ToolResult { content, is_error: false, .. } if content == &"responded".into()
    ));
}

struct RespondId(&'static str);

#[async_trait]
impl LayerImpl for RespondId {
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        context: &ToolContext<'_>,
    ) -> animist::agent::Result<animist::agent::ToolDecision> {
        Ok(if context.call.id == self.0 {
            animist::agent::ToolDecision::Respond {
                output: ToolOutput::builder().content("served").build(),
            }
        } else {
            animist::agent::ToolDecision::Run
        })
    }
}

#[tokio::test]
async fn responded_outputs_land_in_call_order_without_is_error() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let (tx, rx) = flume::unbounded();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![
                        call("1", "echo", json!({"text":"first"})),
                        call("2", "echo", json!({"text":"second"})),
                    ],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(RespondId("1")))
                .build(),
        )
        .build();
    let report = agent
        .run(input(), RunOptions::builder().events(tx).build())
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert_eq!(report.stats.tool_calls, 1);
    let history = agent.history().current().await;
    assert!(matches!(
        &history[2].content[0],
        ContentBlock::ToolResult { tool_use_id, content, is_error: false, .. }
            if tool_use_id == "1" && content == &"served".into()
    ));
    assert!(matches!(
        &history[2].content[1],
        ContentBlock::ToolResult { tool_use_id, content, is_error: false, .. }
            if tool_use_id == "2" && content == &"second".into()
    ));
    let events: Vec<_> = rx.try_iter().collect();
    let mut started = events.iter().filter_map(|event| match event {
        AgentEvent::ToolStarted { call, .. } => Some(call.id.as_str()),
        _ => None,
    });
    assert_eq!(started.next(), Some("2"));
    assert_eq!(started.next(), None);
    let finished: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished { call, output } => Some((
                call.id.as_str(),
                matches!(
                    output.content.0.as_slice(),
                    [ToolContent::Text { text }] if text == "served" || text == "second"
                ),
                output.is_error,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 2);
    assert!(
        finished
            .iter()
            .all(|(_, content, is_error)| *content && !*is_error)
    );
    assert!(finished.iter().map(|(id, _, _)| *id).eq(["1", "2"]));
}

struct RewriteResponded;

#[async_trait]
impl LayerImpl for RewriteResponded {
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
    ) -> animist::agent::Result<animist::agent::ToolDecision> {
        Ok(animist::agent::ToolDecision::Respond {
            output: ToolOutput::builder().content("raw").build(),
        })
    }

    async fn post_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
        _output: ToolOutput,
    ) -> animist::agent::Result<ToolOutput> {
        Ok(ToolOutput::builder().content("rewritten").build())
    }
}

#[tokio::test]
async fn post_tool_exec_transforms_a_responded_output() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("1", "echo", json!({"text":"never"}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(RewriteResponded))
                .build(),
        )
        .build();
    agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert!(matches!(
        &agent.history().current().await[2].content[0],
        ContentBlock::ToolResult { content, is_error: false, .. } if content == &"rewritten".into()
    ));
}

#[tokio::test]
async fn responded_calls_are_admitted_but_not_counted_as_executed() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let replies = || {
        vec![
            response(
                vec![
                    call("1", "echo", json!({"text":"a"})),
                    call("2", "echo", json!({"text":"b"})),
                ],
                StopReason::ToolUse,
            ),
            response(vec![text("done")], StopReason::EndTurn),
        ]
    };
    let within_budget = Agent::builder()
        .model(model(replies(), false, Arc::default()))
        .tools(tools.clone())
        .config(AgentConfig::builder().max_tool_calls(2_u64).build())
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(RespondId("1")))
                .build(),
        )
        .build();
    let report = within_budget
        .run(input(), RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.stats.tool_calls, 1);
    assert_eq!(count.load(Ordering::Relaxed), 1);
    let over_budget = Agent::builder()
        .model(model(replies(), false, Arc::default()))
        .tools(tools)
        .config(AgentConfig::builder().max_tool_calls(1_u64).build())
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(RespondId("1")))
                .build(),
        )
        .build();
    assert!(matches!(
        over_budget.run(input(), RunOptions::default()).await,
        Err(AgentError::ToolCallLimitExceeded { .. })
    ));
    assert_eq!(count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn unknown_and_invalid_tools_become_results_not_run_failures() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![
                        call("1", "missing", json!({})),
                        call("2", "echo", json!({"text":42})),
                    ],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .build();
    agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 0);
    let history = agent.history().current().await;
    assert!(
        history[2]
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { is_error: true, .. }))
    );
}

#[tokio::test]
async fn cancellation_stops_owned_provider_and_run_gate_rejects_overlap() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with_hanging_provider(requests);
    let cancellation = Cancellation::default();
    let (tx, rx) = flume::bounded(1);
    let running = agent.run(
        input(),
        RunOptions::builder()
            .cancellation(cancellation.clone())
            .events(tx)
            .build(),
    );
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => panic!("unexpected result: {result:?}"),
        _ = rx.recv_async() => {}
    }
    assert!(matches!(
        agent.run(input(), RunOptions::default()).await,
        Err(AgentError::AlreadyRunning { .. })
    ));
    cancellation.cancel();
    let report = running.await.unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(agent.history().current().await.len(), 1);
}

fn agent_with_hanging_provider(requests: Arc<Mutex<Vec<Vec<Message>>>>) -> Agent {
    Agent::builder()
        .model(model(vec![], true, requests))
        .build()
}

#[tokio::test]
async fn retries_are_bounded_and_reprepare_requests() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut retry = trace("retry", std::sync::Arc::clone(&log));
    retry.retry = true;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![
                Err(animist::provider::ProviderError::Http {
                    status: 503,
                    body: "busy".into(),
                    body_truncated: false,
                    backtrace: std::backtrace::Backtrace::capture(),
                }),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            std::sync::Arc::clone(&requests),
        ))
        .config(AgentConfig::builder().max_retries(1_u32).build())
        .layers(AgentLayer::builder().layer(Layer::new(retry)).build())
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.stats.attempts, 2);
    assert_eq!(report.stats.steps, 1);
    assert_eq!(report.stats.unreported_incomplete_attempts, 1);
    assert_eq!(report.interrupted_attempts[0].partial.display_text, "delta");
    assert!(report.interrupted_attempts[0].partial.usage.is_none());
    assert_eq!(requests.lock().await.len(), 2);
    assert_eq!(
        log.lock()
            .await
            .iter()
            .filter(|entry| *entry == "retry:prepare")
            .count(),
        2
    );
}

#[tokio::test]
async fn step_limit_preserves_complete_exchange() {
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("1", "missing", json!({}))],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .config(AgentConfig::builder().max_steps(1_u32).build())
        .build();
    assert_eq!(
        agent
            .run(input(), RunOptions::default())
            .await
            .unwrap()
            .status,
        RunStatus::StepLimit
    );
    assert_eq!(agent.history().current().await.len(), 3);
}

#[tokio::test]
async fn terminal_stops_never_execute_tools() {
    for (reason, status) in [
        (StopReason::MaxTokens, RunStatus::MaxTokens),
        (StopReason::PauseTurn, RunStatus::Paused),
        (StopReason::Refusal, RunStatus::Refused),
        (
            StopReason::ContextWindowExceeded,
            RunStatus::ContextWindowExceeded,
        ),
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::default();
        tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
        let agent = Agent::builder()
            .model(model(
                vec![response(
                    vec![call("1", "echo", json!({"text":"never"}))],
                    reason,
                )],
                false,
                Arc::default(),
            ))
            .tools(tools)
            .build();
        assert_eq!(
            agent
                .run(input(), RunOptions::default())
                .await
                .unwrap()
                .status,
            status
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn finalization_attempts_every_layer_on_failure() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut first = trace("a", std::sync::Arc::clone(&log));
    first.fail_end = true;
    let agent = Agent::builder()
        .model(model(
            vec![Err(animist::provider::ProviderError::Http {
                status: 500,
                body: "failed".into(),
                body_truncated: false,
                backtrace: std::backtrace::Backtrace::capture(),
            })],
            false,
            Arc::default(),
        ))
        .layers(layers(first, trace("b", std::sync::Arc::clone(&log))))
        .build();
    assert!(matches!(
        agent.run(input(), RunOptions::default()).await,
        Err(AgentError::Finalization { .. })
    ));
    assert!(
        log.lock()
            .await
            .ends_with(&["a:end:Failed".into(), "b:end:Failed".into()])
    );
}

#[tokio::test]
async fn duplicate_ids_fail_before_tools_execute() {
    let agent = agent(vec![response(
        vec![
            call("1", "missing", json!({})),
            call("1", "missing", json!({})),
        ],
        StopReason::ToolUse,
    )]);
    assert!(matches!(
        agent.run(input(), RunOptions::default()).await,
        Err(AgentError::InvalidToolCallId { .. })
    ));
    assert_eq!(agent.history().current().await.len(), 1);
}

#[test]
fn config_defaults_registration_and_prompt_rendering() {
    let config: AgentConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(config.max_steps, AgentConfig::builder().build().max_steps);
    let mut registry = ToolRegistry::default();
    registry.register(Echo(Arc::default())).unwrap();
    assert!(matches!(
        registry.register(Echo(Arc::default())),
        Err(AgentError::DuplicateTool { .. })
    ));
    let section = Section::builder()
        .name("not a tag\"")
        .title("a & b")
        .summary("<summary>")
        .rules(vec!["x > y".into()])
        .build();
    assert!(
        section
            .to_xml()
            .contains("<section name=\"not a tag&quot;\">")
    );
    assert!(section.to_xml().contains("a &amp; b"));
}

struct Child;

#[async_trait]
impl ToolImpl for Child {
    type Args = serde_json::Value;
    type Output = ToolOutput;
    type Error = AgentError;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("child")
            .description("Dispatch a child")
            .input_schema(json!({"type":"object"}))
            .build()
    }

    async fn call(
        &self,
        context: &ToolContext<'_>,
        _args: Self::Args,
    ) -> Result<ToolOutput, AgentError> {
        context
            .dispatch(
                ToolCall::builder()
                    .id("nested")
                    .name("echo")
                    .input(json!({"text":"nested"}))
                    .build(),
            )
            .await
    }
}

struct DenyEcho;

#[async_trait]
impl LayerImpl for DenyEcho {
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        context: &ToolContext<'_>,
    ) -> animist::agent::Result<animist::agent::ToolDecision> {
        Ok(if context.call.name == "echo" {
            assert_eq!(context.parent_call_id, Some("parent"));
            animist::agent::ToolDecision::Deny {
                reason: "nested denial".into(),
            }
        } else {
            animist::agent::ToolDecision::Run
        })
    }
}

#[tokio::test]
async fn child_tools_cannot_bypass_authorization() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Child).unwrap();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "child", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(AgentLayer::builder().layer(Layer::new(DenyEcho)).build())
        .build();
    agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert!(
        matches!(&agent.history().current().await[2].content[0], ContentBlock::ToolResult { content, is_error: true, .. } if content == &"nested denial".into())
    );
}

struct ScheduledTool {
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    finished: Arc<Mutex<Vec<u64>>>,
    claims: animist::agent::tool::ToolScheduling,
}

struct ActiveTool(Arc<AtomicUsize>);

impl Drop for ActiveTool {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ToolImpl for ScheduledTool {
    type Args = u64;
    type Output = String;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("scheduled")
            .description("Test scheduling")
            .input_schema(json!({"type":"integer"}))
            .build()
    }

    fn scheduling(&self, _: &serde_json::Value) -> animist::agent::tool::ToolScheduling {
        self.claims.clone()
    }

    async fn call(&self, context: &ToolContext<'_>, delay: u64) -> Result<String, Infallible> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _guard = ActiveTool(std::sync::Arc::clone(&self.active));
        self.peak.fetch_max(active, Ordering::SeqCst);
        context.progress("entered").await;
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        self.finished.lock().await.push(delay);
        Ok(delay.to_string())
    }
}

#[tokio::test]
async fn scheduling_bounds_conflicts_and_preserves_model_order() {
    use animist::agent::tool::ToolScheduling;
    for (claims, limit, expected_peak) in [
        (ToolScheduling::default(), 3_usize, 1),
        (
            ToolScheduling::Resources {
                reads: vec![],
                writes: vec!["file".into()],
            },
            3,
            1,
        ),
        (
            ToolScheduling::Resources {
                reads: vec!["file".into()],
                writes: vec![],
            },
            2,
            2,
        ),
        (
            ToolScheduling::Resources {
                reads: vec![],
                writes: vec![],
            },
            1,
            1,
        ),
    ] {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(Mutex::new(Vec::new()));
        let mut tools = ToolRegistry::default();
        tools
            .register(ScheduledTool {
                active: std::sync::Arc::clone(&active),
                peak: std::sync::Arc::clone(&peak),
                finished: std::sync::Arc::clone(&finished),
                claims,
            })
            .unwrap();
        let agent = Agent::builder()
            .model(model(
                vec![
                    response(
                        vec![
                            call("1", "scheduled", json!(30)),
                            call("2", "scheduled", json!(1)),
                            call("3", "scheduled", json!(2)),
                        ],
                        StopReason::ToolUse,
                    ),
                    response(vec![text("done")], StopReason::EndTurn),
                ],
                false,
                Arc::default(),
            ))
            .tools(tools)
            .config(AgentConfig::builder().max_concurrent_tools(limit).build())
            .build();
        agent.run(input(), RunOptions::default()).await.unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), expected_peak);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        if expected_peak == 2 {
            assert_eq!(finished.lock().await[0], 1);
        }
        let history = agent.history().current().await;
        for (index, block) in history[2].content.iter().enumerate() {
            assert!(matches!(block, ContentBlock::ToolResult { tool_use_id, .. }
                if tool_use_id == &(index + 1).to_string()));
        }
        animist::protocol::validate_replay(&history, &[]).unwrap();
    }
}

struct HideEcho;

struct EditDuringTool;

#[async_trait]
impl LayerImpl for EditDuringTool {
    async fn post_tool_exec(
        &self,
        agent: &Agent,
        _: &ToolContext<'_>,
        output: ToolOutput,
    ) -> animist::agent::Result<ToolOutput> {
        agent.history().add_stored(input()).await;
        Ok(output)
    }
}

#[tokio::test]
async fn tool_result_commits_reject_edits_during_execution() {
    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::default())).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![
                    call("1", "echo", json!({"text":"first"})),
                    call("2", "echo", json!({"text":"second"})),
                ],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .tool_scheduling_policy(animist::agent::tool::ToolSchedulingPolicy::new(
            IndependentPolicy,
        ))
        .config(AgentConfig::builder().max_concurrent_tools(2_usize).build())
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(EditDuringTool))
                .build(),
        )
        .build();
    assert!(matches!(
        agent.run(input(), RunOptions::default()).await,
        Err(AgentError::History { .. })
    ));
    let history = agent.history().current().await;
    assert!(history.len() > 3);
    assert!(
        history[2]
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { is_error: true, .. }))
    );
    animist::protocol::validate_replay(&history, &[]).unwrap();
}

#[async_trait]
impl LayerImpl for HideEcho {
    async fn prepare_request(
        &self,
        _: &Agent,
        _: &RunContext,
        mut request: StepRequest,
    ) -> animist::agent::Result<StepRequest> {
        request.tools.retain(|tool| tool.name != "echo");
        Ok(request)
    }
}

#[tokio::test]
async fn hidden_tools_are_unavailable_to_model_and_child_calls() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Child).unwrap();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![
                        call("parent", "child", json!({})),
                        call("direct", "echo", json!({"text":"hidden"})),
                    ],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(AgentLayer::builder().layer(Layer::new(HideEcho)).build())
        .build();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        agent.run(input(), RunOptions::default()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(report.stats.tool_calls, 1);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let history = agent.history().current().await;
    assert!(
        history[2]
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { is_error: true, .. }))
    );
    animist::protocol::validate_replay(&history, &[]).unwrap();
}

#[tokio::test]
async fn nested_dispatch_shares_the_run_budget_without_permit_deadlock() {
    for budget in [1_u64, 2] {
        let count = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::default();
        tools.register(Child).unwrap();
        tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
        let agent = Agent::builder()
            .model(model(
                vec![
                    response(
                        vec![call("parent", "child", json!({}))],
                        StopReason::ToolUse,
                    ),
                    response(vec![text("done")], StopReason::EndTurn),
                ],
                false,
                Arc::default(),
            ))
            .tools(tools)
            .config(AgentConfig::builder().max_tool_calls(budget).build())
            .build();
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            agent.run(input(), RunOptions::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            count.load(Ordering::SeqCst),
            usize::try_from(budget - 1).expect("budget fits usize")
        );
        if budget == 1 {
            assert!(
                matches!(&*report.tool_failures[0].error, ToolError::Execution { source }
                if matches!(source.downcast_ref::<AgentError>(), Some(AgentError::ToolCallLimitExceeded { .. })))
            );
        }
    }
}

#[tokio::test]
async fn ten_call_steps_retain_only_the_placeholder_and_ordered_final_results() {
    use animist::agent::tool::ToolScheduling;
    use animist::history::{History, HistoryChange, HistoryEntry};

    for limit in [1_usize, 10] {
        let original = input();
        let mut first = original.clone();
        first.tags_mut().insert("revision".into(), "first".into());
        let mut second = first.clone();
        second.tags_mut().insert("revision".into(), "second".into());
        let application_entry = HistoryEntry::builder()
            .source(original)
            .revisions(vec![first, second])
            .build();
        let finished = Arc::new(Mutex::new(Vec::new()));
        let mut tools = ToolRegistry::default();
        tools
            .register(ScheduledTool {
                active: Arc::default(),
                peak: Arc::default(),
                finished: Arc::clone(&finished),
                claims: ToolScheduling::Resources {
                    reads: vec![],
                    writes: vec![],
                },
            })
            .unwrap();
        let agent = Agent::builder()
            .model(model(
                vec![
                    response(
                        (1..=10)
                            .rev()
                            .map(|delay| call(&format!("call-{delay}"), "scheduled", json!(delay)))
                            .collect(),
                        StopReason::ToolUse,
                    ),
                    response(vec![text("done")], StopReason::EndTurn),
                ],
                false,
                Arc::default(),
            ))
            .history(History::from_entries(vec![application_entry.clone()]))
            .tools(tools)
            .config(AgentConfig::builder().max_concurrent_tools(limit).build())
            .build();
        let before = agent.history().snapshot().await;
        agent.run(input(), RunOptions::default()).await.unwrap();
        let snapshot = agent.history().snapshot().await;
        let changes = agent
            .history()
            .changes_since(&before.generation())
            .await
            .unwrap();
        assert_eq!(changes.generation(), snapshot.generation());
        let mut replayed = before.entries().to_vec();
        let mut progress_revisions = 0;
        let mut replacements = 0;
        for change in changes.changes() {
            match change {
                HistoryChange::Appended { index, entry } => {
                    assert_eq!(*index, replayed.len());
                    replayed.push(entry.clone());
                }
                HistoryChange::Revised { index, message } => {
                    assert_eq!(*index, 3);
                    let entry = &replayed[*index];
                    let mut revisions = entry.revisions().to_vec();
                    revisions.push(message.clone());
                    replayed[*index] = HistoryEntry::builder()
                        .source(entry.verbatim().clone())
                        .revisions(revisions)
                        .active(entry.is_active())
                        .build();
                    progress_revisions += 1;
                }
                HistoryChange::EntryReplaced { index, entry } => {
                    assert_eq!(*index, 3);
                    assert_eq!(progress_revisions, 10);
                    assert_eq!(replayed[*index].revisions().len(), 10);
                    assert_eq!(entry.revisions().len(), 1);
                    replayed[*index] = entry.clone();
                    replacements += 1;
                }
                other => panic!("unexpected agent history change: {other:?}"),
            }
        }
        assert_eq!(replacements, 1, "concurrency limit {limit}");
        assert_eq!(replayed, snapshot.entries(), "concurrency limit {limit}");
        assert_eq!(snapshot.entries()[0], application_entry);
        let result = &snapshot.entries()[3];
        assert_eq!(result.revisions().len(), 1, "concurrency limit {limit}");
        assert_eq!(result.verbatim().message().content.len(), 10);
        assert!(result.verbatim().message().content.iter().all(|block| {
            matches!(block, ContentBlock::ToolResult { is_error: true, content, .. }
                if matches!(&content.0[..], [ToolContent::Text { text }] if text.contains("interrupted")))
        }));
        let expected: Vec<_> = (1..=10)
            .rev()
            .map(|delay| ContentBlock::ToolResult {
                tool_use_id: format!("call-{delay}"),
                content: delay.to_string().into(),
                is_error: false,
                loaded_tools: Vec::new(),
            })
            .collect();
        assert_eq!(result.latest().message().content, expected);
        assert_eq!(result.latest().timestamp(), result.verbatim().timestamp());
        assert_eq!(result.latest().tags(), result.verbatim().tags());
        assert_eq!(finished.lock().await.len(), 10);
        let encoded = serde_json::to_string(snapshot.entries()).unwrap();
        let restored = History::from_entries(serde_json::from_str(&encoded).unwrap());
        assert_eq!(restored.snapshot().await.entries(), snapshot.entries());
    }
}

#[tokio::test]
async fn application_revisions_on_an_in_flight_result_are_preserved_and_conflict() {
    use animist::history::{
        Alteration, CompactionEdit, CompactionPlan, Compactor, HistorySnapshot,
    };

    struct TagResults;

    #[async_trait]
    impl Alteration for TagResults {
        async fn call(
            &self,
            message: &StoredMessage,
        ) -> Result<Option<StoredMessage>, animist::Error> {
            if !message
                .message()
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
            {
                return Ok(None);
            }
            let mut revision = message.clone();
            revision
                .tags_mut()
                .insert("application".into(), "tag".into());
            Ok(Some(revision))
        }
    }

    struct DuplicateResult;

    #[async_trait]
    impl Compactor for DuplicateResult {
        async fn call(&self, snapshot: &HistorySnapshot) -> Result<CompactionPlan, animist::Error> {
            Ok(CompactionPlan::builder()
                .edits(vec![CompactionEdit::Replace {
                    index: 2,
                    message: snapshot.entries()[2].latest().clone(),
                }])
                .build())
        }
    }

    struct ReviseDuringSecondCall(AtomicUsize);

    #[async_trait]
    impl LayerImpl for ReviseDuringSecondCall {
        async fn pre_tool_exec(
            &self,
            agent: &Agent,
            _: &ToolContext<'_>,
        ) -> animist::agent::Result<animist::agent::ToolDecision> {
            if self.0.fetch_add(1, Ordering::Relaxed) == 1 {
                agent.history().alter(TagResults).await?;
                agent.history().compact(DuplicateResult).await?;
            }
            Ok(animist::agent::ToolDecision::Run)
        }
    }

    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::default())).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![
                    call("first", "echo", json!({"text": "first"})),
                    call("second", "echo", json!({"text": "second"})),
                ],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(ReviseDuringSecondCall(AtomicUsize::new(0))))
                .build(),
        )
        .build();
    assert!(agent.run(input(), RunOptions::default()).await.is_err());
    let snapshot = agent.history().snapshot().await;
    let result = &snapshot.entries()[2];
    assert_eq!(result.revisions().len(), 3);
    assert_eq!(result.revisions()[1].tags()["application"], "tag");
    assert_eq!(result.revisions()[1], result.revisions()[2]);
}

#[tokio::test]
async fn dropping_after_a_partial_completion_keeps_completed_and_interrupted_results() {
    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::default())).unwrap();
    tools.register(HangTool).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![
                    call("first", "echo", json!({"text": "completed"})),
                    call("second", "hang", json!({})),
                ],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .build();
    let (tx, rx) = flume::unbounded();
    let mut running = Box::pin(agent.run(input(), RunOptions::builder().events(tx).build()));
    loop {
        tokio::select! {
            result = &mut running => panic!("unexpected result: {result:?}"),
            event = rx.recv_async() => if matches!(event.unwrap(), AgentEvent::ToolProgress { .. }) {
                break;
            }
        }
    }
    drop(running);
    let snapshot = agent.history().snapshot().await;
    let result = &snapshot.entries()[2];
    assert_eq!(result.revisions().len(), 1);
    assert!(result.verbatim().message().content.iter().all(|block| {
        matches!(block, ContentBlock::ToolResult { is_error: true, content, .. }
            if matches!(&content.0[..], [ToolContent::Text { text }] if text.contains("interrupted")))
    }));
    assert!(matches!(&result.latest().message().content[0],
        ContentBlock::ToolResult { is_error: false, content, .. }
            if content == &"completed".into()));
    assert!(matches!(&result.latest().message().content[1],
        ContentBlock::ToolResult { is_error: true, content, .. }
            if matches!(&content.0[..], [ToolContent::Text { text }] if text.contains("interrupted"))));
    animist::protocol::validate_replay(&agent.history().current().await, &[]).unwrap();
}

#[tokio::test]
async fn parallel_cancellation_and_drop_leave_no_tasks_or_orphan_results() {
    for cancel in [false, true] {
        let active = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::default();
        tools
            .register(ScheduledTool {
                active: std::sync::Arc::clone(&active),
                peak: Arc::default(),
                finished: Arc::default(),
                claims: animist::agent::tool::ToolScheduling::Resources {
                    reads: vec![],
                    writes: vec![],
                },
            })
            .unwrap();
        let agent = Agent::builder()
            .model(model(
                vec![response(
                    vec![
                        call("1", "scheduled", json!(60_000)),
                        call("2", "scheduled", json!(60_000)),
                    ],
                    StopReason::ToolUse,
                )],
                false,
                Arc::default(),
            ))
            .tools(tools)
            .config(AgentConfig::builder().max_concurrent_tools(2_usize).build())
            .build();
        let cancellation = Cancellation::default();
        let (tx, rx) = flume::unbounded();
        let mut running = Box::pin(
            agent.run(
                input(),
                RunOptions::builder()
                    .cancellation(cancellation.clone())
                    .events(tx)
                    .build(),
            ),
        );
        let mut entered = 0;
        while entered < 2 {
            tokio::select! {
                result = &mut running => panic!("unexpected result: {result:?}"),
                event = rx.recv_async() => if matches!(event.unwrap(), AgentEvent::ToolProgress { .. }) {
                    entered += 1;
                }
            }
        }
        if cancel {
            cancellation.cancel();
            assert_eq!(running.await.unwrap().status, RunStatus::Cancelled);
        } else {
            drop(running);
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
        let history = agent.history().current().await;
        assert_eq!(history[2].content.len(), 2);
        animist::protocol::validate_replay(&history, &[]).unwrap();
    }
}

#[tokio::test]
async fn tool_timeout_is_not_retried_and_preserves_pending_results() {
    let mut tools = ToolRegistry::default();
    tools.register(HangTool).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("1", "hang", json!({})), call("2", "hang", json!({}))],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(
            AgentConfig::builder()
                .tool_timeout_ms(1_u64)
                .max_retries(3_u32)
                .build(),
        )
        .build();
    assert!(matches!(
        agent.run(input(), RunOptions::default()).await,
        Err(AgentError::ToolTimeout { .. })
    ));
    animist::protocol::validate_replay(&agent.history().current().await, &[]).unwrap();
}

struct HangTool;

struct StopSecond;

#[async_trait]
impl LayerImpl for StopSecond {
    async fn pre_tool_exec(
        &self,
        _: &Agent,
        context: &ToolContext<'_>,
    ) -> animist::agent::Result<animist::agent::ToolDecision> {
        Ok(if context.call.id == "2" {
            animist::agent::ToolDecision::Stop {
                reason: "stop siblings".into(),
            }
        } else {
            animist::agent::ToolDecision::Run
        })
    }
}

struct IndependentPolicy;

impl animist::agent::tool::ToolSchedulingPolicyImpl for IndependentPolicy {
    fn scheduling(
        &self,
        _: &ToolCall,
        _: animist::agent::tool::ToolScheduling,
    ) -> animist::agent::tool::ToolScheduling {
        animist::agent::tool::ToolScheduling::Resources {
            reads: vec![],
            writes: vec![],
        }
    }
}

#[tokio::test]
async fn stop_drops_parallel_siblings_and_does_not_start_later_waves() {
    let active = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools
        .register(ScheduledTool {
            active: std::sync::Arc::clone(&active),
            peak: Arc::default(),
            finished: Arc::default(),
            claims: animist::agent::tool::ToolScheduling::default(),
        })
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![
                    call("1", "scheduled", json!(60_000)),
                    call("2", "scheduled", json!(60_000)),
                    call("3", "scheduled", json!(60_000)),
                ],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(AgentConfig::builder().max_concurrent_tools(2_usize).build())
        .tool_scheduling_policy(animist::agent::tool::ToolSchedulingPolicy::new(
            IndependentPolicy,
        ))
        .layers(AgentLayer::builder().layer(Layer::new(StopSecond)).build())
        .build();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        agent.run(input(), RunOptions::default()),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(AgentError::Stopped { .. })));
    assert_eq!(active.load(Ordering::SeqCst), 0);
    animist::protocol::validate_replay(&agent.history().current().await, &[]).unwrap();
}

struct FanOut;

#[async_trait]
impl ToolImpl for FanOut {
    type Args = serde_json::Value;
    type Output = String;
    type Error = AgentError;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("fanout")
            .description("Dispatch two children")
            .input_schema(json!({"type":"object"}))
            .build()
    }

    async fn call(&self, context: &ToolContext<'_>, _: Self::Args) -> Result<String, AgentError> {
        let child = ToolCall::builder()
            .id("nested")
            .name("echo")
            .input(json!({"text":"nested"}))
            .build();
        let (first, second) =
            tokio::join!(context.dispatch(child.clone()), context.dispatch(child));
        first?;
        second?;
        Ok("done".into())
    }
}

#[tokio::test]
async fn child_fanout_is_bounded_and_nonexclusive_parents_cannot_dispatch() {
    for nonexclusive in [false, true] {
        let count = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::default();
        tools.register(FanOut).unwrap();
        tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
        let agent = Agent::builder()
            .model(model(
                vec![
                    response(
                        vec![call("parent", "fanout", json!({}))],
                        StopReason::ToolUse,
                    ),
                    response(vec![text("done")], StopReason::EndTurn),
                ],
                false,
                Arc::default(),
            ))
            .tools(tools)
            .config(AgentConfig::builder().max_child_calls(1_u64).build())
            .maybe_tool_scheduling_policy(
                nonexclusive
                    .then(|| animist::agent::tool::ToolSchedulingPolicy::new(IndependentPolicy)),
            )
            .build();
        let report = agent.run(input(), RunOptions::default()).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), usize::from(!nonexclusive));
        assert!(
            matches!(&*report.tool_failures[0].error, ToolError::Execution { source }
            if matches!((nonexclusive, source.downcast_ref::<AgentError>()),
                (true, Some(AgentError::ChildDispatchRequiresExclusive { .. })) |
                (false, Some(AgentError::ChildCallLimitExceeded { .. }))))
        );
    }
}

struct FanOutAll(Vec<ToolCall>);

#[async_trait]
impl ToolImpl for FanOutAll {
    type Args = serde_json::Value;
    type Output = String;
    type Error = AgentError;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("fanout_all")
            .description("Dispatch children concurrently")
            .input_schema(json!({"type":"object"}))
            .build()
    }

    async fn call(&self, context: &ToolContext<'_>, _: Self::Args) -> Result<String, AgentError> {
        let outputs = context.dispatch_all(self.0.clone()).await?;
        Ok(outputs
            .iter()
            .map(|output| {
                let text = match &output.content.0[..] {
                    [ToolContent::Text { text }] => text.clone(),
                    _ => String::new(),
                };
                if output.is_error {
                    format!("!{text}")
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
            .join(","))
    }
}

struct Rendezvous(Arc<tokio::sync::Barrier>);

#[async_trait]
impl ToolImpl for Rendezvous {
    type Args = String;
    type Output = String;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("rendezvous")
            .description("Barrier child")
            .input_schema(json!({"type":"string"}))
            .build()
    }

    fn scheduling(&self, _: &serde_json::Value) -> animist::agent::tool::ToolScheduling {
        animist::agent::tool::ToolScheduling::Resources {
            reads: vec![],
            writes: vec![],
        }
    }

    async fn call(&self, _context: &ToolContext<'_>, id: String) -> Result<String, Infallible> {
        self.0.wait().await;
        Ok(id)
    }
}

#[tokio::test]
async fn concurrent_children_overlap_and_results_return_in_call_order() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tools = ToolRegistry::default();
    tools.register(Rendezvous(Arc::clone(&barrier))).unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("rendezvous")
                .input(json!("1"))
                .build(),
            ToolCall::builder()
                .id("2")
                .name("rendezvous")
                .input(json!("2"))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "fanout_all", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(AgentConfig::builder().max_concurrent_tools(2_usize).build())
        .build();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        agent.run(input(), RunOptions::default()),
    )
    .await
    .expect("children must overlap for the barrier to release")
    .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert!(
        matches!(&agent.history().current().await[2].content[0], ContentBlock::ToolResult { content, is_error: false, .. } if content == &"1,2".into())
    );
}

#[tokio::test]
async fn conflicting_children_serialize_in_call_order() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::default();
    tools
        .register(ScheduledTool {
            active: Arc::clone(&active),
            peak: Arc::clone(&peak),
            finished: Arc::clone(&finished),
            claims: animist::agent::tool::ToolScheduling::Resources {
                reads: vec![],
                writes: vec!["shared".into()],
            },
        })
        .unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("scheduled")
                .input(json!(30))
                .build(),
            ToolCall::builder()
                .id("2")
                .name("scheduled")
                .input(json!(1))
                .build(),
            ToolCall::builder()
                .id("3")
                .name("scheduled")
                .input(json!(2))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "fanout_all", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(AgentConfig::builder().max_concurrent_tools(3_usize).build())
        .build();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        agent.run(input(), RunOptions::default()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(peak.load(Ordering::SeqCst), 1);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(*finished.lock().await, vec![30_u64, 1, 2]);
    assert!(
        matches!(&agent.history().current().await[2].content[0], ContentBlock::ToolResult { content, is_error: false, .. } if content == &"30,1,2".into())
    );
}

struct DenyChildTwo;

#[async_trait]
impl LayerImpl for DenyChildTwo {
    async fn pre_tool_exec(
        &self,
        _: &Agent,
        context: &ToolContext<'_>,
    ) -> animist::agent::Result<animist::agent::ToolDecision> {
        Ok(if context.call.id == "2" {
            animist::agent::ToolDecision::Deny {
                reason: "denied child".into(),
            }
        } else {
            animist::agent::ToolDecision::Run
        })
    }
}

#[tokio::test]
async fn a_denied_child_affects_only_itself() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tools = ToolRegistry::default();
    tools.register(Rendezvous(Arc::clone(&barrier))).unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("rendezvous")
                .input(json!("1"))
                .build(),
            ToolCall::builder()
                .id("2")
                .name("rendezvous")
                .input(json!("2"))
                .build(),
            ToolCall::builder()
                .id("3")
                .name("rendezvous")
                .input(json!("3"))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "fanout_all", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(DenyChildTwo))
                .build(),
        )
        .config(AgentConfig::builder().max_concurrent_tools(3_usize).build())
        .build();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        agent.run(input(), RunOptions::default()),
    )
    .await
    .expect("non-denied children overlap for the barrier to release")
    .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert!(
        matches!(&agent.history().current().await[2].content[0], ContentBlock::ToolResult { content, is_error: false, .. } if content == &"1,!denied child,3".into())
    );
}

struct StopChildTwo;

#[async_trait]
impl LayerImpl for StopChildTwo {
    async fn pre_tool_exec(
        &self,
        _: &Agent,
        context: &ToolContext<'_>,
    ) -> animist::agent::Result<animist::agent::ToolDecision> {
        Ok(if context.call.id == "2" {
            animist::agent::ToolDecision::Stop {
                reason: "child stop".into(),
            }
        } else {
            animist::agent::ToolDecision::Run
        })
    }
}

#[tokio::test]
async fn stop_from_one_child_drops_its_siblings_and_terminates() {
    let active = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools
        .register(ScheduledTool {
            active: Arc::clone(&active),
            peak: Arc::default(),
            finished: Arc::default(),
            claims: animist::agent::tool::ToolScheduling::Resources {
                reads: vec![],
                writes: vec![],
            },
        })
        .unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("scheduled")
                .input(json!(60_000))
                .build(),
            ToolCall::builder()
                .id("2")
                .name("scheduled")
                .input(json!(60_000))
                .build(),
            ToolCall::builder()
                .id("3")
                .name("scheduled")
                .input(json!(60_000))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("parent", "fanout_all", json!({}))],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(StopChildTwo))
                .build(),
        )
        .config(AgentConfig::builder().max_concurrent_tools(2_usize).build())
        .build();
    let (tx, rx) = flume::unbounded();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        agent.run(input(), RunOptions::builder().events(tx).build()),
    )
    .await
    .unwrap();
    assert!(matches!(&result, Err(AgentError::Stopped { reason, .. }) if reason == "child stop"));
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let started = rx
        .try_iter()
        .filter(|event| {
            matches!(event, AgentEvent::ToolStarted { parent_call_id: Some(id), .. } if id == "parent")
        })
        .count();
    assert_eq!(started, 1, "wave two must not start after the stop");
}

#[tokio::test]
async fn cancelling_the_run_drops_children_without_orphan_work() {
    let active = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools
        .register(ScheduledTool {
            active: Arc::clone(&active),
            peak: Arc::default(),
            finished: Arc::default(),
            claims: animist::agent::tool::ToolScheduling::Resources {
                reads: vec![],
                writes: vec![],
            },
        })
        .unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("scheduled")
                .input(json!(60_000))
                .build(),
            ToolCall::builder()
                .id("2")
                .name("scheduled")
                .input(json!(60_000))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("parent", "fanout_all", json!({}))],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(AgentConfig::builder().max_concurrent_tools(2_usize).build())
        .build();
    let cancellation = Cancellation::default();
    let (tx, rx) = flume::unbounded();
    let mut running = Box::pin(
        agent.run(
            input(),
            RunOptions::builder()
                .cancellation(cancellation.clone())
                .events(tx)
                .build(),
        ),
    );
    let mut entered = 0;
    while entered < 2 {
        tokio::select! {
            result = &mut running => panic!("unexpected result: {result:?}"),
            event = rx.recv_async() => if matches!(event.unwrap(), AgentEvent::ToolProgress { .. }) {
                entered += 1;
            }
        }
    }
    cancellation.cancel();
    assert_eq!(running.await.unwrap().status, RunStatus::Cancelled);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_child_dispatch_enforces_limits() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::clone(&count))).unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("echo")
                .input(json!({"text":"a"}))
                .build(),
            ToolCall::builder()
                .id("2")
                .name("echo")
                .input(json!({"text":"b"}))
                .build(),
            ToolCall::builder()
                .id("3")
                .name("echo")
                .input(json!({"text":"c"}))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "fanout_all", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(
            AgentConfig::builder()
                .max_concurrent_tools(3_usize)
                .max_child_calls(2_u64)
                .build(),
        )
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(
        matches!(&*report.tool_failures[0].error, ToolError::Execution { source }
        if matches!(source.downcast_ref::<AgentError>(), Some(AgentError::ChildCallLimitExceeded { .. })))
    );

    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::clone(&count))).unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("echo")
                .input(json!({"text":"a"}))
                .build(),
            ToolCall::builder()
                .id("2")
                .name("echo")
                .input(json!({"text":"b"}))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "fanout_all", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(
            AgentConfig::builder()
                .max_concurrent_tools(2_usize)
                .max_tool_calls(2_u64)
                .build(),
        )
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(
        matches!(&*report.tool_failures[0].error, ToolError::Execution { source }
        if matches!(source.downcast_ref::<AgentError>(), Some(AgentError::ToolCallLimitExceeded { .. })))
    );
}

struct NestedOrchestrator;

#[async_trait]
impl ToolImpl for NestedOrchestrator {
    type Args = serde_json::Value;
    type Output = String;
    type Error = AgentError;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("nested_orchestrator")
            .description("Try concurrent then serial dispatch")
            .input_schema(json!({"type":"object"}))
            .build()
    }

    async fn call(&self, context: &ToolContext<'_>, _: Self::Args) -> Result<String, AgentError> {
        let grandchild = ToolCall::builder()
            .id("grandchild")
            .name("echo")
            .input(json!({"text":"nested"}))
            .build();
        let concurrent = context.dispatch_all(vec![grandchild.clone()]).await;
        let serial = context.dispatch(grandchild).await?;
        Ok(format!("{concurrent:?}|{serial:?}"))
    }
}

#[tokio::test]
async fn nested_concurrent_dispatch_is_rejected_and_serial_stays_available() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(NestedOrchestrator).unwrap();
    tools.register(Echo(Arc::clone(&count))).unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("nested")
                .name("nested_orchestrator")
                .input(json!({}))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "fanout_all", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(AgentConfig::builder().max_concurrent_tools(2_usize).build())
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let history = agent.history().current().await;
    let ContentBlock::ToolResult { content, .. } = &history[2].content[0] else {
        panic!("expected a tool result")
    };
    let output = format!("{content:?}");
    assert!(output.contains("NestedConcurrentDispatch"), "{output}");
}

#[tokio::test]
async fn nonexclusive_parents_cannot_dispatch_children_concurrently() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::clone(&count))).unwrap();
    tools
        .register(FanOutAll(vec![
            ToolCall::builder()
                .id("1")
                .name("echo")
                .input(json!({"text":"a"}))
                .build(),
        ]))
        .unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "fanout_all", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .tool_scheduling_policy(animist::agent::tool::ToolSchedulingPolicy::new(
            IndependentPolicy,
        ))
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(
        matches!(&*report.tool_failures[0].error, ToolError::Execution { source }
        if matches!(source.downcast_ref::<AgentError>(), Some(AgentError::ChildDispatchRequiresExclusive { .. })))
    );
}

#[async_trait]
impl ToolImpl for HangTool {
    type Args = serde_json::Value;
    type Output = String;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builder()
            .name("hang")
            .description("Never finishes")
            .input_schema(json!({"type":"object"}))
            .build()
    }

    async fn call(
        &self,
        context: &ToolContext<'_>,
        _args: Self::Args,
    ) -> Result<String, Infallible> {
        context.progress("entered").await;
        std::future::pending().await
    }
}

#[tokio::test]
async fn dropping_tool_execution_leaves_replay_safe_interrupted_results() {
    let mut tools = ToolRegistry::default();
    tools.register(HangTool).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("1", "hang", json!({}))],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .build();
    let (tx, rx) = flume::unbounded();
    let mut running = Box::pin(agent.run(input(), RunOptions::builder().events(tx).build()));
    loop {
        tokio::select! {
            result = &mut running => panic!("unexpected result: {result:?}"),
            event = rx.recv_async() => if matches!(event.unwrap(), AgentEvent::ToolProgress { .. }) { break; }
        }
    }
    drop(running);
    let history = agent.history().current().await;
    assert_eq!(history.len(), 3);
    assert!(
        matches!(&history[2].content[0], ContentBlock::ToolResult { is_error: true, content, .. } if matches!(&content.0[..], [animist::history::ToolContent::Text { text }] if text.contains("interrupted")))
    );
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn cooperative_tool_cancellation_closes_every_call() {
    let mut tools = ToolRegistry::default();
    tools.register(HangTool).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("1", "hang", json!({})), call("2", "hang", json!({}))],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .build();
    let cancellation = Cancellation::default();
    let (tx, rx) = flume::unbounded();
    let running = agent.run(
        input(),
        RunOptions::builder()
            .cancellation(cancellation.clone())
            .events(tx)
            .build(),
    );
    tokio::pin!(running);
    loop {
        tokio::select! {
            result = &mut running => panic!("unexpected result: {result:?}"),
            event = rx.recv_async() => if matches!(event.unwrap(), AgentEvent::ToolProgress { .. }) { break; }
        }
    }
    cancellation.cancel();
    assert_eq!(running.await.unwrap().status, RunStatus::Cancelled);
    let history = agent.history().current().await;
    assert_eq!(history[2].content.len(), 2);
    assert!(
        history[2]
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { is_error: true, .. }))
    );
    let snapshot = agent.history().snapshot().await;
    assert_eq!(snapshot.entries()[2].revisions().len(), 1);
}

#[tokio::test]
async fn startup_runs_once_across_runs_and_shutdown_is_idempotent() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![
                response(vec![text("first")], StopReason::EndTurn),
                response(vec![text("second")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(trace("a", std::sync::Arc::clone(&log))))
                .build(),
        )
        .build();
    let first = agent.run(input(), RunOptions::default()).await.unwrap();
    let second = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(second.stats.steps, 1);
    agent.shutdown().await.unwrap();
    agent.shutdown().await.unwrap();
    let log = log.lock().await;
    assert_eq!(log.iter().filter(|entry| *entry == "a:startup").count(), 1);
    assert_eq!(log.iter().filter(|entry| *entry == "a:shutdown").count(), 1);
}

#[tokio::test]
async fn retry_budget_exhaustion_preserves_provider_error() {
    let mut retry = trace("retry", Arc::default());
    retry.retry = true;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![
                Err(animist::provider::ProviderError::Http {
                    status: 503,
                    body: "first".into(),
                    body_truncated: false,
                    backtrace: std::backtrace::Backtrace::capture(),
                }),
                Err(animist::provider::ProviderError::Http {
                    status: 503,
                    body: "second".into(),
                    body_truncated: false,
                    backtrace: std::backtrace::Backtrace::capture(),
                }),
            ],
            false,
            std::sync::Arc::clone(&requests),
        ))
        .config(AgentConfig::builder().max_retries(1_u32).build())
        .layers(AgentLayer::builder().layer(Layer::new(retry)).build())
        .build();
    assert!(matches!(agent.run(input(), RunOptions::default()).await,
        Err(AgentError::Inference { source: animist::provider::ProviderError::Http { body, .. }, .. }) if body == "second"));
    assert_eq!(requests.lock().await.len(), 2);
}

fn partial_attempt_model(hanging: bool, requests: Arc<Mutex<Vec<Vec<Message>>>>) -> Model {
    Model::builder()
        .id("partial-script")
        .provider(Provider::new(Script {
            replies: Mutex::new(
                vec![
                    Err(animist::provider::ProviderError::Http {
                        status: 503,
                        body: "original failure".into(),
                        body_truncated: false,
                        backtrace: std::backtrace::Backtrace::capture(),
                    }),
                    response(vec![text("recovered")], StopReason::EndTurn),
                ]
                .into(),
            ),
            requests,
            hanging,
            checkpoints: vec![
                ProviderEvent::ContentComplete {
                    block: call("not-executed", "echo", json!({"text":"never"})),
                },
                ProviderEvent::Usage {
                    usage: TokenUsage::builder()
                        .input(15_u32)
                        .output(1_u32)
                        .cache_creation(0_u32)
                        .cache_read(0_u32)
                        .build(),
                },
                ProviderEvent::Usage {
                    usage: TokenUsage::builder()
                        .input(15_u32)
                        .output(3_u32)
                        .cache_creation(0_u32)
                        .cache_read(0_u32)
                        .build(),
                },
            ],
        }))
        .info(ModelInfo::default())
        .build()
}

#[tokio::test]
async fn recovered_partial_attempts_keep_identity_and_usage_without_replaying_or_executing_them() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools
        .register(Echo(std::sync::Arc::clone(&executions)))
        .unwrap();
    let mut retry = trace("retry", Arc::default());
    retry.retry = true;
    let agent = Agent::builder()
        .model(partial_attempt_model(
            false,
            std::sync::Arc::clone(&requests),
        ))
        .config(AgentConfig::builder().max_retries(1_u32).build())
        .tools(tools)
        .layers(AgentLayer::builder().layer(Layer::new(retry)).build())
        .build();
    let (tx, rx) = flume::unbounded();
    let report = agent
        .run(input(), RunOptions::builder().events(tx).build())
        .await
        .unwrap();
    assert_eq!(report.stats.steps, 1);
    assert_eq!(report.stats.attempts, 2);
    assert_eq!(report.stats.reported_incomplete_attempts, 1);
    assert_eq!(report.stats.unreported_incomplete_attempts, 0);
    assert_eq!(report.stats.usage.input, 25);
    assert_eq!(report.stats.usage.output, 5);
    assert_eq!(report.stats.unpriced_attempts, 2);
    assert_eq!(report.stats.unpriced_steps, 1);
    assert_eq!(executions.load(Ordering::Relaxed), 0);
    let partial = &report.interrupted_attempts[0];
    assert_eq!(partial.run_id, report.id);
    assert_eq!(partial.step, 0);
    assert_eq!(partial.attempt, 0);
    assert_eq!(partial.model, "partial-script");
    assert!(!partial.cancelled);
    assert_eq!(partial.partial.display_text, "delta");
    assert!(
        matches!(&partial.partial.content[0], ContentBlock::ToolUse { id, .. } if id == "not-executed")
    );
    let requests = requests.lock().await;
    assert_eq!(requests[0], requests[1]);
    assert_eq!(agent.history().current().await.len(), 2);
    let events: Vec<_> = rx.try_iter().collect();
    assert!(events.iter().any(|event| matches!(event, AgentEvent::AttemptInterrupted {run_id, step:0, attempt:0} if *run_id == report.id)));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Provider { attempt: 1, .. }))
    );
}

#[tokio::test]
async fn cooperative_inference_cancellation_returns_partial_attempt_without_committing_it() {
    let agent = Agent::builder()
        .model(partial_attempt_model(true, Arc::default()))
        .build();
    let cancellation = Cancellation::default();
    let (tx, rx) = flume::unbounded();
    let running = agent.run(
        input(),
        RunOptions::builder()
            .cancellation(cancellation.clone())
            .events(tx)
            .build(),
    );
    tokio::pin!(running);
    loop {
        tokio::select! {
            result = &mut running => panic!("unexpected result {result:?}"),
            event = rx.recv_async() => if matches!(event.unwrap(), AgentEvent::Provider {
                event: ProviderEvent::Usage { usage }, ..
            } if usage.output == 3) { break; }
        }
    }
    cancellation.cancel();
    let report = running.await.unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(report.stats.steps, 0);
    assert_eq!(report.stats.reported_incomplete_attempts, 1);
    assert_eq!(report.stats.usage.output, 3);
    assert_eq!(report.interrupted_attempts.len(), 1);
    assert!(report.interrupted_attempts[0].cancelled);
    assert_eq!(report.interrupted_attempts[0].partial.content.len(), 1);
    assert_eq!(agent.history().current().await.len(), 1);
}

#[tokio::test]
async fn concurrent_history_edits_are_not_overwritten() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with_hanging_provider(requests);
    let cancellation = Cancellation::default();
    let (tx, rx) = flume::unbounded();
    let running = agent.run(
        input(),
        RunOptions::builder()
            .cancellation(cancellation.clone())
            .events(tx)
            .build(),
    );
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => panic!("unexpected result: {result:?}"),
        _ = rx.recv_async() => {}
    }
    agent.history().add_stored(input()).await;
    cancellation.cancel();
    running.await.unwrap();
    assert_eq!(agent.history().current().await.len(), 2);
}

struct EditHistory;

#[async_trait]
impl LayerImpl for EditHistory {
    async fn response(
        &self,
        agent: &Agent,
        _run: &RunContext,
        response: StreamResponse,
    ) -> animist::agent::Result<StreamResponse> {
        agent.history().add_stored(input()).await;
        Ok(response)
    }
}

#[tokio::test]
async fn stale_exchange_does_not_execute_tools_or_overwrite_external_edits() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(std::sync::Arc::clone(&count))).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("1", "echo", json!({"text":"never"}))],
                StopReason::ToolUse,
            )],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .layers(AgentLayer::builder().layer(Layer::new(EditHistory)).build())
        .build();
    let error = agent.run(input(), RunOptions::default()).await.unwrap_err();
    assert!(matches!(error, AgentError::History {
        source
    } if matches!(*source, animist::history::HistoryError::Transformation { source: animist::Error::Agent { source: AgentError::HistoryChanged { .. } } })));
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert_eq!(agent.history().current().await.len(), 2);
}

#[tokio::test]
async fn argument_error_retains_structured_source_in_events() {
    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::default())).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("1", "echo", json!({"text":1}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .build();
    let (tx, rx) = flume::unbounded();
    agent
        .run(input(), RunOptions::builder().events(tx).build())
        .await
        .unwrap();
    assert!(rx.try_iter().any(|event| matches!(event, AgentEvent::ToolFailed { error, .. }
        if matches!(&*error, animist::agent::tool::ToolError::Arguments { source } if source.is_data()))));
}

struct Transform(&'static str);

#[async_trait]
impl LayerImpl for Transform {
    async fn turn_start(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        message: &StoredMessage,
    ) -> animist::agent::Result<StoredMessage> {
        let mut message = message.clone();
        if let ContentBlock::Text { text } = &mut message.message_mut().content[0] {
            text.push_str(self.0);
        }
        Ok(message)
    }

    async fn prepare_request(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        mut request: StepRequest,
    ) -> animist::agent::Result<StepRequest> {
        request
            .messages
            .push(message(Role::User, vec![text(self.0)]));
        Ok(request)
    }

    async fn response(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        mut response: StreamResponse,
    ) -> animist::agent::Result<StreamResponse> {
        if let ContentBlock::Text { text } = &mut response.message.content[0] {
            text.push_str(self.0);
        }
        Ok(response)
    }
}

#[tokio::test]
async fn transformations_compose_without_persisting_request_only_context() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![response(vec![text("done")], StopReason::EndTurn)],
            false,
            std::sync::Arc::clone(&requests),
        ))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(Transform("a")))
                .next(Box::new(
                    AgentLayer::builder()
                        .layer(Layer::new(Transform("b")))
                        .build(),
                ))
                .build(),
        )
        .build();
    let original = input();
    let report = agent
        .run(original.clone(), RunOptions::default())
        .await
        .unwrap();
    let requests = requests.lock().await;
    assert_eq!(requests[0].len(), 3);
    assert!(matches!(&requests[0][0].content[0], ContentBlock::Text { text } if text == "helloab"));
    assert!(matches!(&requests[0][1].content[0], ContentBlock::Text { text } if text == "a"));
    assert!(matches!(&requests[0][2].content[0], ContentBlock::Text { text } if text == "b"));
    assert!(
        matches!(&report.response.unwrap().message().content[0], ContentBlock::Text { text } if text == "doneab")
    );
    let snapshot = agent.history().snapshot().await;
    assert_eq!(snapshot.entries().len(), 2);
    assert_eq!(
        snapshot.entries()[0].latest().timestamp(),
        original.timestamp()
    );
}

#[tokio::test]
async fn cancellation_releases_event_backpressure() {
    let agent = agent(vec![response(vec![text("done")], StopReason::EndTurn)]);
    let cancellation = Cancellation::default();
    let (tx, rx) = flume::bounded(0);
    let running = agent.run(
        input(),
        RunOptions::builder()
            .events(tx)
            .cancellation(cancellation.clone())
            .build(),
    );
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => panic!("unexpected result: {result:?}"),
        () = tokio::task::yield_now() => {}
    }
    cancellation.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap();
    drop(rx);
}

#[tokio::test]
async fn malformed_tool_stop_is_not_success() {
    let agent = agent(vec![response(vec![text("no calls")], StopReason::ToolUse)]);
    assert!(matches!(
        agent.run(input(), RunOptions::default()).await,
        Err(AgentError::MissingToolCalls { .. })
    ));
}

struct FailStartup(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl LayerImpl for FailStartup {
    async fn startup(&self, _agent: &Agent) -> animist::agent::Result<()> {
        self.0.lock().await.push("b:startup".into());
        Err(AgentError::Stopped {
            reason: "startup failed".into(),
            backtrace: std::backtrace::Backtrace::capture(),
        })
    }

    async fn shutdown(&self, _agent: &Agent) -> animist::agent::Result<()> {
        self.0.lock().await.push("b:shutdown".into());
        Ok(())
    }
}

#[tokio::test]
async fn failed_startup_rolls_back_initialized_layers() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(vec![], false, Arc::default()))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(trace("a", std::sync::Arc::clone(&log))))
                .next(Box::new(
                    AgentLayer::builder()
                        .layer(Layer::new(FailStartup(std::sync::Arc::clone(&log))))
                        .build(),
                ))
                .build(),
        )
        .build();
    assert!(agent.run(input(), RunOptions::default()).await.is_err());
    assert!(log.lock().await.starts_with(&[
        "a:startup".into(),
        "b:startup".into(),
        "b:shutdown".into(),
        "a:shutdown".into(),
    ]));
    agent.shutdown().await.unwrap();
    assert_eq!(
        log.lock()
            .await
            .iter()
            .filter(|entry| *entry == "a:shutdown")
            .count(),
        1
    );
    assert_eq!(agent.history().current().await, Vec::new());
}

#[tokio::test]
async fn child_depth_errors_are_structured_even_without_an_event_observer() {
    let mut tools = ToolRegistry::default();
    tools.register(Child).unwrap();
    tools.register(Echo(Arc::default())).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("parent", "child", json!({}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::default(),
        ))
        .tools(tools)
        .config(AgentConfig::builder().max_tool_depth(1_u32).build())
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.stats.tool_calls, 1);
    assert_eq!(report.tool_failures.len(), 1);
    assert!(
        matches!(&*report.tool_failures[0].error, animist::agent::tool::ToolError::Execution { source }
        if matches!(source.downcast_ref::<AgentError>(), Some(AgentError::ToolDepthExceeded { .. })))
    );
}

fn observation(text: &str) -> StoredMessage {
    StoredMessage::builder()
        .timestamp(jiff::Timestamp::now())
        .message(
            Message::builder()
                .role(Role::User)
                .kind(MessageKind::Observation)
                .content(vec![ContentBlock::Text { text: text.into() }])
                .build(),
        )
        .build()
}

struct Grace(&'static str);

#[async_trait]
impl LayerImpl for Grace {
    async fn step_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _outcome: &StepOutcome,
    ) -> animist::agent::Result<StepAction> {
        Ok(StepAction::builder()
            .messages(vec![observation(self.0)])
            .build())
    }
}

struct Nudge(&'static str);

#[async_trait]
impl LayerImpl for Nudge {
    async fn step_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _outcome: &StepOutcome,
    ) -> animist::agent::Result<StepAction> {
        Ok(StepAction::builder()
            .messages(vec![observation(self.0)])
            .continue_run(true)
            .build())
    }
}

struct ContinueFrom(RunStatus);

#[async_trait]
impl LayerImpl for ContinueFrom {
    async fn step_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        outcome: &StepOutcome,
    ) -> animist::agent::Result<StepAction> {
        Ok(StepAction::builder()
            .continue_run(outcome.terminal == Some(self.0))
            .build())
    }
}

struct AppendFixed(StoredMessage);

#[async_trait]
impl LayerImpl for AppendFixed {
    async fn step_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _outcome: &StepOutcome,
    ) -> animist::agent::Result<StepAction> {
        Ok(StepAction::builder().messages(vec![self.0.clone()]).build())
    }
}

struct TraceStepEnd {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
    text: &'static str,
}

#[async_trait]
impl LayerImpl for TraceStepEnd {
    async fn step_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        outcome: &StepOutcome,
    ) -> animist::agent::Result<StepAction> {
        self.log
            .lock()
            .await
            .push(format!("{}:step_end:{}", self.name, outcome.step));
        Ok(StepAction::builder()
            .messages(vec![observation(self.text)])
            .build())
    }
}

struct EditDuringStepEnd;

#[async_trait]
impl LayerImpl for EditDuringStepEnd {
    async fn step_end(
        &self,
        agent: &Agent,
        _run: &RunContext,
        _outcome: &StepOutcome,
    ) -> animist::agent::Result<StepAction> {
        agent.history().add_stored(input()).await;
        Ok(StepAction::builder()
            .messages(vec![observation("grace")])
            .build())
    }
}

struct HangingStepEnd;

#[async_trait]
impl LayerImpl for HangingStepEnd {
    async fn step_end(
        &self,
        _agent: &Agent,
        _run: &RunContext,
        _outcome: &StepOutcome,
    ) -> animist::agent::Result<StepAction> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn step_boundary_messages_reach_history_events_and_the_next_request() {
    let mut tools = ToolRegistry::default();
    tools.register(Echo(Arc::default())).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = flume::unbounded();
    let agent = Agent::builder()
        .model(model(
            vec![
                response(
                    vec![call("1", "echo", json!({"text":"hi"}))],
                    StopReason::ToolUse,
                ),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            Arc::clone(&requests),
        ))
        .tools(tools)
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(Grace("budget left")))
                .build(),
        )
        .build();
    let report = agent
        .run(input(), RunOptions::builder().events(tx).build())
        .await
        .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(report.stats.steps, 2);
    let history = agent.history().current().await;
    assert_eq!(history.len(), 6);
    assert_eq!(history[3].kind, MessageKind::Observation);
    for index in [3, 5] {
        assert!(
            matches!(&history[index].content[..], [ContentBlock::Text { text }]
            if text == "budget left")
        );
    }
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .iter()
            .any(|message| message.kind == MessageKind::Observation
                && matches!(&message.content[..], [ContentBlock::Text { text }]
                if text == "budget left"))
    );
    let appended: Vec<_> = rx
        .try_iter()
        .filter_map(|event| match event {
            AgentEvent::MessagesAppended { step, messages, .. } => Some((step, messages)),
            _ => None,
        })
        .collect();
    assert_eq!(
        appended
            .iter()
            .map(|(step, messages)| (*step, messages.len()))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 1)]
    );
    assert!(
        appended
            .iter()
            .all(|(_, messages)| matches!(&messages[0].message().content[..],
                [ContentBlock::Text { text }] if text == "budget left"))
    );
}

#[tokio::test]
async fn continuation_after_an_empty_reply_runs_another_step_within_max_steps() {
    for (max_steps, attempts, history_len) in [(1_u32, 1_u64, 3_usize), (2, 2, 5)] {
        let agent = Agent::builder()
            .model(model(
                vec![
                    response(vec![text("first")], StopReason::EndTurn),
                    response(vec![text("second")], StopReason::EndTurn),
                ],
                false,
                Arc::default(),
            ))
            .config(AgentConfig::builder().max_steps(max_steps).build())
            .layers(
                AgentLayer::builder()
                    .layer(Layer::new(Nudge("please continue")))
                    .build(),
            )
            .build();
        let report = agent.run(input(), RunOptions::default()).await.unwrap();
        assert_eq!(report.status, RunStatus::StepLimit);
        assert_eq!(report.stats.attempts, attempts);
        let history = agent.history().current().await;
        assert_eq!(history.len(), history_len);
        assert!(history.iter().any(
            |message| matches!(&message.content[..], [ContentBlock::Text { text }]
                if text == "please continue")
        ));
    }
}

#[tokio::test]
async fn max_tokens_and_paused_continue_when_a_layer_opts_in() {
    for (reason, terminal) in [
        (StopReason::MaxTokens, RunStatus::MaxTokens),
        (StopReason::PauseTurn, RunStatus::Paused),
    ] {
        let agent = Agent::builder()
            .model(model(
                vec![
                    response(vec![text("partial")], reason),
                    response(vec![text("rest")], StopReason::EndTurn),
                ],
                false,
                Arc::default(),
            ))
            .layers(
                AgentLayer::builder()
                    .layer(Layer::new(ContinueFrom(terminal)))
                    .build(),
            )
            .build();
        let report = agent.run(input(), RunOptions::default()).await.unwrap();
        assert_eq!(report.status, RunStatus::Completed);
        assert_eq!(report.stats.attempts, 2);
        assert_eq!(report.stats.steps, 2);
    }
}

#[tokio::test]
async fn step_boundary_appends_reject_non_replay_views() {
    let thinking = StoredMessage::builder()
        .timestamp(jiff::Timestamp::now())
        .message(
            Message::builder()
                .role(Role::User)
                .kind(MessageKind::Observation)
                .content(vec![ContentBlock::Thinking {
                    thinking: "posed".into(),
                    signature: None,
                }])
                .build(),
        )
        .build();
    let agent = Agent::builder()
        .model(model(
            vec![response(vec![text("done")], StopReason::EndTurn)],
            false,
            Arc::default(),
        ))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(AppendFixed(thinking)))
                .build(),
        )
        .build();
    let error = agent.run(input(), RunOptions::default()).await.unwrap_err();
    assert!(matches!(error, AgentError::Replay { .. }));
    assert_eq!(agent.history().current().await.len(), 2);
}

#[tokio::test]
async fn refused_and_context_window_exceeded_cannot_be_continued() {
    for (reason, status) in [
        (StopReason::Refusal, RunStatus::Refused),
        (
            StopReason::ContextWindowExceeded,
            RunStatus::ContextWindowExceeded,
        ),
    ] {
        let agent = Agent::builder()
            .model(model(
                vec![response(vec![text("no")], reason)],
                false,
                Arc::default(),
            ))
            .layers(
                AgentLayer::builder()
                    .layer(Layer::new(ContinueFrom(status)))
                    .build(),
            )
            .build();
        let error = agent.run(input(), RunOptions::default()).await.unwrap_err();
        assert!(matches!(error, AgentError::UnsupportedContinuation {
            status: denied,
            ..
        } if denied == status));
        assert_eq!(agent.history().current().await.len(), 2);
    }
}

#[tokio::test]
async fn step_boundary_appends_reject_edits_during_the_step() {
    let agent = Agent::builder()
        .model(model(
            vec![response(vec![text("done")], StopReason::EndTurn)],
            false,
            Arc::default(),
        ))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(EditDuringStepEnd))
                .build(),
        )
        .build();
    let error = agent.run(input(), RunOptions::default()).await.unwrap_err();
    let AgentError::History { source } = &error else {
        panic!("expected a history conflict, got {error:?}")
    };
    assert!(matches!(
        &**source,
        animist::history::HistoryError::Transformation {
            source: animist::Error::Agent {
                source: AgentError::HistoryChanged { messages, .. },
            },
        } if messages.len() == 1
            && matches!(&messages[0].message().content[..], [ContentBlock::Text { text }]
                if text == "grace")
    ));
    let history = agent.history().current().await;
    assert_eq!(history.len(), 3);
    assert!(
        matches!(&history[2].content[..], [ContentBlock::Text { text }]
        if text == "hello")
    );
}

#[tokio::test]
async fn step_boundary_appends_reject_assistant_roles_and_tool_blocks() {
    let assistant = StoredMessage::builder()
        .timestamp(jiff::Timestamp::now())
        .message(message(Role::Assistant, vec![text("posing")]))
        .build();
    let tooled = StoredMessage::builder()
        .timestamp(jiff::Timestamp::now())
        .message(
            Message::builder()
                .role(Role::User)
                .kind(MessageKind::Observation)
                .content(vec![call("1", "echo", json!({}))])
                .build(),
        )
        .build();
    for (message, invalid) in [(assistant, "role"), (tooled, "tool blocks")] {
        let agent = Agent::builder()
            .model(model(
                vec![response(vec![text("done")], StopReason::EndTurn)],
                false,
                Arc::default(),
            ))
            .layers(
                AgentLayer::builder()
                    .layer(Layer::new(AppendFixed(message)))
                    .build(),
            )
            .build();
        let error = agent.run(input(), RunOptions::default()).await.unwrap_err();
        assert!(
            matches!(
                &error,
                AgentError::InvalidRole { .. } | AgentError::ToolInputMessage { .. }
            ),
            "expected rejection for {invalid}, got {error:?}"
        );
        assert_eq!(agent.history().current().await.len(), 2);
    }
}

#[tokio::test]
async fn step_end_hooks_run_head_to_tail_and_append_in_layer_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let first = TraceStepEnd {
        name: "a",
        log: Arc::clone(&log),
        text: "first",
    };
    let second = TraceStepEnd {
        name: "b",
        log: Arc::clone(&log),
        text: "second",
    };
    let agent = Agent::builder()
        .model(model(
            vec![response(vec![text("done")], StopReason::EndTurn)],
            false,
            Arc::default(),
        ))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(first))
                .next(Box::new(
                    AgentLayer::builder().layer(Layer::new(second)).build(),
                ))
                .build(),
        )
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert_eq!(
        *log.lock().await,
        vec!["a:step_end:0".to_string(), "b:step_end:0".to_string()]
    );
    let history = agent.history().current().await;
    assert_eq!(history.len(), 4);
    assert!(
        matches!(&history[2].content[..], [ContentBlock::Text { text }]
        if text == "first")
    );
    assert!(
        matches!(&history[3].content[..], [ContentBlock::Text { text }]
        if text == "second")
    );
}

#[tokio::test]
async fn cancellation_during_step_end_returns_cancelled_without_appending() {
    let cancellation = Cancellation::default();
    let (tx, rx) = flume::unbounded();
    let agent = Agent::builder()
        .model(model(
            vec![response(vec![text("done")], StopReason::EndTurn)],
            false,
            Arc::default(),
        ))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(HangingStepEnd))
                .build(),
        )
        .build();
    let running = agent.run(
        input(),
        RunOptions::builder()
            .cancellation(cancellation.clone())
            .events(tx)
            .build(),
    );
    tokio::pin!(running);
    loop {
        tokio::select! {
            result = &mut running => panic!("unexpected result: {result:?}"),
            event = rx.recv_async() => {
                if matches!(event, Ok(AgentEvent::StepFinished { .. })) {
                    break;
                }
            }
        }
    }
    cancellation.cancel();
    let report = running.await.unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(agent.history().current().await.len(), 2);
    assert!(
        !rx.try_iter()
            .any(|event| matches!(event, AgentEvent::MessagesAppended { .. }))
    );
}
