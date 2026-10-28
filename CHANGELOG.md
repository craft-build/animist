# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/craft-build/animist/compare/v0.4.0...v0.5.0) - 2026-10-28

### Added

- *(provider)* [**breaking**] add shared sampling controls
- *(agent)* [**breaking**] add bounded conflict-aware tool scheduling
- *(inference)* [**breaking**] retain cancellation-safe partial responses
- *(protocol)* validate prepared conversation replay
- *(tools)* [**breaking**] preserve rich tool results across execution and replay

### Fixed

- [**breaking**] bound HTTP error reads and guard empty score levels

### Other

- *(github)* fix changelog for release-plz
- *(deps)* update dependencies
- [**breaking**] apply whole-crate rust review findings

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
