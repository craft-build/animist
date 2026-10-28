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
        Recovery, RunContext, RunOptions, RunStatus, Section, StepRequest,
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
}

#[async_trait]
impl ProviderImpl for Script {
    fn name(&self) -> &str {
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
        })))
        .info(ModelInfo::default())
        .build()
}

fn agent(replies: Vec<Reply>) -> Agent {
    Agent::builder()
        .model(model(replies, false, Default::default()))
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
struct Trace {
    name: &'static str,
    log: Arc<Mutex<Vec<String>>>,
    deny: bool,
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

#[tokio::test]
async fn external_api_streams_without_tools_and_preserves_metadata() {
    let agent = agent(vec![response(vec![text("done")], StopReason::EndTurn)]);
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
            requests.clone(),
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
    tools.register(Echo(count.clone())).unwrap();
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
            requests.clone(),
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
            history.add(message).await;
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
                requests.clone(),
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
                source: ReplayError::OrphanResult { id, location }
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
        .model(model(vec![], false, requests.clone()))
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
    let mut first = trace("a", log.clone());
    first.deny = true;
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(count.clone())).unwrap();
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
            Default::default(),
        ))
        .tools(tools)
        .layers(layers(first, trace("b", log.clone())))
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
        Err(AgentError::ShutDown)
    ));
}

#[tokio::test]
async fn unknown_and_invalid_tools_become_results_not_run_failures() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::default();
    tools.register(Echo(count.clone())).unwrap();
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
            Default::default(),
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
        Err(AgentError::AlreadyRunning)
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
    let mut retry = trace("retry", log.clone());
    retry.retry = true;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![
                Err(animist::provider::ProviderError::Http {
                    status: 503,
                    body: "busy".into(),
                }),
                response(vec![text("done")], StopReason::EndTurn),
            ],
            false,
            requests.clone(),
        ))
        .config(AgentConfig::builder().max_retries(1_u32).build())
        .layers(AgentLayer::builder().layer(Layer::new(retry)).build())
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.stats.attempts, 2);
    assert_eq!(report.stats.steps, 1);
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
            Default::default(),
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
        tools.register(Echo(count.clone())).unwrap();
        let agent = Agent::builder()
            .model(model(
                vec![response(
                    vec![call("1", "echo", json!({"text":"never"}))],
                    reason,
                )],
                false,
                Default::default(),
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
    let mut first = trace("a", log.clone());
    first.fail_end = true;
    let agent = Agent::builder()
        .model(model(
            vec![Err(animist::provider::ProviderError::Http {
                status: 500,
                body: "failed".into(),
            })],
            false,
            Default::default(),
        ))
        .layers(layers(first, trace("b", log.clone())))
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
    registry.register(Echo(Default::default())).unwrap();
    assert!(matches!(
        registry.register(Echo(Default::default())),
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
    tools.register(Echo(count.clone())).unwrap();
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
            Default::default(),
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

struct HangTool;

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
            Default::default(),
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
            Default::default(),
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
    assert_eq!(snapshot.entries()[2].revisions().len(), 2);
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
            Default::default(),
        ))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(trace("a", log.clone())))
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
    let mut retry = trace("retry", Default::default());
    retry.retry = true;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::builder()
        .model(model(
            vec![
                Err(animist::provider::ProviderError::Http {
                    status: 503,
                    body: "first".into(),
                }),
                Err(animist::provider::ProviderError::Http {
                    status: 503,
                    body: "second".into(),
                }),
            ],
            false,
            requests.clone(),
        ))
        .config(AgentConfig::builder().max_retries(1_u32).build())
        .layers(AgentLayer::builder().layer(Layer::new(retry)).build())
        .build();
    assert!(matches!(agent.run(input(), RunOptions::default()).await,
        Err(AgentError::Provider { source: animist::provider::ProviderError::Http { body, .. } }) if body == "second"));
    assert_eq!(requests.lock().await.len(), 2);
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
    tools.register(Echo(count.clone())).unwrap();
    let agent = Agent::builder()
        .model(model(
            vec![response(
                vec![call("1", "echo", json!({"text":"never"}))],
                StopReason::ToolUse,
            )],
            false,
            Default::default(),
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
    tools.register(Echo(Default::default())).unwrap();
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
            Default::default(),
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
            requests.clone(),
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
        _ = tokio::task::yield_now() => {}
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
        Err(AgentError::MissingToolCalls)
    ));
}

struct FailStartup(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl LayerImpl for FailStartup {
    async fn startup(&self, _agent: &Agent) -> animist::agent::Result<()> {
        self.0.lock().await.push("b:startup".into());
        Err(AgentError::Stopped {
            reason: "startup failed".into(),
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
        .model(model(vec![], false, Default::default()))
        .layers(
            AgentLayer::builder()
                .layer(Layer::new(trace("a", log.clone())))
                .next(Box::new(
                    AgentLayer::builder()
                        .layer(Layer::new(FailStartup(log.clone())))
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
    assert!(agent.history().current().await.is_empty());
}

#[tokio::test]
async fn child_depth_errors_are_structured_even_without_an_event_observer() {
    let mut tools = ToolRegistry::default();
    tools.register(Child).unwrap();
    tools.register(Echo(Default::default())).unwrap();
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
            Default::default(),
        ))
        .tools(tools)
        .config(AgentConfig::builder().max_tool_depth(1_u32).build())
        .build();
    let report = agent.run(input(), RunOptions::default()).await.unwrap();
    assert_eq!(report.stats.tool_calls, 1);
    assert_eq!(report.tool_failures.len(), 1);
    assert!(
        matches!(&*report.tool_failures[0].error, animist::agent::tool::ToolError::Execution { source }
        if matches!(source.downcast_ref::<AgentError>(), Some(AgentError::ToolDepthExceeded)))
    );
}
