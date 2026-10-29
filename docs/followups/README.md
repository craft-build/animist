# animist followups for craft

Implementation prompts for **animist** (`~/Projects/animist`), not craft. Each
one closes a gap that craft's migration (`docs/animist/01`–`12`) works around
on animist 0.5.0. A craft prompt stays valid without its followup: workarounds
ship first, and each followup lists the craft-side cleanup to do once animist
releases it.

## Instructions for every followup

- Work in `~/Projects/animist` on a branch from `main`. Read `AGENTS.md`
  first, and follow it strictly:
  - async-native, with no `spawn_blocking`/`block_on` without approval and no
    detached tasks;
  - snafu errors as specific variants;
  - `async-trait`, plus `arc-handle` for shared handles;
  - `bon` builders;
  - doc comments of five lines or fewer.
- **Load the `rust-best-practices` skill before writing any code** and apply
  it throughout.
- Public enums are exhaustive and additive changes are breaking (`lib.rs`), so
  mark breaking changes `feat!:`/`fix!:` for release-plz, and update the
  affected `docs/*.md` contract text in the same change.
- Tests use descriptive sentence names and local fixtures or mock servers, with
  no paid inference.
- Validate:
  - `cargo fmt --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`
  - `cargo test --features bedrock`
- **When implementation is finished, run the review agent** over the full
  diff. Address every finding at or above medium, re-run validation, and repeat
  until the verdict is clean.
- When done, fill in the completion note. Once released, mark the row
  *Released* below with the version, and do the listed craft cleanup (or hand
  it to a craft prompt).

## Index

Priority reflects how much craft behaviour or complexity rides on the
workaround.

| # | Followup | Priority | Craft workaround it removes | Status |
| --- | --- | --- | --- | --- |
| F01 | [Step-boundary messages](F01-step-boundary-messages.md) | High | Doom grace / nudge / continuation re-runs (07, 08) | Open |
| F02 | [Parallel child dispatch](F02-parallel-child-dispatch.md) | High | Serial `batch` children (10) | Open |
| F03 | [Tool test context](F03-tool-test-context.md) | Medium | One-shot-agent `tool_harness` (06) | Implemented; reviewed, unreleased |
| F04 | [Revision squash](F04-revision-squash.md) | High | Persistence-time placeholder pruning (11) | Implemented; reviewed, unreleased |
| F05 | [History change feed](F05-history-change-feed.md) | Medium | Snapshot-shape diffing for incremental saves (11) | Implemented; reviewed, unreleased |
| F06 | [Tool decision with a response](F06-tool-decision-respond.md) | Medium | Dedup served inside `HarnessExecutor` (05) | Implemented; reviewed, unreleased |
| F07 | [Panic-safe tool execution](F07-panic-safe-tools.md) | Medium | `catch_unwind` in `HarnessExecutor` (05) | Implemented; reviewed, unreleased |
| F08 | [Usage cost](F08-usage-cost.md) | Medium | Craft-side pricing (03) | Open |
| F09 | [Bedrock client, listing, Converse](F09-bedrock-client-and-converse.md) | Medium | No timeout client, `max_attempts = 1`, craft listing, Converse-only models unsupported (02) | Implemented; reviewed, unreleased |
| F10 | [Tool name on results](F10-tool-result-name.md) | Low | `history::ext::tool_name_for` (04) | Open |
