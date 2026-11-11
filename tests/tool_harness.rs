#![cfg(feature = "testing")]
#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::{convert::Infallible, sync::Arc};

use animist::agent::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentLayer, Layer, LayerImpl, ToolDecision,
    testing::ToolHarness,
    tool::{
        ToolCall, ToolContext, ToolDefinition, ToolError, ToolExecutor, ToolExecutorImpl, ToolImpl,
        ToolOutput, ToolRegistry, ToolScheduling, ToolSelection,
    },
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Deserialize)]
struct Args {
    text: String,
}

struct Echo;

#[async_trait]
impl ToolImpl for Echo {
    type Args = Args;
    type Output = String;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        definition("echo")
    }

    async fn call(&self, context: &ToolContext<'_>, args: Args) -> Result<String, Infallible> {
        context.progress(&args.text).await;
        Ok(format!("{}:{}", context.run.id, args.text))
    }
}

fn definition(name: &str) -> ToolDefinition {
    ToolDefinition::builder()
        .name(name)
        .description("test tool")
        .input_schema(json!({}))
        .build()
}

fn child(id: &str) -> ToolCall {
    ToolCall::builder()
        .id(id)
        .name("echo")
        .input(json!({"text": id}))
        .build()
}

#[tokio::test]
async fn typed_arguments_outputs_and_progress_need_no_provider() {
    let mut harness = ToolHarness::builder().run_id(42_u64).build().unwrap();
    let events = harness.events();
    let output = harness.call(Echo, json!({"text": "hello"})).await.unwrap();
    assert_eq!(output.content, "42:hello".into());
    assert!(matches!(
        events.try_recv().unwrap(),
        AgentEvent::ToolProgress { id, content }
            if id == "harness-0" && content == "hello"
    ));
    assert!(matches!(
        harness.call(Echo, json!({"text": 12})).await,
        Err(ToolError::Arguments { .. })
    ));
}

struct WaitForCancellation;

struct PanickingExecutor;

#[async_trait]
impl ToolExecutorImpl for PanickingExecutor {
    fn definition(&self) -> ToolDefinition {
        definition("echo")
    }

    async fn execute(
        &self,
        _context: &ToolContext<'_>,
        _input: Value,
    ) -> Result<ToolOutput, ToolError> {
        tokio::task::yield_now().await;
        panic!("erased executor panic")
    }
}

#[tokio::test]
async fn erased_executor_panics_return_structured_errors_and_the_harness_remains_usable() {
    let mut harness = ToolHarness::builder().build().unwrap();
    assert!(matches!(
        harness.call_executor(ToolExecutor::new(PanickingExecutor), Value::Null).await,
        Err(ToolError::Panicked { message, .. }) if message == "erased executor panic"
    ));
    let output = harness
        .call(Echo, json!({"text": "still usable"}))
        .await
        .unwrap();
    assert_eq!(output.content, "0:still usable".into());
}

#[async_trait]
impl ToolExecutorImpl for WaitForCancellation {
    fn definition(&self) -> ToolDefinition {
        definition("wait")
    }

    async fn execute(
        &self,
        context: &ToolContext<'_>,
        _input: Value,
    ) -> Result<ToolOutput, ToolError> {
        context.progress("waiting").await;
        context.run.cancellation.cancelled().await;
        Ok(ToolOutput::builder().content("cancelled").build())
    }
}

#[tokio::test]
async fn erased_executors_observe_cancellation_from_a_cloned_handle() {
    let mut harness = ToolHarness::builder().build().unwrap();
    let cancellation = harness.cancellation();
    let events = harness.events();
    let (output, ()) = tokio::join!(
        harness.call_executor(ToolExecutor::new(WaitForCancellation), Value::Null),
        async {
            events.recv_async().await.unwrap();
            cancellation.cancel();
        }
    );
    assert_eq!(output.unwrap().content, "cancelled".into());
    assert!(harness.run().cancellation.is_cancelled());
}

struct Parent {
    parallel: bool,
    scheduling: ToolScheduling,
    selection: Option<ToolSelection>,
}

struct PanicChildHook;

#[async_trait]
impl LayerImpl for PanicChildHook {
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
    ) -> Result<ToolDecision, AgentError> {
        panic!("child layer panic")
    }
}

