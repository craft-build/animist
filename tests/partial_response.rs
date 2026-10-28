use animist::{
    history::ContentBlock,
    protocol::{
        Protocol, ProtocolError,
        anthropic::{AnthropicProtocolError, Messages},
        openai::{ChatCompletions, Responses},
    },
    provider::{ExchangeEvent, InferenceExchange, ProviderError, ProviderEvent},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::{Duration, timeout},
};

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

async fn fixture(body: String) -> (reqwest::Response, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert_ne!(socket.read(&mut request).await.unwrap(), 0);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        socket
            .write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes())
            .await
            .unwrap();
        let mut byte = [0];
        let closed = timeout(Duration::from_secs(3), socket.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
    });
    (
        reqwest::get(format!("http://{address}")).await.unwrap(),
        server,
    )
}

fn exchange<P>(protocol: P, response: reqwest::Response) -> InferenceExchange<'static>
where
    P: Protocol<Response = reqwest::Response> + Send + 'static,
{
    InferenceExchange::new(move |tx| async move {
        protocol.assemble(response, &tx).await.map_err(Into::into)
    })
}

async fn until(exchange: &mut InferenceExchange<'_>, predicate: impl Fn(&ProviderEvent) -> bool) {
    timeout(Duration::from_secs(3), async {
        loop {
            match exchange.next().await {
                Some(ExchangeEvent::Provider(event)) if predicate(&event) => break,
                Some(ExchangeEvent::Provider(_)) => {}
                _ => panic!("exchange ended before expected checkpoint"),
            }
        }
    })
    .await
    .unwrap();
}

fn start() -> Value {
    json!({"type":"message_start","message":{"role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":0}}})
}

