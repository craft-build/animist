# Animist

Model inference and agent-building primitives for the Craft ecosystem, shared by
[Craft](https://github.com/craft-build) and Revelry.

Animist is an async Rust library with a common provider interface, streaming
responses, model catalog metadata, and a shared agent runtime. Applications supply
tools and policy through typed implementations and ordered agent layers.

> [!WARNING]
> Animist is pre-1.0. Releases contain breaking changes; each release's
> [changelog](CHANGELOG.md) will include notices of any breaking changes.

## Features

- **Shared agents** with typed tools, ordered linked layers, bounded inference
  loops, cancellation, events, and revision-preserving tool results.
  See [agents](docs/agents.md) for construction and lifecycle contracts.
- **Shared message types** for text, images, reasoning, tool calls, and tool results.
- **Streaming inference** with incremental text, reasoning, and tool-input events,
  plus an assembled response, token usage, and stop reason.
- **Provider-independent application code** through a clonable `Provider` handle.
- **Text embeddings** through a separate `EmbeddingProvider` handle, with Synthetic,
  llama.cpp, Ollama, and oMLX support, batched inputs, and validated vector dimensions.
- **Typed decisions** through `DeciderProvider`, with llama.cpp System One support
  for choice, score, and yes/no probability questions.
- **Reusable protocol adapters** for OpenAI Responses, OpenAI Chat Completions,
  and Anthropic Messages.
- **Model catalogs** loaded from models.dev or a saved API document, retaining
  source metadata alongside normalized capabilities and pricing.
- **Tokio-native I/O** and structured errors using Snafu.

## Providers

| Provider | Protocols | Guide |
| --- | --- | --- |
| OpenAI | Responses (default), Chat Completions | [OpenAI](docs/openai.md) |
| Ollama (local server) | OpenAI Chat Completions, Embeddings with Ollama compatibility policy | [Ollama](docs/ollama.md) |
| llama.cpp (single-model or router) | OpenAI Chat Completions, Embeddings, System One decisions | [llama.cpp](docs/llama-cpp.md) |
| oMLX | OpenAI Chat Completions with oMLX thinking policy, Embeddings | [oMLX](docs/omlx.md) |
| Anthropic | Messages | [Anthropic](docs/anthropic.md) |
| Synthetic | OpenAI Chat Completions, Embeddings | [Synthetic](docs/synthetic.md) |
| Z.AI Coding Plan | OpenAI Chat Completions | [Z.AI Coding Plan](docs/zai-coding-plan.md) |
| Amazon Bedrock | Native Anthropic/OpenAI adapters; requires `bedrock` | [Amazon Bedrock](docs/bedrock.md) |

Provider support does not imply identical capabilities across models or endpoints.
Bedrock does not currently implement Converse.

## Getting started

Use a Rust toolchain that supports edition 2024. To depend on this crate.

There are no default Cargo features. Enable `features = ["bedrock"]` on the
Animist dependency to include the AWS provider and its dependencies.

### Stream a response

Supply `ANTHROPIC_API_KEY` through your process environment and set
`ANIMIST_MODEL` to an Anthropic model ID available to your account. Do not put
credentials in source code. This example discovers model metadata, sends one
user message, and writes streamed text to standard output:

```rust
use animist::provider::{
    ContentBlock, Message, MessageKind, Model, Provider, ProviderEvent,
    RequestOptions, Role,
    anthropic::{Anthropic, AnthropicConfig},
};
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Provider::new(Anthropic::new(
        AnthropicConfig::builder().build(),
    )?);
    let id = std::env::var("ANIMIST_MODEL")?;
    let info = provider
        .list_models()
        .await?
        .into_iter()
        .find(|info| info.ids.contains(&id))
        .ok_or("ANIMIST_MODEL was not returned by the provider")?;
    let model = Model::builder()
        .id(id)
        .provider(provider.clone())
        .info(info)
        .build();
    let messages = vec![
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::Text {
                text: "Explain Rust ownership in two sentences.".into(),
            }])
            .build(),
    ];
    let (tx, rx) = flume::bounded(64);

    let request = async {
        let result = provider
            .stream_message(
                &model,
                &messages,
                "You are a concise, helpful assistant.",
                &[],
                &tx,
                RequestOptions::default(),
            )
            .await;
        drop(tx);
        result
    };
    let output = async {
        let mut stdout = tokio::io::stdout();
        while let Ok(event) = rx.recv_async().await {
            if let ProviderEvent::TextDelta { text } = event {
                stdout.write_all(text.as_bytes()).await?;
            }
        }
        stdout.write_all(b"\n").await?;
        stdout.flush().await
    };

    let (response, output) = tokio::join!(request, output);
    output?;
    let _response = response?;
    Ok(())
}
```

Consume bounded event channels concurrently with the request to respect
backpressure. Errors return from `stream_message`, even if some events have
already arrived. Dropping the request future cancels the exchange.

The returned message preserves content beyond displayed text, including reasoning
and tool calls. Keep the complete message when replaying history; reasoning
signatures may be protocol-specific. Applications execute requested tools and
append tool results before making the next request.

## Model metadata

`catalog::Catalog` parses the models.dev API format with `from_slice`, loads saved
documents asynchronously with `load`, or fetches them with `fetch` and a
`reqwest::Client`. It provides provider-scoped model lookup and preserves metadata
that the common types cannot yet represent.

Catalog data does not construct providers or discover credentials. Runtime model
overrides are separate from catalog metadata. Missing pricing is not evidence
of free inference, and capabilities are not inferred from model names.

## Examples and documentation

The repository includes live streaming and tool-call examples:

```sh
cargo run --example synthetic -- --help
cargo run --example zai-coding-plan -- --help
cargo run --features bedrock --example bedrock -- --help
```

Help requires no credentials or network access. Inference runs require provider
credentials and consume quota. See [manual provider tests](docs/manual-provider-tests.md)
for model selection, thinking options, tool-call round trips, and local mock
endpoints.

See [providers and protocols](docs/providers-and-protocols.md) for the architecture,
responsibility boundaries, and implementation guidance.
See [decision models](docs/deciders.md) for the non-streaming typed decision API.

## Development

```sh
cargo fmt --check
cargo test
cargo test --features bedrock
```

The automated provider tests use local fixtures and mock servers rather than
paid inference. Live API compatibility and account-specific access require
separate smoke tests.

## License

Licensed under [Apache-2.0](LICENSE).
