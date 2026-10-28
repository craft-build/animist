# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- *(crate)* [**breaking**] backtraces on source-less snafu error variants; snafu `backtrace` feature enabled
- *(crate)* [**breaking**] re-export `reqwest`, `flume`, and `jiff`; public docs across provider, protocol, agent, history, and catalog surfaces
- *(crate)* [**breaking**] `[lints]` table enforcing the stylish Rust lint set; MSRV pinned to 1.95
- *(protocol)* [**breaking**] typed `StreamEvent` dispatch for the Responses assembler and `Display` for `ReplayLocation`
- *(protocol)* unit tests for replay validation, System One answers, and embeddings assembly
- *(agent)* [**breaking**] `Debug` for `Agent`, `RunOptions`, `RunContext`, `StepRequest`, `ToolRegistry`, and `Model`

### Changed

- *(provider)* [**breaking**] `Pricing` rates are `Option<f64>`; missing rates stay unknown instead of free
- *(provider)* [**breaking**] `EmbeddingRequest::dimensions` is `Option<NonZeroU32>`
- *(provider)* [**breaking**] `Provider::name` returns `&'static str`; failed-response bodies clamp to 64 KiB
- *(provider)* `Effort::openai_wire` replaces seven duplicated effort-to-wire mappings
- *(history)* [**breaking**] `History::add` takes `Message` by value; first VCC breadcrumb is excerpted into budget instead of dropped
- *(catalog)* [**breaking**] model names fall back to the model ID; catalog parsing borrows instead of cloning records
- *(agent)* [**breaking**] removed dead `AgentError::Provider` and `AgentError::Layer`; system prompt rendered once per run

### Fixed

- *(provider)* OpenAI credential loading preserves `VarError` for non-UTF-8 variables
- *(agent)* tool timeout path no longer discards the elapsed error through `map_err`

## [0.4.0](https://github.com/craft-build/animist/compare/v0.3.0...v0.4.0) - 2026-10-27

### Added

- *(agent)* [**breaking**] implement shared agent runtime and typed tools
- *(agent)* initial skeleton for agent module

### Fixed

- *(provider)* reconcile fixtures with runtime model handles

### Other

- add local rebase landing skill

## [0.3.0](https://github.com/craft-build/animist/compare/v0.2.0...v0.3.0) - 2026-10-23

### Added

- *(decider)* add System One protocol and llama.cpp support
- *(embeddings)* add typed providers and OpenAI-compatible adapters
- *(provider)* add Ollama, llama.cpp, and oMLX support
- *(history)* add extensible VCC compaction
- *(history)* [**breaking**] preserve revisions and apply atomic compaction plans
- *(examples)* add manual Bedrock provider test
- *(history)* [**breaking**] implement a history shape and migrate message structs into it

### Other

- fix clippy issues

## [0.2.0](https://github.com/craft-build/animist/compare/v0.1.0...v0.2.0) - 2026-10-22

### Added

- *(bedrock)* add native API provider with AWS SigV4
- *(examples)* add manual provider smoke tests
- *(zai-coding-plan)* add streaming provider
- *(synthetic)* add streaming provider
- *(anthropic)* add provider and Messages protocol
- *(openai)* add provider and streaming protocol adapters
- *(catalog)* load model metadata from models.dev
- *(provider)* complete shared message and request types
- *(provider)* starting structure

### Fixed

- *(sse)* process final events at end of stream
- *(protocol)* reject silent reasoning history loss

### Other

- add project README
- *(github)* add missing permissions
- *(provider)* mark unused constants for now allowed
- *(github)* fix release-plz
- *(deps)* update dependencies
- record provider protocol design and agent guidelines
- *(github)* fix github workflows
- add repository-wide agent guidelines
- setup release plz and ci workflows