#[tokio::test]
async fn child_panics_are_recorded_at_the_executor_boundary_they_cross() {
    for parallel in [false, true] {
        for layer_panic in [false, true] {
            let mut tools = ToolRegistry::default();
            tools
                .register_executor(ToolExecutor::new(PanickingExecutor))
                .unwrap();
            let layers = layer_panic.then(|| {
                AgentLayer::builder()
                    .layer(Layer::new(PanicChildHook))
                    .build()
            });
            let mut harness = ToolHarness::builder()
                .tools(tools)
                .maybe_layers(layers)
                .build()
                .unwrap();
            let result = harness
                .call(
                    Parent {
                        parallel,
                        scheduling: ToolScheduling::Exclusive,
                        selection: None,
                    },
                    json!(["child"]),
                )
                .await;
            let failures = harness.run().tool_failures().await;
            if layer_panic {
                assert!(matches!(
                    result, Err(ToolError::Panicked { message, .. }) if message == "child layer panic"
                ));
                assert!(failures.is_empty(), "the child executor was never entered");
            } else {
                let output = result.unwrap();
                assert!(output.is_error);
                assert_eq!(
                    output.content,
                    "Tool execution panicked: erased executor panic".into()
                );
                assert_eq!(failures.len(), 1);
                assert_eq!(failures[0].call.id, "child");
                assert!(matches!(
                    &*failures[0].error, ToolError::Panicked { message, .. } if message == "erased executor panic"
                ));
            }
        }
    }
}

#[async_trait]
impl ToolImpl for Parent {
    type Args = Vec<String>;
    type Output = ToolOutput;
    type Error = AgentError;

    fn definition(&self) -> ToolDefinition {
        definition("parent")
    }

    fn scheduling(&self, _input: &Value) -> ToolScheduling {
        self.scheduling.clone()
    }

    async fn call(
        &self,
        context: &ToolContext<'_>,
        calls: Vec<String>,
    ) -> Result<ToolOutput, AgentError> {
        let calls: Vec<_> = calls.iter().map(|id| child(id)).collect();
        if self.parallel {
            let outputs = match &self.selection {
                Some(selection) => context.dispatch_all_in(calls, selection).await?,
                None => context.dispatch_all(calls).await?,
            };
            return Ok(outputs.into_iter().next().unwrap());
        }
        let mut output = ToolOutput::builder().content("empty").build();
        for call in calls {
            output = match &self.selection {
                Some(selection) => context.dispatch_in(call, selection).await?,
                None => context.dispatch(call).await?,
            };
        }
        Ok(output)
    }
}

type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

struct DenyEcho(Seen);

#[async_trait]
impl LayerImpl for DenyEcho {
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        context: &ToolContext<'_>,
    ) -> Result<ToolDecision, AgentError> {
        self.0.lock().await.push((
            context.call.name.clone(),
            context.parent_call_id.map(str::to_owned),
        ));
        Ok(ToolDecision::Deny {
            reason: "policy denied".into(),
        })
    }
}

#[tokio::test]
async fn children_use_the_supplied_registry_and_layers_in_both_dispatch_modes() {
    for parallel in [false, true] {
        for selected in [false, true] {
            let mut tools = ToolRegistry::default();
            tools.register(Echo).unwrap();
            let selection = tools.selection(|_| true);
            let seen = Seen::default();
            let mut harness = ToolHarness::builder()
                .tools(tools)
                .layers(
                    AgentLayer::builder()
                        .layer(Layer::new(DenyEcho(Arc::clone(&seen))))
                        .build(),
                )
                .build()
                .unwrap();
            let output = harness
                .call(
                    Parent {
                        parallel,
                        scheduling: ToolScheduling::Exclusive,
                        selection: selected.then_some(selection),
                    },
                    json!(["a"]),
                )
                .await
                .unwrap();
            assert!(output.is_error);
            assert_eq!(output.content, "policy denied".into());
            assert_eq!(
                *seen.lock().await,
                vec![("echo".into(), Some("harness-0".into()))]
            );
        }
    }

    for selected in [false, true] {
        let mut tools = ToolRegistry::default();
        tools.register(Echo).unwrap();
        let selection = tools.selection(|_| true);
        let mut harness = ToolHarness::builder().tools(tools).build().unwrap();
        let output = harness
            .call(
                Parent {
                    parallel: false,
                    scheduling: ToolScheduling::Exclusive,
                    selection: selected.then_some(selection),
                },
                json!(["a"]),
            )
            .await
            .unwrap();
        assert_eq!(output.content, "0:a".into());
        assert_eq!(harness.run().stats.snapshot().await.tool_calls, 1);
    }
}