#[tokio::test]
async fn cancellation_retains_display_but_not_unsigned_or_unclosed_thinking() {
    for signed in [false, true] {
        let mut events = vec![
            start(),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"visible"}}),
        ];
        if signed {
            events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-signature"}}));
        }
        let (response, server) = fixture(sse(&events)).await;
        let mut exchange = exchange(Messages, response);
        until(&mut exchange, |event| {
            matches!(event, ProviderEvent::ThinkingDelta { .. })
        })
        .await;
        let partial = exchange.cancel();
        assert_eq!(partial.display_thinking, "visible");
        assert!(partial.content.is_empty());
        assert_eq!(partial.usage.as_ref().unwrap().input, 12);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn closed_signatures_redaction_and_calls_survive_without_executing_partial_json() {
    let events = vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"plan","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"complete","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"partial","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{\"x\":"}}),
    ];
    let (response, server) = fixture(sse(&events)).await;
    let mut exchange = exchange(Messages, response);
    until(
        &mut exchange,
        |event| matches!(event, ProviderEvent::ToolInputDelta { id, .. } if id == "partial"),
    )
    .await;
    let partial = exchange.cancel();
    assert_eq!(
        partial.content,
        vec![
            ContentBlock::Thinking {
                thinking: "plan".into(),
                signature: Some("signed".into())
            },
            ContentBlock::RedactedThinking {
                data: "opaque".into()
            },
            ContentBlock::ToolUse {
                id: "complete".into(),
                name: "lookup".into(),
                input: json!({"x":1}),
                thought_signature: None
            },
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancelling_between_text_fragments_or_dropping_exchange_closes_transport() {
    for cancel in [true, false] {
        let events = vec![
            start(),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"first 🦀"}}),
        ];
        let (response, server) = fixture(sse(&events)).await;
        let mut exchange = exchange(Messages, response);
        until(&mut exchange, |event| {
            matches!(event, ProviderEvent::TextDelta { .. })
        })
        .await;
        if cancel {
            let partial = exchange.cancel();
            assert_eq!(partial.display_text, "first 🦀");
            assert!(partial.content.is_empty());
            assert!(exchange.next().await.is_none());
        }
        drop(exchange);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn failures_keep_typed_errors_and_latest_usage_without_double_counting_updates() {
    let payload =
        json!({"type":"overloaded_error","message":"retry later","details":{"opaque":42}});
    let (response, server) = fixture(sse(&[
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"partial"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
        json!({"type":"error","error":payload}),
    ])).await;
    let mut exchange = exchange(Messages, response);
    loop {
        if let Some(ExchangeEvent::Finished(result)) = exchange.next().await {
            assert!(matches!(result, Err(ProviderError::Protocol {
                source: ProtocolError::Anthropic { source: AnthropicProtocolError::Api { error } }
            }) if error == payload));
            break;
        }
    }
    assert_eq!(exchange.partial().display_text, "partial");
    assert_eq!(
        exchange.partial().content,
        vec![ContentBlock::Text {
            text: "partial".into()
        }]
    );
    assert_eq!(exchange.partial().usage.as_ref().unwrap().input, 12);
    assert_eq!(exchange.partial().usage.as_ref().unwrap().output, 7);
    server.await.unwrap();
}

#[tokio::test]
async fn chat_requires_finish_boundary_even_for_syntactically_complete_tool_arguments() {
    for finished in [false, true] {
        let mut events = vec![json!({"choices":[{"index":0,"delta":{
            "content":"visible","tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{}"}}]
        }}]})];
        if finished {
            events.push(json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}));
        }
        events.push(json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":3}}));
        let (response, server) = fixture(sse(&events)).await;
        let mut exchange = exchange(ChatCompletions, response);
        until(&mut exchange, |event| {
            matches!(event, ProviderEvent::Usage { .. })
        })
        .await;
        let partial = exchange.cancel();
        assert_eq!(partial.display_text, "visible");
        assert_eq!(partial.usage.as_ref().unwrap().input, 20);
        assert_eq!(partial.content.len(), if finished { 2 } else { 0 });
        if finished {
            assert!(
                matches!(&partial.content[1], ContentBlock::ToolUse { id, input, .. } if id == "call" && input == &json!({}))
            );
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn responses_preserve_closed_opaque_items_in_output_order_and_failure_usage() {
    let reasoning = json!({"type":"reasoning","id":"rs","encrypted_content":"opaque","summary":[{"type":"summary_text","text":"plan"}]});
    let (response, server) = fixture(sse(&[
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"call","name":"lookup","arguments":"{}","status":"completed"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
        json!({"type":"response.output_text.delta","delta":"unfinished"}),
        json!({"type":"response.failed","response":{"error":{"code":"failed","opaque":42},"usage":{"input_tokens":10,"output_tokens":4}}}),
    ])).await;
    let mut exchange = exchange(Responses, response);
    loop {
        if let Some(ExchangeEvent::Finished(result)) = exchange.next().await {
            assert!(matches!(result, Err(ProviderError::Protocol {
                source: ProtocolError::OpenAI { source: animist::protocol::openai::OpenAIProtocolError::ResponseFailed { error } }
            }) if error == json!({"code":"failed","opaque":42})));
            break;
        }
    }
    let partial = exchange.partial();
    assert_eq!(partial.display_text, "unfinished");
    assert!(
        matches!(&partial.content[0], ContentBlock::Thinking { signature:Some(signature), .. } if serde_json::from_str::<Value>(signature).unwrap() == reasoning)
    );
    assert!(matches!(&partial.content[1], ContentBlock::ToolUse { id, .. } if id == "call"));
    assert_eq!(partial.usage.as_ref().unwrap().output, 4);
    server.await.unwrap();
}

#[tokio::test]
async fn cancelling_next_is_resumable_and_queued_checkpoints_precede_terminal_errors() {
    let (release, wait) = tokio::sync::oneshot::channel();
    let mut exchange = InferenceExchange::new(|tx| async move {
        tx.send_async(ProviderEvent::TextDelta {
            text: "visible".into(),
        })
        .await
        .unwrap();
        wait.await.unwrap();
        tx.send_async(ProviderEvent::ContentComplete {
            block: ContentBlock::Text {
                text: "closed".into(),
            },
        })
        .await
        .unwrap();
        Err(ProviderError::Http {
            status: 500,
            body: "intact".into(),
        })
    });
    assert!(matches!(
        exchange.next().await,
        Some(ExchangeEvent::Provider(ProviderEvent::TextDelta { .. }))
    ));
    assert!(
        timeout(Duration::from_millis(10), exchange.next())
            .await
            .is_err()
    );
    assert_eq!(exchange.partial().display_text, "visible");
    release.send(()).unwrap();
    assert!(matches!(
        exchange.next().await,
        Some(ExchangeEvent::Provider(
            ProviderEvent::ContentComplete { .. }
        ))
    ));
    assert!(
        matches!(exchange.next().await, Some(ExchangeEvent::Finished(Err(ProviderError::Http { status:500, body }))) if body == "intact")
    );
    assert_eq!(exchange.partial().content.len(), 1);
    assert!(exchange.next().await.is_none());
}

#[tokio::test]
async fn retention_limits_never_skip_a_block_and_then_retain_later_content() {
    let mut exchange = InferenceExchange::new(|tx| async move {
        for text in ["x".repeat(16 * 1024 * 1024), "later".into()] {
            tx.send_async(ProviderEvent::ContentComplete {
                block: ContentBlock::Text { text },
            })
            .await
            .unwrap();
        }
        tx.send_async(ProviderEvent::Usage {
            usage: animist::provider::TokenUsage::builder()
                .input(0_u32)
                .output(9_u32)
                .cache_creation(0_u32)
                .cache_read(0_u32)
                .build(),
        })
        .await
        .unwrap();
        Err(ProviderError::Http {
            status: 500,
            body: String::new(),
        })
    });
    while !matches!(exchange.next().await, Some(ExchangeEvent::Finished(_))) {}
    assert!(exchange.partial().retention_limited);
    assert!(exchange.partial().content.is_empty());
    assert_eq!(exchange.partial().usage.as_ref().unwrap().output, 9);
}

#[tokio::test]
async fn incomplete_responses_items_block_later_checkpoints_without_masking_upstream_failures() {
    let (response, server) = fixture(sse(&[
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"partial","name":"lookup","arguments":"{\"x\":","status":"incomplete"}}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"closed","name":"lookup","arguments":"{}","status":"completed"}}),
        json!({"type":"response.output_text.delta","delta":"visible"}),
        json!({"type":"response.failed","response":{"error":{"opaque":"failure"},"usage":{"input_tokens":"invalid"}}}),
    ])).await;
    let mut exchange = exchange(Responses, response);
    loop {
        if let Some(ExchangeEvent::Finished(result)) = exchange.next().await {
            assert!(matches!(result, Err(ProviderError::Protocol {
                source: ProtocolError::OpenAI { source: animist::protocol::openai::OpenAIProtocolError::ResponseFailed { error } }
            }) if error == json!({"opaque":"failure"})));
            break;
        }
    }
    assert_eq!(exchange.partial().display_text, "visible");
    assert!(exchange.partial().content.is_empty());
    assert!(exchange.partial().usage.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn successful_terminal_usage_supersedes_the_last_queued_checkpoint() {
    use animist::{
        history::{Message, MessageKind, Role},
        provider::{StreamResponse, TokenUsage},
    };
    let usage = TokenUsage::builder()
        .input(10_u32)
        .output(2_u32)
        .cache_creation(0_u32)
        .cache_read(0_u32)
        .build();
    let expected = usage.clone();
    let mut exchange = InferenceExchange::new(|tx| async move {
        tx.send_async(ProviderEvent::Usage {
            usage: TokenUsage::default(),
        })
        .await
        .unwrap();
        Ok(StreamResponse::builder()
            .message(
                Message::builder()
                    .role(Role::Assistant)
                    .kind(MessageKind::Turn)
                    .content(Vec::new())
                    .build(),
            )
            .usage(usage)
            .input_transformations(Vec::new())
            .build())
    });
    assert!(matches!(
        exchange.next().await,
        Some(ExchangeEvent::Provider(ProviderEvent::Usage { .. }))
    ));
    assert!(matches!(
        exchange.next().await,
        Some(ExchangeEvent::Finished(Ok(_)))
    ));
    assert_eq!(exchange.partial().usage, Some(expected));
}
