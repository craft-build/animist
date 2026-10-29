use std::path::{Path, PathBuf};

use animist::{
    catalog::{Catalog, ProviderCatalog},
    history::{ContentBlock, Message, MessageKind, Role},
    provider::{
        Effort, Model, ModelInfo, Provider, ProviderEvent, RequestOptions, StopReason,
        ThinkingConfig, Tool,
    },
};
use serde_json::json;
use snafu::{OptionExt, ResultExt, Snafu, ensure};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub(crate) enum Error {
    #[snafu(transparent)]
    Provider {
        source: animist::provider::ProviderError,
    },
    #[snafu(transparent)]
    Catalog { source: animist::catalog::Error },
    #[snafu(display("Output failed: {source}"))]
    Output { source: std::io::Error },
    #[snafu(display("Unknown option {option}; use --help"))]
    UnknownOption { option: String },
    #[snafu(display("Missing value for {option}"))]
    MissingValue { option: String },
    #[snafu(display("Unknown thinking setting {value}; use --help"))]
    InvalidThinking { value: String },
    #[snafu(display("Invalid output token limit {value}: {source}"))]
    InvalidTokenLimit {
        value: String,
        source: std::num::ParseIntError,
    },
    #[snafu(display("Output token limit must be positive"))]
    ZeroTokenLimit,
    #[snafu(display("Catalog does not contain provider {provider}"))]
    MissingProvider { provider: String },
    #[snafu(display("Model {model} was not listed by {provider}; use --list-models"))]
    MissingModel { model: String, provider: String },
    #[snafu(display("Unexpected tool {name}; this example only implements get_test_value"))]
    UnexpectedTool { name: String },
    #[snafu(display("get_test_value requires an empty object, received {input}"))]
    InvalidToolInput { input: serde_json::Value },
    #[snafu(display("The tool test did not call get_test_value and report animist-ok"))]
    ToolTestFailed,
    #[snafu(display("Response was not a completed text answer (stop reason {reason:?})"))]
    IncompleteResponse { reason: Option<StopReason> },
    #[snafu(display("Tool test exceeded three requests"))]
    TooManyRequests,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arguments {
    pub model: String,
    pub prompt: String,
    pub thinking: ThinkingConfig,
    pub max_tokens: u32,
    pub tool_test: bool,
    pub list_models: bool,
    pub catalog: Option<PathBuf>,
    pub base_url: Option<String>,
}

impl Arguments {
    pub(crate) async fn parse(example: &str, default_model: &str) -> Result<Option<Self>, Error> {
        let values = std::env::args().skip(1).collect::<Vec<_>>();
        if values
            .iter()
            .any(|value| value == "--help" || value == "-h")
        {
            let features = if example == "bedrock" {
                " --features bedrock"
            } else {
                ""
            };
            let credentials = match example {
                "bedrock" => {
                    "AWS credential chain; AWS_PROFILE and AWS_REGION select profile/region"
                }
                "synthetic" => "SYNTHETIC_API_KEY",
                _ => "ZHIPU_API_KEY (Coding Plan key)",
            };
            let text = format!(
                "cargo run{features} --example {example} -- [OPTIONS]\n\
                 Credentials: {credentials}\n\
                 --model ID               Model ID (default: {default_model})\n\
                 --prompt TEXT            Prompt (default: a short connection test)\n\
                 --thinking MODE          adaptive (default), off, minimal, low, medium, high, xhigh, max\n\
                 --max-tokens N           Output limit per request (default: 4096)\n\
                 --tool-test              Test a harmless local tool and its history replay\n\
                 --list-models            List available model metadata without inference\n\
                 --catalog PATH           Load saved models.dev JSON instead of fetching metadata\n\
                 --base-url URL           Override the provider API base (for local mock testing)\n\
                 --help                   Print help without credentials or network access\n"
            );
            tokio::io::stdout()
                .write_all(text.as_bytes())
                .await
                .context(OutputSnafu)?;
            return Ok(None);
        }
        Ok(Some(Self::from_values(default_model, values)?))
    }

    fn from_values(default_model: &str, values: Vec<String>) -> Result<Self, Error> {
        let mut args = Self {
            model: default_model.into(),
            prompt: "Reply with one short sentence confirming that you received this test.".into(),
            thinking: ThinkingConfig::Adaptive,
            max_tokens: 4096,
            tool_test: false,
            list_models: false,
            catalog: None,
            base_url: None,
        };
        let mut values = values.into_iter();
        while let Some(option) = values.next() {
            match option.as_str() {
                "--tool-test" => args.tool_test = true,
                "--list-models" => args.list_models = true,
                "--model" | "--prompt" | "--thinking" | "--max-tokens" | "--catalog"
                | "--base-url" => {
                    let value = values
                        .next()
                        .filter(|value| !value.starts_with("--"))
                        .context(MissingValueSnafu { option: &option })?;
                    match option.as_str() {
                        "--model" => args.model = value,
                        "--prompt" => args.prompt = value,
                        "--catalog" => args.catalog = Some(value.into()),
                        "--base-url" => args.base_url = Some(value),
                        "--max-tokens" => {
                            args.max_tokens =
                                value.parse().context(InvalidTokenLimitSnafu { value })?;
                            ensure!(args.max_tokens > 0, ZeroTokenLimitSnafu);
                        }
                        "--thinking" => {
                            args.thinking = match value.as_str() {
                                "off" => ThinkingConfig::Off,
                                "adaptive" => ThinkingConfig::Adaptive,
                                "minimal" => ThinkingConfig::Effort(Effort::Minimal),
                                "low" => ThinkingConfig::Effort(Effort::Low),
                                "medium" => ThinkingConfig::Effort(Effort::Medium),
                                "high" => ThinkingConfig::Effort(Effort::High),
                                "xhigh" => ThinkingConfig::Effort(Effort::XHigh),
                                "max" => ThinkingConfig::Effort(Effort::Max),
                                _ => return InvalidThinkingSnafu { value }.fail(),
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                _ => return UnknownOptionSnafu { option }.fail(),
            }
        }
        Ok(args)
    }
}

pub(crate) async fn load_catalog(
    provider: &str,
    path: Option<&Path>,
) -> Result<ProviderCatalog, Error> {
    let mut catalog = match path {
        Some(path) => Catalog::load(path).await?,
        None => Catalog::fetch(&reqwest::Client::new()).await?,
    };
    catalog
        .providers
        .remove(provider)
        .context(MissingProviderSnafu { provider })
}

pub(crate) async fn run(
    provider: Provider,
    models: Vec<ModelInfo>,
    args: Arguments,
) -> Result<(), Error> {
    let mut stdout = tokio::io::stdout();
    if args.list_models {
        for info in models {
            stdout
                .write_all(
                    format!(
                        "{}  thinking={}  efforts={:?}\n",
                        info.ids.join(", "),
                        info.features.thinking.supported,
                        info.features.thinking.efforts
                    )
                    .as_bytes(),
                )
                .await
                .context(OutputSnafu)?;
        }
        return Ok(());
    }
    let info = models
        .into_iter()
        .find(|info| info.ids.contains(&args.model))
        .context(MissingModelSnafu {
            model: &args.model,
            provider: provider.name(),
        })?;
    let model = Model::builder()
        .id(&args.model)
        .provider(provider.clone())
        .info(info)
        .build();
    stdout
        .write_all(format!("Provider: {}  Model: {}\n", provider.name(), model.id).as_bytes())
        .await
        .context(OutputSnafu)?;
    let tools = if args.tool_test {
        vec![
            Tool::builder()
                .name("get_test_value")
                .description("Returns the test value. Call once with an empty object.")
                .input_schema(json!({"type":"object","properties":{},"additionalProperties":false}))
                .build(),
        ]
    } else {
        vec![]
    };
    let system = if args.tool_test {
        "This is a provider integration test. You must call get_test_value once, then include its returned value in your final text answer. Do not invent its value."
    } else {
        "You are participating in a short provider connection test."
    };
    let mut messages = vec![
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::Text { text: args.prompt }])
            .build(),
    ];
    let mut called_tool = false;
    for turn in 1..=3 {
        let (tx, rx) = flume::bounded(64);
        let request = async {
            let result = provider
                .stream_message(
                    &model,
                    &messages,
                    system,
                    &tools,
                    &tx,
                    RequestOptions::builder()
                        .thinking(args.thinking.clone())
                        .fast(false)
                        .max_output_tokens(args.max_tokens)
                        .build(),
                )
                .await;
            drop(tx);
            result.map_err(Error::from)
        };
        let display = async {
            let mut previous = "";
            while let Ok(event) = rx.recv_async().await {
                let (label, text) = match event {
                    ProviderEvent::ContentComplete { .. } | ProviderEvent::Usage { .. } => continue,
                    ProviderEvent::TextDelta { text } => ("text", text),
                    ProviderEvent::ThinkingDelta { text } => ("thinking", text),
                    ProviderEvent::ToolUseStart { id, name } => ("tool", format!("{name} ({id})")),
                    ProviderEvent::ToolInputDelta { id, partial_json } => {
                        ("tool-input", format!("{id}: {partial_json}\n"))
                    }
                    ProviderEvent::PromptProgress {
                        processed,
                        total,
                        cache,
                    } => ("progress", format!("{processed}/{total}, cache={cache}\n")),
                };
                if label != previous {
                    stdout
                        .write_all(format!("\n[{label}]\n").as_bytes())
                        .await
                        .context(OutputSnafu)?;
                    previous = label;
                }
                stdout
                    .write_all(text.as_bytes())
                    .await
                    .context(OutputSnafu)?;
                stdout.flush().await.context(OutputSnafu)?;
            }
            Ok::<_, Error>(())
        };
        let (response, ()) = tokio::try_join!(request, display)?;
        stdout.write_all(format!(
            "\n[request {turn}] stop={:?} input={} output={} cache_read={} cache_creation={}\n",
            response.stop_reason, response.usage.input, response.usage.output,
            response.usage.cache_read, response.usage.cache_creation,
        ).as_bytes()).await.context(OutputSnafu)?;
        let mut results = Vec::new();
        let mut text = String::new();
        for block in &response.message.content {
            match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    ensure!(
                        args.tool_test && name == "get_test_value",
                        UnexpectedToolSnafu { name }
                    );
                    ensure!(
                        input.as_object().is_some_and(serde_json::Map::is_empty),
                        InvalidToolInputSnafu {
                            input: input.clone()
                        }
                    );
                    called_tool = true;
                    results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        name: Some(name.clone()),
                        content: r#"{"value":"animist-ok"}"#.into(),
                        is_error: false,
                        loaded_tools: vec![],
                    });
                }
                ContentBlock::Text { text: fragment } => text.push_str(fragment),
                _ => {}
            }
        }
        if results.is_empty() {
            ensure!(
                response.stop_reason == Some(StopReason::EndTurn) && !text.is_empty(),
                IncompleteResponseSnafu {
                    reason: response.stop_reason
                }
            );
            ensure!(
                !args.tool_test || (called_tool && text.contains("animist-ok")),
                ToolTestFailedSnafu
            );
            stdout.write_all(b"\nPASS\n").await.context(OutputSnafu)?;
            return Ok(());
        }
        ensure!(
            response.stop_reason == Some(StopReason::ToolUse),
            IncompleteResponseSnafu {
                reason: response.stop_reason
            }
        );
        messages.push(response.message);
        messages.push(
            Message::builder()
                .role(Role::User)
                .kind(MessageKind::Turn)
                .content(results)
                .build(),
        );
    }
    TooManyRequestsSnafu.fail()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_adaptive_thinking_and_a_small_output_limit() {
        let args = Arguments::from_values("test-model", vec![]).unwrap();
        assert_eq!(args.model, "test-model");
        assert_eq!(args.thinking, ThinkingConfig::Adaptive);
        assert_eq!(args.max_tokens, 4096);
        assert!(!args.tool_test);
    }

    #[test]
    fn parses_model_prompt_thinking_and_tool_test() {
        let args = Arguments::from_values(
            "default",
            [
                "--model",
                "custom",
                "--prompt",
                "hello",
                "--thinking",
                "high",
                "--max-tokens",
                "1024",
                "--tool-test",
            ]
            .map(str::to_owned)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(args.model, "custom");
        assert_eq!(args.prompt, "hello");
        assert_eq!(args.thinking, ThinkingConfig::Effort(Effort::High));
        assert_eq!(args.max_tokens, 1024);
        assert!(args.tool_test);
    }

    #[test]
    fn rejects_unknown_missing_and_invalid_arguments() {
        for values in [
            vec!["--unknown"],
            vec!["--model"],
            vec!["--thinking", "unknown"],
            vec!["--max-tokens", "0"],
            vec!["--max-tokens", "no"],
        ] {
            assert!(
                Arguments::from_values("default", values.into_iter().map(str::to_owned).collect())
                    .is_err()
            );
        }
    }
}
