# F12 — Provider client timeouts

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  the followup instructions in
  [the followups README](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

Every non-Bedrock provider builds a plain `reqwest::Client::new()`, which has
no timeouts at all. A provider that accepts the connection and then stalls
(the server hangs mid-stream, a proxy black-holes the read) holds the attempt
open forever; nothing in the exchange or the agent runtime bounds it.

Bedrock is fine: its default client sets a 10 s connect timeout and its
`send_signed` loop never retries transport errors. The unbounded case is the
generic providers.

Craft pinned a 10 s connect / 300 s stream policy on a shared client for
years (against rig) and dropped it in the animist cutover to keep client
construction single-sourced. The guard needs to come back here so every
harness gets it.

## Goal

- A documented default timeout policy for the provider clients: connect
  timeout and an overall request/stream timeout, applied without callers
  constructing clients.
- Deliberate: the overall timeout must not cut off legitimately long
  streams (Synthetic's infinite mode, long reasoning turns). The policy
  needs to distinguish "no bytes for N seconds" from "streaming for a long
  time" — a read/idle timeout (`read_timeout` on the reqwest builder) is
  the right shape, not a total request timeout.
- Keep `.with_client` as the escape hatch for harnesses with their own
  policy; document that injected clients own their own timeouts.

## Tests

- A stalled-loopback test against a real provider (accept, never respond)
  that fails within the idle budget rather than hanging.
- A slow-drip test (a byte every second) that runs past the idle budget
  and completes.

## Out of scope

- Bedrock's client (already bounded per attempt).
- Retries (`max_attempts` policy stays as is).

## Completion note

Implemented, unreleased. `src/provider/http.rs::default_client` now builds the
default client for all seven generic providers with a 10-second connect
timeout and a 300-second read-inactivity timeout (`ClientBuilder::read_timeout`,
reset by every received byte) and no total request deadline, so active
long-running and Infinite-mode streams survive while accepted-then-stalled
exchanges fail as `ProviderError::Transport` timeouts. `with_client` keeps its
signature; injected clients own all timeout policy, and Bedrock is untouched.

Two paused-time loopback tests in `tests/openai.rs` exercise the production
`OpenAi::new` default: a stalled accepted request times out within the idle
budget (with a virtual watchdog), and a byte-per-second stream longer than 300
virtual seconds completes, proving there is no overall deadline. A yield pump
keeps the paused runtime polling real loopback IO before letting virtual time
advance; tokio `test-util` was added as a dev dependency. The contract is
documented under "Transport timeouts" in `docs/providers-and-protocols.md`,
linked from every provider's client-injection passage.

Review verdict was clean with no findings; release with `fix:` (no public API
change). Craft cleanup (02): craft's own injected timeout client for the
generic providers can be dropped once animist ships this default.