#[tokio::test]
async fn child_limits_and_root_resource_claims_are_enforced() {
    for (config, parallel, scheduling, selected, expected) in [
        (
            AgentConfig::builder().max_child_calls(1_u64).build(),
            false,
            ToolScheduling::Exclusive,
            false,
            "child count",
        ),
        (
            AgentConfig::builder().max_child_calls(1_u64).build(),
            false,
            ToolScheduling::Exclusive,
            true,
            "child count",
        ),
        (
            AgentConfig::builder().max_child_calls(1_u64).build(),
            true,
            ToolScheduling::Exclusive,
            false,
            "child count",
        ),
        (
            AgentConfig::builder().max_child_calls(1_u64).build(),
            true,
            ToolScheduling::Exclusive,
            true,
            "child count",
        ),
        (
            AgentConfig::builder().max_tool_depth(1_u32).build(),
            false,
            ToolScheduling::Exclusive,
            false,
            "depth",
        ),
        (
            AgentConfig::builder().max_tool_depth(1_u32).build(),
            false,
            ToolScheduling::Exclusive,
            true,
            "depth",
        ),
        (
            AgentConfig::builder().max_tool_calls(1_u64).build(),
            false,
            ToolScheduling::Exclusive,
            false,
            "run count",
        ),
        (
            AgentConfig::builder().max_tool_calls(1_u64).build(),
            false,
            ToolScheduling::Exclusive,
            true,
            "run count",
        ),
        (
            AgentConfig::default(),
            false,
            ToolScheduling::Resources {
                reads: vec![],
                writes: vec![],
            },
            false,
            "reservation",
        ),
        (
            AgentConfig::default(),
            false,
            ToolScheduling::Resources {
                reads: vec![],
                writes: vec![],
            },
            true,
            "reservation",
        ),
    ] {
        let mut tools = ToolRegistry::default();
        tools.register(Echo).unwrap();
        let selection = tools.selection(|_| true);
        let mut harness = ToolHarness::builder()
            .tools(tools)
            .config(config)
            .build()
            .unwrap();
        let error = harness
            .call(
                Parent {
                    parallel,
                    scheduling,
                    selection: selected.then_some(selection),
                },
                json!(["a", "b"]),
            )
            .await
            .unwrap_err();
        let ToolError::Execution { source } = error else {
            panic!("expected an execution error for {expected}");
        };
        let error = source.downcast_ref::<AgentError>().unwrap();
        assert!(
            matches!(
                (expected, error),
                ("child count", AgentError::ChildCallLimitExceeded { .. })
                    | ("depth", AgentError::ToolDepthExceeded { .. })
                    | ("run count", AgentError::ToolCallLimitExceeded { .. })
                    | (
                        "reservation",
                        AgentError::ChildDispatchRequiresExclusive { .. }
                    )
            ),
            "{expected}: {error:?}"
        );
    }
}

#[test]
fn zero_concurrency_is_rejected_before_child_wave_dispatch() {
    assert!(matches!(
        ToolHarness::builder()
            .config(AgentConfig::builder().max_concurrent_tools(0_usize).build())
            .build(),
        Err(AgentError::InvalidLimits { .. })
    ));
}

