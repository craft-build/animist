use animist::{
    protocol::{
        Protocol, ProtocolError, ProtocolInput, SamplingError,
        anthropic::{AnthropicOptions, AnthropicThinking, Messages},
        openai::{ChatCompletions, OpenAiOptions, Responses},
    },
    provider::{
        Model, ModelInfo, Provider, ProviderError, RequestOptions,
        anthropic::{Anthropic, AnthropicConfig},
        llama_cpp::{LlamaCpp, LlamaCppConfig},
        ollama::{Ollama, OllamaConfig},
        omlx::{Omlx, OmlxConfig},
        openai::{OpenAi, OpenAiConfig},
        synthetic::{Synthetic, SyntheticConfig},
        zai_coding_plan::{ZaiCodingPlan, ZaiCodingPlanConfig},
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Duration, timeout},
};

fn model(provider: Provider) -> Model {
    Model::builder()
        .id("explicit-model")
        .provider(provider)
        .info(ModelInfo::default())
        .build()
}

fn request(adapter: &str, options: &RequestOptions) -> Result<Value, ProtocolError> {
    let model = model(Provider::new(Ollama::new(OllamaConfig::default())));
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(&[][..])
        .system("")
        .tools(&[][..])
        .options(options)
        .build();
    match adapter {
        "chat" => ChatCompletions.request(input, &OpenAiOptions::default()),
        "responses" => Responses.request(input, &OpenAiOptions::default()),
        "messages" => Messages.request(
            input,
            &AnthropicOptions::builder()
                .max_tokens(128_u32)
                .fast(false)
                .build(),
        ),
        _ => unreachable!(),
    }
}

#[test]
fn sampling_wire_fixtures_preserve_omission_zero_and_explicit_empty_stops() {
    assert_eq!(RequestOptions::builder().build(), RequestOptions::default());
    for adapter in ["chat", "responses", "messages"] {
        let omitted = request(adapter, &RequestOptions::default()).unwrap();
        for field in [
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
            "stop",
            "stop_sequences",
        ] {
            assert!(omitted.get(field).is_none(), "{adapter}: {field}");
        }
        for (options, expected) in [
            (
                RequestOptions::builder().temperature(0.0).build(),
                json!({"temperature":0.0}),
            ),
            (
                RequestOptions::builder().top_p(1.0).build(),
                json!({"top_p":1.0}),
            ),
        ] {
            let body = request(adapter, &options).unwrap();
            for (field, value) in expected.as_object().unwrap() {
                assert_eq!(&body[field], value, "{adapter}: {field}");
            }
        }
    }
    for adapter in ["chat", "messages"] {
        let field = if adapter == "chat" {
            "stop"
        } else {
            "stop_sequences"
        };
        for stops in [vec![], vec!["END".to_owned(), "🦀".to_owned()]] {
            let options = RequestOptions::builder()
                .stop_sequences(stops.clone())
                .build();
            assert_eq!(request(adapter, &options).unwrap()[field], json!(stops));
        }
    }
    let options = RequestOptions::builder()
        .temperature(2.0)
        .top_p(0.0)
        .frequency_penalty(-2.0)
        .presence_penalty(2.0)
        .build();
    let body = request("chat", &options).unwrap();
    for (field, value) in [
        ("temperature", 2.0),
        ("top_p", 0.0),
        ("frequency_penalty", -2.0),
        ("presence_penalty", 2.0),
    ] {
        assert_eq!(body[field], value);
    }
}

