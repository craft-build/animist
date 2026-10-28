#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use animist::{
    protocol::{
        DeciderProtocol, ProtocolError,
        system_one::{SystemOne, SystemOneError},
    },
    provider::{
        BooleanCriteria, DeciderModel, DeciderProvider, DecisionAnswer, DecisionQuestion,
        DecisionRequest, Provider, ProviderError,
        llama_cpp::{LlamaCpp, LlamaCppConfig},
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};

const FIXTURE: &str = include_str!("fixtures/system-one.json");

struct LocalDecider;

#[async_trait::async_trait]
impl animist::provider::DeciderProviderImpl for LocalDecider {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn decide(
        &self,
        model: &DeciderModel,
        request: &DecisionRequest,
    ) -> animist::provider::Result<animist::provider::DecisionResponse> {
        let protocol = LocalDecider;
        let response = protocol.request(model, request)?;
        Ok(protocol.assemble(response, request).await?)
    }

    async fn refresh_auth(&self) -> animist::provider::Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> animist::provider::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl DeciderProtocol for LocalDecider {
    type Request = String;
    type Response = String;

    fn request(
        &self,
        model: &DeciderModel,
        _request: &DecisionRequest,
    ) -> animist::protocol::Result<Self::Request> {
        Ok(model.id.clone())
    }

    async fn assemble(
        &self,
        model: Self::Response,
        request: &DecisionRequest,
    ) -> animist::protocol::Result<animist::provider::DecisionResponse> {
        Ok(animist::provider::DecisionResponse::builder()
            .model(model)
            .answers(
                request
                    .questions
                    .keys()
                    .map(|id| (id.clone(), DecisionAnswer::Boolean { probability: 0.5 }))
                    .collect::<BTreeMap<_, _>>(),
            )
            .build())
    }
}

#[tokio::test]
async fn downstream_decider_and_non_http_protocol_use_public_types() {
    let provider = DeciderProvider::new(LocalDecider);
    let request = DecisionRequest::builder()
        .state(json!("state"))
        .questions(BTreeMap::from([(
            "test".into(),
            DecisionQuestion::Boolean {
                instructions: json!("test?"),
                criteria: None,
            },
        )]))
        .build();
    let model = DeciderModel::builder().id("test").provider("local").build();
    let response = provider.decide(&model, &request).await.unwrap();
    assert_eq!(provider.name(), "local");
    assert_eq!(
        response.answers["test"],
        DecisionAnswer::Boolean { probability: 0.5 }
    );
}

fn model() -> DeciderModel {
    DeciderModel::builder()
        .id("router/decision")
        .provider("llama-cpp")
        .build()
}

fn request() -> DecisionRequest {
    DecisionRequest::builder()
        .state(json!({"message": "I was charged twice."}))
        .questions(BTreeMap::from([
            (
                "route".into(),
                DecisionQuestion::Choice {
                    instructions: json!("Which team should handle this?"),
                    options: BTreeMap::from([
                        ("billing".into(), None),
                        ("shipping".into(), None),
                        ("technical".into(), None),
                    ]),
                },
            ),
            (
                "angry".into(),
                DecisionQuestion::Boolean {
                    instructions: json!("Is the customer angry?"),
                    criteria: Some(
                        BooleanCriteria::builder()
                            .when_true(json!({"description": "angry"}))
                            .when_false(json!(["calm"]))
                            .build(),
                    ),
                },
            ),
            (
                "urgency".into(),
                DecisionQuestion::Score {
                    instructions: json!(["How urgent is this?"]),
                    levels: vec![
                        json!("can wait"),
                        json!("this week"),
                        json!("today"),
                        json!("right now"),
                    ],
                },
            ),
        ]))
        .build()
}

async fn read_request(socket: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8(bytes[..end].to_vec())
                .unwrap()
                .to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return (
                    headers,
                    serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                );
            }
        }
    }
}

async fn server(status: &str, body: &str) -> (String, JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/prefix/v1/", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        socket.write_all(response.as_bytes()).await.unwrap();
        request
    });
    (url, task)
}

