# Decision models

`provider::DeciderProvider` is a clonable handle for non-streaming typed decisions.
The first implementation is `LlamaCpp`, using the TypeSafe-compatible System One
protocol exposed at `/v1/systemone` by direct servers and the llama.cpp router.

## Usage

```rust,no_run
use std::{collections::BTreeMap, sync::Arc};
use animist::provider::{
    DeciderModel, DeciderProvider, DecisionAnswer, DecisionQuestion, DecisionRequest,
    Provider, llama_cpp::{LlamaCpp, LlamaCppConfig},
};
use serde_json::json;

# async fn example() -> Result<(), animist::provider::ProviderError> {
let llama = Arc::new(LlamaCpp::new(LlamaCppConfig::default())?);
let chat = Provider::from_arc(llama.clone());
let decider = DeciderProvider::from_arc(llama);
let model = DeciderModel::builder()
    .id("my-decision-preset")
    .provider("llama-cpp")
    .build();
let request = DecisionRequest::builder()
    .state(json!("The customer was charged twice."))
    .questions(BTreeMap::from([
        ("route".into(), DecisionQuestion::Choice {
            instructions: json!("Which team should handle this?"),
            options: BTreeMap::from([
                ("billing".into(), None),
                ("technical".into(), None),
            ]),
        }),
        ("refund".into(), DecisionQuestion::Boolean {
            instructions: json!("Is a refund requested?"),
            criteria: None,
        }),
    ]))
    .build();
let response = decider.decide(&model, &request).await?;
if let DecisionAnswer::Choice { choice, .. } = &response.answers["route"] {
    println!("Route to {choice}");
}
# Ok(())
# }
```

`DeciderProvider::new(LlamaCpp::new(config)?)` is sufficient without a chat handle.
Shared `Arc` construction retains one credential store and HTTP client across
chat, embedding, and decision handles. Existing llama.cpp authentication, client
injection, URL prefixes, and credential reload behavior apply unchanged.

## Questions and answers

Questions and answers are keyed by caller-chosen IDs. Multiple question types
can be mixed in one request:

| Public question | Wire type | Answer |
| --- | --- | --- |
| `Choice` | `choice` | Selected option, per-option probabilities, confidence |
| `Score` | `score` | Expected zero-based level index, legend, per-level probabilities, confidence |
| `Boolean` | `noul` | Probability of true, not a thresholded boolean |

`Choice::options` maps names to optional descriptions. `Score::levels` contains
descriptions in ascending order. `BooleanCriteria` optionally describes true
and false through `when_true` and `when_false`.

State, instructions, and descriptions retain structured JSON supplied by the
application; Animist does not stringify them or construct chat history. State
and instructions may be text, objects, arrays, or null. Null support, allowed
description shapes, and other model-specific restrictions remain server-owned.

Results preserve the returned model ID, probabilities, confidence, and score.
Scores are fractional expected indices, not rounded classifications. Reported
usage is optional; missing usage stays unknown. llama.cpp reports zero output
tokens, but the common type does not force that rule on other implementations.

The adapter rejects missing/extra answer IDs, type mismatches, unknown selected
options, missing/extra probability keys, invalid level indices, out-of-range
scores, and non-finite or out-of-range probabilities/confidence. Probability
sums must be within 0.001 of one to allow rounding. Values are never renormalized.
Score legends retain server descriptions, which some models serialize as text.

## Server and routing requirements

Use a recent llama.cpp build with `/v1/systemone` and a native decision model.
The request's ordinary `model` field selects the router model or preset. A chat
model prompted to classify is not a native decision model; upstream returns
HTTP 501 for unsupported models. Animist preserves HTTP status and error bodies.

The router advertises native decision models in
`architecture.output_modalities`, containing `"decisions"`. A model can advertise
both `"text"` and `"decisions"`; older servers may omit this metadata.
Animist currently uses explicit `DeciderModel` IDs, not model-name heuristics or
automatic mixed-operation catalog discovery.

Option counts, score-level limits, prompt truncation, and batching requirements
vary by model. Most score models support 2–10 levels; h2o-lightning supports a
wider range. Animist rejects empty criteria but leaves model-specific limits to
the server. Questions are generally evaluated independently; joint models such
as clef evaluate them together. Animist does not assume independence.

`DecisionRequest::files` accepts inline image/audio base64 data URLs. Remote URLs
are rejected locally. Media requires a capable model, its projector, and server
support. Animist sends `files`, not the server's `images` alias; it does not
translate Animist chat messages or support TypeSafe asset placeholders or boxes.
Raw structured state can use llama.cpp's documented message-shaped media input.

Requests do not stream. Dropping the request future cancels the HTTP exchange;
no background request is detached. Applications own thresholds, routing policy,
and action execution. Model probabilities need not be calibrated for your data.

## Validation and references

`cargo test --test decider` uses local HTTP fixtures, including the documented
mixed-question response, authentication, router IDs, malformed answers, and
cancellation. It also exercises downstream provider and non-HTTP protocol
implementations. No models are downloaded or live inference invoked.

- [llama.cpp System One API](https://github.com/ggml-org/llama.cpp/tree/master/tools/server#post-v1systemone-typesafe-compatible-system-one-api)
- [TypeSafe System One API](https://docs.typesafe.ai/api)
- [OpenJev multimodal API](https://jev-skills.github.io/openjev-multimodal/api)