#[test]
fn invalid_controls_return_structured_protocol_errors() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -2.01, 2.01] {
        for options in [
            RequestOptions::builder().frequency_penalty(value).build(),
            RequestOptions::builder().presence_penalty(value).build(),
        ] {
            assert!(matches!(
                request("chat", &options),
                Err(ProtocolError::Sampling {
                    source: SamplingError::OutOfRange { .. }
                })
            ));
        }
    }
    for adapter in ["chat", "responses", "messages"] {
        assert!(matches!(
            request(adapter, &RequestOptions::builder().top_p(1.01).build()),
            Err(ProtocolError::Sampling {
                source: SamplingError::OutOfRange {
                    control: "top_p",
                    ..
                }
            })
        ));
    }
    for adapter in ["chat", "responses", "messages"] {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 2.01] {
            for options in [
                RequestOptions::builder().temperature(value).build(),
                RequestOptions::builder().top_p(value).build(),
            ] {
                assert!(
                    matches!(
                        request(adapter, &options),
                        Err(ProtocolError::Sampling {
                            source: SamplingError::OutOfRange { .. }
                        })
                    ),
                    "{adapter}: {value}"
                );
            }
        }
    }
    for adapter in ["responses", "messages"] {
        for options in [
            RequestOptions::builder().frequency_penalty(0.0).build(),
            RequestOptions::builder().presence_penalty(0.0).build(),
        ] {
            assert!(matches!(
                request(adapter, &options),
                Err(ProtocolError::Sampling {
                    source: SamplingError::Unsupported { .. }
                })
            ));
        }
    }
    for stops in [vec![], vec!["END".into()]] {
        assert!(matches!(
            request(
                "responses",
                &RequestOptions::builder().stop_sequences(stops).build()
            ),
            Err(ProtocolError::Sampling {
                source: SamplingError::Unsupported {
                    control: "stop_sequences",
                    ..
                }
            })
        ));
    }
    for adapter in ["chat", "messages"] {
        assert!(matches!(
            request(
                adapter,
                &RequestOptions::builder()
                    .stop_sequences(vec![String::new()])
                    .build()
            ),
            Err(ProtocolError::Sampling {
                source: SamplingError::EmptyStop { index: 0, .. }
            })
        ));
    }
    assert!(matches!(
        request(
            "chat",
            &RequestOptions::builder()
                .stop_sequences(vec!["x".into(); 5])
                .build()
        ),
        Err(ProtocolError::Sampling {
            source: SamplingError::TooManyStops {
                count: 5,
                max: 4,
                ..
            }
        })
    ));
    assert!(matches!(
        request(
            "messages",
            &RequestOptions::builder()
                .temperature(0.5)
                .top_p(0.9)
                .build()
        ),
        Err(ProtocolError::Sampling {
            source: SamplingError::Conflict { .. }
        })
    ));
    assert!(matches!(
        request(
            "messages",
            &RequestOptions::builder().temperature(1.01).build()
        ),
        Err(ProtocolError::Sampling {
            source: SamplingError::OutOfRange { max: 1.0, .. }
        })
    ));
    for thinking in [
        AnthropicThinking::Adaptive,
        AnthropicThinking::Enabled {
            budget_tokens: 1024,
        },
    ] {
        let model = model(Provider::new(Ollama::new(OllamaConfig::default())));
        let options = RequestOptions::builder().temperature(0.5).build();
        let input = ProtocolInput::builder()
            .model(&model)
            .messages(&[][..])
            .system("")
            .tools(&[][..])
            .options(&options)
            .build();
        let wire = AnthropicOptions::builder()
            .max_tokens(2048_u32)
            .thinking(thinking)
            .fast(false)
            .build();
        assert!(matches!(
            Messages.request(input, &wire),
            Err(ProtocolError::Sampling {
                source: SamplingError::Conflict {
                    other: "thinking",
                    ..
                }
            })
        ));
    }
}

