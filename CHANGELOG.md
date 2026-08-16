# Changelog

All notable changes to OpsCodex are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Event schema v2 with stable IDs, stream kinds, and compatible v0.1 JSONL reads.
- Evidence IDs, Claim-linked Diagnosis, artifact spillover, and default redaction.
- Multi-budget context builder and provider capability declarations.
- Alert/Incident Context on CLI, API, and Web, kept separate from Evidence.
- Loki `log_query` and Tempo `trace_search` / `trace_get` read-only tools.
- `/api/v1` routes, Evidence/Artifact APIs, `/metrics`, and Evidence-linked UI.
- Workspace scope, read-only Kubernetes tools, topology projection, and local Runbooks.
- Capability descriptors, MCP/Custom Tool adapters, and local Skill context loading.
- SQLite WAL default store, JSONL migrate/export, durable checkpoints/approvals/leases,
  recovery classification, context compaction, thread fork, and a simulated
  crash-after-checkpoint recovery suite.
- Structured Safe Remediation: ActionPlan state machine, parameter-bound approvals,
  isolated demo fault reset and Kubernetes scale runners, kill switch, and
  hash-chained Security Audit. Default configuration still performs zero mutations.
- Production-readiness operations: `doctor` / `config validate` / `storage verify` /
  `storage backup` / `audit verify`, `/readyz`, loopback-only binds without TLS,
  and frozen `/api/v1` contract fixtures. `/healthz` is process liveness only.

### Documentation

- Defined the `v1.0` product goal, scope, measurable completion criteria, and
  target architecture.
- Added detailed `v0.1` through `v1.0` phase plans, engineering delivery rules,
  traceability, and architecture decision records.

## [0.1.0] - 2026-08-16

### Added

- Rust Agent Runtime with a bounded Model -> Tool -> Evidence -> Model loop.
- OpenAI Responses API provider with configurable reasoning effort, streaming,
  function calls, usage, errors, timeouts, and cancellation.
- PromQL, Docker logs, HTTP GET, and approval-gated exec tools.
- Append-only JSONL threads with replay-safe REST and SSE APIs.
- React investigation UI with streaming, evidence, approvals, and interruption.
- Deterministic order-service and Prometheus incident demo.
- Protected manual Docker/Responses release acceptance with redacted evidence.
- Rust, frontend, API, runtime, tool-safety, and demo integration tests.

[Unreleased]: https://github.com/baicie/ops-agent-test/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/baicie/ops-agent-test/releases/tag/v0.1.0
