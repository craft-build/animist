# F08 — Compute usage cost

## Required workflow

- **Load the `rust-best-practices` skill before writing any code**, and follow
  [the followup instructions](README.md#instructions-for-every-followup).
- When implementation is finished, **run the review agent** over the full diff,
  and iterate until the verdict is clean.

## Problem

`TokenUsage::cost` is documented as "set only when rates are known", but no
code sets it. `Pricing::rates` and the cache multipliers in
`src/provider/model.rs` are `#[expect(dead_code, reason = "wired up with usage
cost totals")]`. So `StatsSnapshot.usage.cost` is always `None` and every
attempt counts as unpriced. Craft (03) prices usage itself from `usage.rs` and
the catalog.

## Goal

Every assembled response's usage carries a cost when the model's `Pricing`
covers the billed token classes. Fast-mode pricing is applied when the request
used `fast`. Missing rates stay `None`; a missing rate is never treated as
zero.

## Design guidance

- Compute in one place that sees the `Model` and the request's `fast` flag:
  either the provider or exchange assembly, or the agent's stats recording.
  Prefer the shared location over per-provider code.
- Partial (interrupted) attempts with checkpoint usage are priced the same way.
- Decide what happens when some rates are present and some are missing, for
  example cache-write rates absent: `None` (conservative), or a documented
  partial. Keep `unpriced_*` counters meaningful.
- Remove the `dead_code` expectations, and update the
  `docs/providers-and-protocols.md` and `docs/agents.md` usage sections.

## Tests

- Priced, unpriced, partially priced, fast-mode and cache-read/write cases.
- An interrupted attempt with checkpoint usage is priced.
- `StatsSnapshot` totals and `by_model` costs add up.

## Craft cleanup after release

03 and 08: price from `TokenUsage::cost`. Keep `usage.rs` only for craft-specific
schedules, if any remain.

## Completion note

_(fill in when done)_
