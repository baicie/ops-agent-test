# Changelog

All notable changes to OpsCodex are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-16

### Added

- Rust Agent Runtime with a bounded Model -> Tool -> Evidence -> Model loop.
- OpenAI Responses API provider with streaming, function calls, usage, errors,
  timeouts, and cancellation.
- PromQL, Docker logs, HTTP GET, and approval-gated exec tools.
- Append-only JSONL threads with replay-safe REST and SSE APIs.
- React investigation UI with streaming, evidence, approvals, and interruption.
- Deterministic order-service and Prometheus incident demo.
- Protected manual Docker/OpenAI release acceptance with redacted evidence.
- Rust, frontend, API, runtime, tool-safety, and demo integration tests.

[Unreleased]: https://github.com/baicie/ops-agent-test/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/baicie/ops-agent-test/releases/tag/v0.1.0
