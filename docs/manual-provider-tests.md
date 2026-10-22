# Manual provider tests

These Cargo examples make real inference requests and consume provider quota.
Supply credentials in your shell environment; do not put keys in source files
or command-line arguments.

## Quick start

With `SYNTHETIC_API_KEY` set:

```sh
cargo run --example synthetic
```

With `ZHIPU_API_KEY` set to a Coding Plan key:

```sh
cargo run --example zai-coding-plan
```

The defaults are `syn:large:text` for Synthetic and `glm-5.3` for Z.AI, adaptive
thinking, and a 4,096-token output limit. Both examples display streamed thinking
and text, then the stop reason and token usage. A completed text answer prints
`PASS`; provider errors, incomplete responses, and failed tool tests exit nonzero.

Synthetic discovers metadata through its `/models` API. Z.AI loads model metadata
from models.dev, since the Coding Plan provider has no documented discovery
endpoint. No capabilities are guessed from model names. Z.AI catalog entries
are not proof of subscription entitlement; check the plan's supported-tool rules.

## Choose a model, prompt, or effort

```sh
cargo run --example synthetic -- --list-models
cargo run --example zai-coding-plan -- --list-models

cargo run --example synthetic -- \
  --model syn:small:text --prompt "Explain Rust ownership in two sentences." \
  --thinking low --max-tokens 4096

cargo run --example zai-coding-plan -- \
  --model glm-5.3 --prompt "Write a short Rust iterator example." \
  --thinking high --max-tokens 8192
```

Efforts must appear in the model metadata. `--thinking off` tests disabling
reasoning, but some models require thinking and will reject it. A small output
limit may be exhausted by reasoning before a final answer; increase
`--max-tokens` rather than interpreting a truncated response as a successful test.

## Test tool calling and history replay

```sh
cargo run --example synthetic -- --tool-test
cargo run --example zai-coding-plan -- --tool-test
```

This registers a harmless `get_test_value` tool that returns a fixed value. The
example handles its call locally, appends the complete assistant message
(including reasoning) and tool result to history, then requests the final answer.
It passes only if the model calls the tool and reports the returned `animist-ok`
value. The tool runs no commands and accesses no files or network. At most three
inference requests are allowed.

## Offline metadata and local mock endpoints

Use `--catalog PATH` to load a saved **raw models.dev API document** instead of
fetching model metadata. Select a `--model` ID present in that document; Synthetic
aliases may not be included in saved models.dev data.

`--base-url URL` replaces the inference API base and, for Synthetic's live
discovery, the models endpoint. Include the protocol path:
`http://127.0.0.1:PORT/openai/v1` for a Synthetic mock or
`http://127.0.0.1:PORT/api/coding/paas/v4` for a Z.AI mock.
Use fake credentials when testing a local mock.

```sh
cargo run --example synthetic -- --help
cargo run --example zai-coding-plan -- --help
```

Help requires neither credentials nor network access. Streamed output and
upstream error bodies can contain sensitive conversation data; handle logs
accordingly.