fn provider(url: &str, authenticated: bool) -> DeciderProvider {
    DeciderProvider::new(
        LlamaCpp::new(
            LlamaCppConfig::builder()
                .base_url(url)
                .maybe_api_key(authenticated.then(|| "test-key".to_owned()))
                .build(),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn direct_score_assembly_rejects_empty_levels_without_panicking() {
    let body = json!({
        "model": "fixture",
        "answers": {
            "score": {
                "type": "score",
                "score": 0.0,
                "legend": {},
                "probabilities": {},
                "confidence": 0.9
            }
        }
    });
    let (url, task) = server("200 OK", &body.to_string()).await;
    let response = reqwest::Client::new()
        .post(url)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let request = DecisionRequest::builder()
        .state(Value::Null)
        .questions(BTreeMap::from([(
            "score".into(),
            DecisionQuestion::Score {
                instructions: json!("score"),
                levels: vec![],
            },
        )]))
        .build();
    assert!(matches!(
        SystemOne.assemble(response, &request).await,
        Err(ProtocolError::SystemOne {
            source: SystemOneError::InvalidScore { question, levels: 0, .. }
        }) if question == "score"
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn llama_cpp_routes_typed_decisions_and_preserves_reported_results() {
    for authenticated in [false, true] {
        let (url, task) = server("200 OK", FIXTURE).await;
        let concrete = Arc::new(
            LlamaCpp::new(
                LlamaCppConfig::builder()
                    .base_url(url)
                    .maybe_api_key(authenticated.then(|| "test-key".to_owned()))
                    .build(),
            )
            .unwrap()
            .with_client(
                reqwest::Client::builder()
                    .default_headers(
                        [(
                            reqwest::header::HeaderName::from_static("x-test-client"),
                            reqwest::header::HeaderValue::from_static("shared"),
                        )]
                        .into_iter()
                        .collect(),
                    )
                    .build()
                    .unwrap(),
            ),
        );
        let cloned = Arc::clone(&concrete);
        let decider = DeciderProvider::from_arc(cloned);
        let shared: Arc<dyn animist::provider::ProviderImpl + Send + Sync> = concrete;
        let chat = Provider::from_arc(shared);
        assert_eq!(
            Arc::as_ptr(chat.inner()).cast::<()>(),
            Arc::as_ptr(decider.inner()).cast::<()>()
        );
        assert_eq!(decider.name(), "llama-cpp");
        let response = decider.clone().decide(&model(), &request()).await.unwrap();
        assert_eq!(response.model, "openjev");
        assert_eq!(
            response.answers["angry"],
            DecisionAnswer::Boolean {
                probability: 0.6328
            }
        );
        assert!(
            matches!(&response.answers["route"], DecisionAnswer::Choice { choice, probabilities, confidence }
            if choice == "billing" && (probabilities["shipping"] - 0.0001).abs() < 1e-9 && (*confidence - 0.9997).abs() < 1e-9)
        );
        assert!(
            matches!(&response.answers["urgency"], DecisionAnswer::Score { score, legend, probabilities, .. }
            if (*score - 2.0858).abs() < 1e-9 && legend[&2] == "today" && (probabilities[&2] - 0.6753).abs() < 1e-9)
        );
        let usage = response.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (239, 0));
        decider.refresh_auth().await.unwrap();
        decider.reload_auth().await.unwrap();
        let (headers, body) = task.await.unwrap();
        assert!(headers.starts_with("post /prefix/v1/systemone "));
        assert!(headers.contains("accept: application/json"));
        assert!(headers.contains("x-test-client: shared"));
        assert_eq!(
            headers.contains("authorization: bearer test-key"),
            authenticated
        );
        assert_eq!(body["model"], "router/decision");
        assert_eq!(body["state"], request().state);
        assert_eq!(body["questions"]["angry"]["type"], "noul");
        assert_eq!(
            body["questions"]["angry"]["criteria"]["true"],
            json!({"description":"angry"})
        );
        assert_eq!(
            body["questions"]["urgency"]["criteria"],
            json!(["can wait", "this week", "today", "right now"])
        );
        assert_eq!(
            body["questions"]["route"]["criteria"]["billing"],
            Value::Null
        );
        assert!(body.get("files").is_none());
        assert!(body.get("stream").is_none());
    }
}

#[test]
fn translation_preserves_structured_state_and_inline_media() {
    for state in [json!("state"), json!({"key":1}), json!([1, 2]), Value::Null] {
        let mut request = request();
        request.state = state.clone();
        request.files = vec![
            "data:image/png;base64,YQ==".into(),
            "data:audio/wav;base64,Yg==".into(),
        ];
        let body = SystemOne.request(&model(), &request).unwrap();
        assert_eq!(body["state"], state);
        assert_eq!(body["files"], json!(request.files));
    }
}

#[tokio::test]
async fn invalid_requests_fail_before_http() {
    let provider = provider("http://127.0.0.1:1", false);
    let mut wrong_model = model();
    wrong_model.provider = "other".into();
    assert!(matches!(
        provider.decide(&wrong_model, &request()).await,
        Err(ProviderError::ModelProviderMismatch { .. })
    ));
    let mut empty = request();
    empty.questions.clear();
    assert!(matches!(
        provider.decide(&model(), &empty).await,
        Err(ProviderError::Protocol {
            source: ProtocolError::SystemOne {
                source: SystemOneError::EmptyQuestions { .. }
            }
        })
    ));
    let mut scalar = request();
    scalar.state = json!(42);
    assert!(matches!(
        provider.decide(&model(), &scalar).await,
        Err(ProviderError::Protocol {
            source: ProtocolError::SystemOne {
                source: SystemOneError::InvalidContent { .. }
            }
        })
    ));
    let mut no_options = request();
    no_options.questions.insert(
        "route".into(),
        DecisionQuestion::Choice {
            instructions: json!("route?"),
            options: BTreeMap::new(),
        },
    );
    assert!(matches!(
        provider.decide(&model(), &no_options).await,
        Err(ProviderError::Protocol {
            source: ProtocolError::SystemOne {
                source: SystemOneError::EmptyCriteria { .. }
            }
        })
    ));
    let mut remote_media = request();
    remote_media.files = vec!["https://example.com/image.png".into()];
    assert!(matches!(
        provider.decide(&model(), &remote_media).await,
        Err(ProviderError::Protocol {
            source: ProtocolError::SystemOne {
                source: SystemOneError::InvalidFile { index: 0, .. }
            }
        })
    ));
}

#[tokio::test]
async fn missing_usage_is_unknown_and_upstream_output_usage_is_preserved() {
    for usage in [None, Some(json!({"input_tokens": 10, "output_tokens": 12}))] {
        let mut body: Value = serde_json::from_str(FIXTURE).unwrap();
        body.as_object_mut().unwrap().remove("usage");
        if let Some(usage) = &usage {
            body["usage"] = usage.clone();
        }
        let (url, task) = server("200 OK", &body.to_string()).await;
        let response = provider(&url, false)
            .decide(&model(), &request())
            .await
            .unwrap();
        assert_eq!(
            response.usage.map(|usage| usage.output_tokens),
            usage.map(|_| 12)
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn malformed_results_return_specific_protocol_errors() {
    let mutations = [
        ("missing", "angry", Value::Null),
        ("unexpected", "other", json!({"type":"noul","noul":0.5})),
        (
            "type",
            "angry",
            json!({"type":"choice","choice":"x","probabilities":{"x":1.0},"confidence":1.0}),
        ),
        ("probability", "angry", json!({"type":"noul","noul":1.1})),
        (
            "probability",
            "route",
            json!({"type":"choice","choice":"billing","probabilities":{"billing":1.0,"shipping":0.0,"technical":0.0},"confidence":-0.1}),
        ),
        (
            "sum",
            "route",
            json!({"type":"choice","choice":"billing","probabilities":{"billing":0.2,"shipping":0.1,"technical":0.1},"confidence":0.2}),
        ),
        (
            "keys",
            "route",
            json!({"type":"choice","choice":"billing","probabilities":{"billing":1.0},"confidence":1.0}),
        ),
        (
            "choice",
            "route",
            json!({"type":"choice","choice":"other","probabilities":{"billing":1.0},"confidence":1.0}),
        ),
        (
            "score",
            "urgency",
            json!({"type":"score","score":4.0,"legend":{},"probabilities":{},"confidence":1.0}),
        ),
        (
            "legend",
            "urgency",
            json!({"type":"score","score":1.0,"legend":{},"probabilities":{},"confidence":1.0}),
        ),
        ("decode", "angry", json!({"type":"unknown"})),
        (
            "decode",
            "urgency",
            json!({"type":"score","score":0.0,"legend":{"00":"first"},"probabilities":{"0":1.0},"confidence":1.0}),
        ),
    ];
    for (expected, id, answer) in mutations {
        let mut body: Value = serde_json::from_str(FIXTURE).unwrap();
        if answer.is_null() {
            body["answers"].as_object_mut().unwrap().remove(id);
        } else {
            body["answers"][id] = answer;
        }
        let (url, task) = server("200 OK", &body.to_string()).await;
        let error = provider(&url, false)
            .decide(&model(), &request())
            .await
            .unwrap_err();
        let ProviderError::Protocol {
            source: ProtocolError::SystemOne { source },
        } = error
        else {
            panic!("unexpected error: {error:?}")
        };
        assert!(
            matches!(
                (expected, &source),
                ("missing", SystemOneError::MissingAnswer { .. })
                    | ("unexpected", SystemOneError::UnexpectedAnswer { .. })
                    | ("type", SystemOneError::AnswerTypeMismatch { .. })
                    | ("probability", SystemOneError::InvalidProbability { .. })
                    | ("sum", SystemOneError::ProbabilitySum { .. })
                    | ("keys", SystemOneError::ProbabilityKeys { .. })
                    | ("choice", SystemOneError::UnknownChoice { .. })
                    | ("score", SystemOneError::InvalidScore { .. })
                    | ("legend", SystemOneError::LegendKeys { .. })
                    | ("decode", SystemOneError::DecodeResponse { .. })
            ),
            "{expected}: {source:?}"
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn score_levels_are_numeric_and_model_specific_limits_are_not_inferred() {
    let levels: Vec<Value> = (0..12)
        .map(|index| json!(format!("level {index}")))
        .collect();
    let legend: BTreeMap<u32, Value> = levels
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, value)| (u32::try_from(index).expect("index fits u32"), value))
        .collect();
    let probabilities: BTreeMap<u32, f64> = (0..12).map(|index| (index, 1.0 / 12.0)).collect();
    let request = DecisionRequest::builder()
        .state(json!("state"))
        .questions(BTreeMap::from([(
            "rank".into(),
            DecisionQuestion::Score {
                instructions: json!("rank?"),
                levels,
            },
        )]))
        .build();
    assert_eq!(
        SystemOne.request(&model(), &request).unwrap()["questions"]["rank"]["criteria"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
    let body = json!({
        "model": "h2o-lightning",
        "answers": {"rank": {
            "type": "score", "score": 5.5,
            "legend": legend, "probabilities": probabilities, "confidence": 0.0
        }}
    });
    let (url, task) = server("200 OK", &body.to_string()).await;
    let response = provider(&url, false)
        .decide(&model(), &request)
        .await
        .unwrap();
    assert!(
        matches!(&response.answers["rank"], DecisionAnswer::Score { score, legend, probabilities, .. }
        if (*score - 5.5).abs() < 1e-9 && legend[&10] == "level 10" && probabilities.len() == 12)
    );
    task.await.unwrap();
}

#[tokio::test]
async fn http_errors_preserve_status_and_body() {
    for status in ["400 Bad Request", "501 Not Implemented"] {
        let body = r#"{"error":{"message":"not a decision model","code":501}}"#;
        let (url, task) = server(status, body).await;
        let error = provider(&url, true)
            .decide(&model(), &request())
            .await
            .unwrap_err();
        assert!(
            matches!(error, ProviderError::Http { status: 400 | 501, body: payload ,
            ..} if payload == body)
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn dropping_decision_future_cancels_the_http_exchange() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = provider(
        &format!("http://{}/v1", listener.local_addr().unwrap()),
        false,
    );
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"model\":").await.unwrap();
        ready_tx.send(()).unwrap();
        let mut byte = [0];
        match timeout(Duration::from_secs(5), socket.read(&mut byte))
            .await
            .unwrap()
        {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("expected a closed connection, got {result:?}"),
        }
    });
    let model = model();
    let request = request();
    let mut future = Box::pin(provider.decide(&model, &request));
    tokio::select! {
        result = &mut future => panic!("request unexpectedly finished: {result:?}"),
        _ = ready_rx => {}
    }
    drop(future);
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
