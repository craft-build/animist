# F15: expose `reasoning_content` replay as OpenAI config

## Gap

`OpenAiOptions::replay_reasoning_content` gates replaying assistant
thinking blocks as `reasoning_content` on Chat Completions
(`protocol/openai/chat_completions.rs` refuses the request when history
carries thinking and the flag is off). The dedicated providers set it
(`ollama`, `llama_cpp`, `omlx`, `synthetic`, `zai_coding_plan` all build
their options with `replay_reasoning_content(true)`), but the plain
`OpenAi` provider never does, and `OpenAiConfig` exposes no field for a
consumer to opt in — so `kind = "openai"` with
`protocol = "chat_completions"`, and every `openai-compatible` endpoint
whose server emits `reasoning_content` (DeepSeek-dialect servers: GLM's
coding plan via base_url, Synthetic via base_url, …) fails its **second**
message with "Chat Completions reasoning history requires
reasoning_content replay support".

Any endpoint that streams `reasoning_content` also accepts it back on
assistant messages (that is the convention), so the conservative default
rejects the very servers that produced the reasoning.

Found from the consumer side: craft's offline conformance suite
(`agent/src/conformance/mod.rs`,
`reasoning_history_replays_on_every_chat_kind`) reproduces it for openai
chat_completions, openai-compatible, and bedrock chat_completions.

## Proposal

Add `replay_reasoning_content: bool` (default false) to `OpenAiConfig`,
thread it into the provider's `OpenAiOptions`, and document that
endpoints streaming `reasoning_content` should set it. Bedrock's
`chat_completions` protocol should default it on the same rule or take it
from `BedrockConfig`.

## Workaround

Consumers drop thinking blocks from the request view for these targets
(what craft's `harness::view::drop_thinking_blocks` does behind
`ModelHandle::replays_thinking`), losing reasoning replay for endpoints
that would have accepted it.

## Completion note

Implemented on `f15-openai-reasoning-replay-config`, unreleased:

- Added `OpenAiConfig::replay_reasoning_content` (default `false`; serde and
  builder defaults agree) and threaded it into the provider's `OpenAiOptions`
  in `stream_message`. It is inert for `Protocol::Responses`, which never
  reads it.
- Bedrock's shared Chat Completions/Responses options now set
  `replay_reasoning_content(true)` by default: Bedrock's OpenAI compatibility
  surface streams `reasoning_content` (DeepSeek dialect) and accepts it back.
  No `BedrockConfig` field.
- Documented the knob in `docs/openai.md` and replaced the recorded refusal in
  `docs/bedrock.md` (signed/redacted reasoning stays rejected there).

Tests in `tests/openai.rs` cover the opt-in wire replay, the `false` default,
and the `snake_case` round trip; `tests/bedrock.rs` covers default-on replay
for `Protocol::ChatCompletions`. The existing default-refusal test is
unchanged. Independent review of the full diff returned clean with no
findings. Validation passed:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo test --features bedrock`
- `cargo doc --no-deps`