#[tokio::test]
async fn selected_dispatch_admits_only_registered_selection_members() {
    for parallel in [false, true] {
        let mut tools = ToolRegistry::default();
        tools.register(Echo).unwrap();
        let empty = tools.selection(|_| false);
        let mut harness = ToolHarness::builder().tools(tools).build().unwrap();
        let output = harness
            .call(
                Parent {
                    parallel,
                    scheduling: ToolScheduling::Exclusive,
                    selection: Some(empty),
                },
                json!(["a"]),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(output.content, "Unknown tool: echo".into());
        assert_eq!(harness.run().stats.snapshot().await.tool_calls, 0);
    }

    let mut foreign = ToolRegistry::default();
    foreign.register(Echo).unwrap();
    let foreign = foreign.selection(|_| true);
    let mut tools = ToolRegistry::default();
    tools.register(Echo).unwrap();
    let mut harness = ToolHarness::builder().tools(tools).build().unwrap();
    let output = harness
        .call(
            Parent {
                parallel: false,
                scheduling: ToolScheduling::Exclusive,
                selection: Some(foreign),
            },
            json!(["a"]),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    assert_eq!(output.content, "0:a".into());
}

struct SelectedRoot(ToolSelection);

struct Outside;

#[async_trait]
impl ToolImpl for Outside {
    type Args = Args;
    type Output = String;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        definition("outside")
    }

    async fn call(&self, context: &ToolContext<'_>, args: Args) -> Result<String, Infallible> {
        context.progress(&args.text).await;
        Ok(format!("{}:{}", context.run.id, args.text))
    }
}

#[async_trait]
impl ToolImpl for SelectedRoot {
    type Args = serde_json::Value;
    type Output = ToolOutput;
    type Error = AgentError;

    fn definition(&self) -> ToolDefinition {
        definition("selected-root")
    }

    async fn call(
        &self,
        context: &ToolContext<'_>,
        _args: Self::Args,
    ) -> Result<ToolOutput, AgentError> {
        context
            .dispatch_in(child_of("middle", "middle"), &self.0)
            .await
    }
}

struct Middle {
    target: &'static str,
    all_in: bool,
    selection: ToolSelection,
}

#[async_trait]
impl ToolImpl for Middle {
    type Args = serde_json::Value;
    type Output = ToolOutput;
    type Error = AgentError;

    fn definition(&self) -> ToolDefinition {
        definition("middle")
    }

    async fn call(
        &self,
        context: &ToolContext<'_>,
        _args: Self::Args,
    ) -> Result<ToolOutput, AgentError> {
        let call = child_of("nested", self.target);
        if self.all_in {
            let mut outputs = context.dispatch_all_in(vec![call], &self.selection).await?;
            return Ok(outputs.remove(0));
        }
        context.dispatch(call).await
    }
}

fn child_of(id: &str, name: &str) -> ToolCall {
    ToolCall::builder()
        .id(id)
        .name(name)
        .input(json!({"text": id}))
        .build()
}

fn empty_selection() -> ToolSelection {
    ToolRegistry::default().selection(|_| false)
}

#[tokio::test]
async fn selected_children_inherit_the_selection_scope() {
    for target in ["echo", "outside"] {
        let mut tools = ToolRegistry::default();
        tools.register(Echo).unwrap();
        tools.register(Outside).unwrap();
        tools
            .register(Middle {
                target,
                all_in: false,
                selection: empty_selection(),
            })
            .unwrap();
        let selection = tools.selection(|entry| entry.definition.name != "outside");
        let mut harness = ToolHarness::builder().tools(tools).build().unwrap();
        let output = harness
            .call(SelectedRoot(selection), Value::Null)
            .await
            .unwrap();
        if target == "echo" {
            assert!(!output.is_error);
            assert_eq!(output.content, "0:nested".into());
        } else {
            assert!(output.is_error);
            assert_eq!(output.content, "Unknown tool: outside".into());
        }
    }

    let mut tools = ToolRegistry::default();
    tools.register(Echo).unwrap();
    tools
        .register(Middle {
            target: "echo",
            all_in: true,
            selection: empty_selection(),
        })
        .unwrap();
    let selection = tools.selection(|_| true);
    let mut harness = ToolHarness::builder().tools(tools).build().unwrap();
    let output = harness
        .call(SelectedRoot(selection), Value::Null)
        .await
        .unwrap();
    assert!(output.is_error);
    let failures = harness.run().tool_failures().await;
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].call.id, "middle");
    let ToolError::Execution { source } = &*failures[0].error else {
        panic!("expected the nested concurrent dispatch failure");
    };
    assert!(matches!(
        source.downcast_ref::<AgentError>(),
        Some(AgentError::NestedConcurrentDispatch { .. })
    ));
}

struct StopChildren;

#[async_trait]
impl LayerImpl for StopChildren {
    async fn pre_tool_exec(
        &self,
        _agent: &Agent,
        _context: &ToolContext<'_>,
    ) -> Result<ToolDecision, AgentError> {
        Ok(ToolDecision::Stop {
            reason: "test stop".into(),
        })
    }
}

struct SwallowStop {
    wait: bool,
}

#[async_trait]
impl ToolImpl for SwallowStop {
    type Args = ();
    type Output = String;
    type Error = Infallible;

    fn definition(&self) -> ToolDefinition {
        definition("swallow-stop")
    }

    async fn call(&self, context: &ToolContext<'_>, (): ()) -> Result<String, Infallible> {
        let _ = context.dispatch(child("stopped-child")).await;
        if self.wait {
            std::future::pending::<()>().await;
        }
        Ok("ignored stop".into())
    }
}

#[tokio::test]
async fn child_stops_terminate_roots_even_when_caught_and_reject_later_roots() {
    for wait in [false, true] {
        let mut tools = ToolRegistry::default();
        tools.register(Echo).unwrap();
        let mut harness = ToolHarness::builder()
            .tools(tools)
            .layers(
                AgentLayer::builder()
                    .layer(Layer::new(StopChildren))
                    .build(),
            )
            .build()
            .unwrap();
        let events = harness.events();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            harness.call(SwallowStop { wait }, Value::Null),
        )
        .await
        .expect("a child stop must terminate the root")
        .unwrap_err();
        let ToolError::Execution { source } = error else {
            panic!("expected a structured stop error");
        };
        assert!(matches!(
            source.downcast_ref::<AgentError>(),
            Some(AgentError::Stopped { reason, .. }) if reason == "test stop"
        ));
        let error = harness
            .call(Echo, json!({"text": "must not execute"}))
            .await
            .unwrap_err();
        let ToolError::Execution { source } = error else {
            panic!("expected the persistent stop error");
        };
        assert!(matches!(
            source.downcast_ref::<AgentError>(),
            Some(AgentError::Stopped { reason, .. }) if reason == "test stop"
        ));
        assert!(events.is_empty(), "a later root must not emit progress");
    }
}