#[tokio::test]
async fn concrete_providers_forward_supported_controls_and_preserve_upstream_errors() {
    for name in [
        "openai-chat",
        "openai-responses",
        "anthropic",
        "synthetic",
        "ollama",
        "llama.cpp",
        "omlx",
        "zai",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let provider = match name {
            "openai-chat" | "openai-responses" => Provider::new(
                OpenAi::new(
                    OpenAiConfig::builder()
                        .api_key("test")
                        .base_url(url)
                        .protocol(if name == "openai-chat" {
                            animist::provider::openai::Protocol::ChatCompletions
                        } else {
                            animist::provider::openai::Protocol::Responses
                        })
                        .build(),
                )
                .unwrap(),
            ),
            "anthropic" => Provider::new(
                Anthropic::new(
                    AnthropicConfig::builder()
                        .api_key("test")
                        .base_url(url)
                        .build(),
                )
                .unwrap(),
            ),
            "synthetic" => Provider::new(
                Synthetic::new(
                    SyntheticConfig::builder()
                        .api_key("test")
                        .base_url(url)
                        .build(),
                )
                .unwrap(),
            ),
            "ollama" => Provider::new(Ollama::new(OllamaConfig::builder().base_url(url).build())),
            "llama.cpp" => Provider::new(
                LlamaCpp::new(LlamaCppConfig::builder().base_url(url).build()).unwrap(),
            ),
            "omlx" => {
                Provider::new(Omlx::new(OmlxConfig::builder().base_url(url).build()).unwrap())
            }
            "zai" => Provider::new(
                ZaiCodingPlan::new(
                    ZaiCodingPlanConfig::builder()
                        .api_key("test")
                        .base_url(url)
                        .build(),
                )
                .unwrap(),
            ),
            _ => unreachable!(),
        };
        let model = model(provider.clone());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut buffer = [0; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap().to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length])
                            .unwrap();
                    }
                }
            };
            let error =
                r#"{"error":{"code":"model_sampling_unsupported","detail":{"retained":true}}}"#;
            socket.write_all(format!("HTTP/1.1 400 Bad Request\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{error}", error.len()).as_bytes()).await.unwrap();
            body
        });
        let mut options = RequestOptions::builder()
            .temperature(0.5)
            .max_output_tokens(128_u32)
            .build();
        if name != "anthropic" {
            options.top_p = Some(0.9);
        }
        if !matches!(name, "anthropic" | "openai-responses" | "zai") {
            options.frequency_penalty = Some(0.25);
            options.presence_penalty = Some(-0.5);
        }
        if name != "openai-responses" {
            options.stop_sequences = Some(vec!["END".into()]);
        }
        let (tx, _rx) = flume::unbounded();
        let error = timeout(
            Duration::from_secs(5),
            provider.stream_message(&model, &[], "", &[], &tx, options),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(error, ProviderError::Http { status: 400, ref body } if body.contains("\"retained\":true")),
            "{name}: {error}"
        );
        let body = server.await.unwrap();
        assert_eq!(body["temperature"], 0.5, "{name}");
        if name != "anthropic" {
            assert_eq!(body["top_p"], 0.9, "{name}");
        }
        if !matches!(name, "anthropic" | "openai-responses" | "zai") {
            assert_eq!(body["frequency_penalty"], 0.25, "{name}");
            assert_eq!(body["presence_penalty"], -0.5, "{name}");
        }
        if name != "openai-responses" {
            assert_eq!(
                body[if name == "anthropic" {
                    "stop_sequences"
                } else {
                    "stop"
                }],
                json!(["END"]),
                "{name}"
            );
        }
    }
}

#[tokio::test]
async fn coding_plan_rejects_incompatible_controls_before_transport() {
    let provider = Provider::new(
        ZaiCodingPlan::new(
            ZaiCodingPlanConfig::builder()
                .api_key("test")
                .base_url("http://127.0.0.1:1")
                .build(),
        )
        .unwrap(),
    );
    let model = model(provider.clone());
    let (tx, _rx) = flume::unbounded();
    for (options, expected) in [
        (RequestOptions::builder().temperature(1.1).build(), "range"),
        (RequestOptions::builder().top_p(0.0).build(), "range"),
        (
            RequestOptions::builder().frequency_penalty(0.0).build(),
            "unsupported",
        ),
        (
            RequestOptions::builder().presence_penalty(0.0).build(),
            "unsupported",
        ),
        (
            RequestOptions::builder()
                .stop_sequences(vec!["a".into(), "b".into()])
                .build(),
            "stops",
        ),
    ] {
        let error = provider
            .stream_message(&model, &[], "", &[], &tx, options)
            .await
            .unwrap_err();
        match expected {
            "range" => assert!(matches!(error, ProviderError::SamplingOutOfRange { .. })),
            "unsupported" => assert!(matches!(
                error,
                ProviderError::UnsupportedSamplingControl { .. }
            )),
            "stops" => assert!(matches!(error, ProviderError::TooManyStopSequences { .. })),
            _ => unreachable!(),
        }
    }
}
