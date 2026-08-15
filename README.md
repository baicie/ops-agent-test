# OpsCodex

OpsCodex is a local-first AIOps agent runtime written in Rust. It investigates runtime incidents through a bounded `Model -> Tool -> Evidence -> Model` loop and exposes the same session through a CLI or a React UI.

The MVP is deliberately small: one Rust process, append-only JSONL sessions, REST/SSE, four read-oriented tools, and a two-container reproducible incident.

## What is implemented

- Hand-written Agent Runtime with Thread, Turn, Item, Context, Event and Tool abstractions
- OpenAI Responses Provider with streaming text, function calls, usage and cancellation
- `promql_query`, `docker_logs`, `http_get`, and opt-in `exec`
- Safe/Ask/Forbidden policy decisions and interactive approval
- Per-tool/model timeouts, 12-step default limit, 64 KiB output bounds and cancellation
- One active Turn per Thread and four concurrent Turns globally by default
- Append-only JSONL persistence with monotonic sequence numbers and reconnect replay
- Axum REST API and SSE event stream
- React/Vite UI for threads, streaming chat, tools, Evidence, approvals and Stop
- Deterministic order-service incident with Prometheus metrics and Docker logs

## Architecture

```text
React or CLI
     |
 REST/SSE
     |
Axum App Server
     |
Agent Runtime ---- JSONL Thread Store
     |
     +---- ModelProvider ---- OpenAI Responses API
     |
     +---- Tool Registry ---- Prometheus / Docker / HTTP / approved Exec
```

The Runtime only depends on the project's `ModelProvider` contract. OpenAI request and SSE types stay inside `src/model/openai.rs`.

## Prerequisites

- Rust 1.88 or newer
- Node.js 20 or newer
- Docker Compose for the complete incident demo
- An OpenAI API key for the real model provider

Docker is not required for unit tests or the local fake-model walkthrough.

## Quick start without an API key

```sh
cd web
npm ci
npm run build
cd ..
cargo run -- --fake-model serve
```

Open `http://127.0.0.1:3000`. The deterministic local provider calls the real `http_get` tool and is intended only for development and UI verification.

For Vite hot reload, run these in separate terminals:

```sh
cargo run -- --fake-model serve
```

```sh
cd web
npm run dev
```

The Vite UI is then available at `http://127.0.0.1:5173` and proxies API/SSE traffic to port 3000.

## Run with OpenAI

```sh
mkdir -p ~/.opscodex
cp config.example.toml ~/.opscodex/config.toml
printf 'OpenAI API key: ' >&2
IFS= read -rs OPENAI_API_KEY
printf '\n' >&2
export OPENAI_API_KEY
cargo run -- run "Why is order-service failing?"
unset OPENAI_API_KEY
```

The model name and Responses endpoint are configurable. The default example model is not a guarantee of account availability; set `[model].model` to a Responses-compatible model available to your OpenAI project.

Start the web server with the real provider:

```sh
cargo run -- serve
```

## Reproduce the incident

Start the two-container demo:

```sh
docker compose -f demo/docker-compose.yml up --build
```

Inject database-pool exhaustion:

```sh
curl -X POST http://localhost:8080/debug/fault/db-pool
curl http://localhost:8080/health
```

Then ask OpsCodex:

```sh
cargo run -- run "order-service is returning errors. Investigate it."
```

The demo produces elevated 5xx responses, about 2.4 seconds of latency, depleted DB-pool gauges, degraded health, and the exact log evidence `database pool exhausted`. See [demo/README.md](demo/README.md) for PromQL examples and all fault modes.

## Configuration

OpsCodex reads `~/.opscodex/config.toml` by default. Set `OPSCODEX_HOME` to relocate both configuration and thread logs. Pass `--config FILE` to load a specific config file.

Important defaults:

| Setting | Default |
| --- | --- |
| `runtime.max_steps` | `12` |
| `runtime.max_concurrent_turns` | `4` |
| `runtime.tool_timeout_seconds` | `30` |
| `runtime.model_timeout_seconds` | `120` |
| `runtime.max_output_bytes` | `65536` |
| `prometheus.url` | `http://localhost:9090` |
| `tools.exec` | `false` |
| `server` | `127.0.0.1:3000` |

`allowed_hosts` gates `http_get`; `allowed_containers` gates `docker_logs`. The `exec` tool is not registered unless `[tools].exec = true` or `--enable-exec` is passed, and every call still requires explicit approval.

## API

```text
GET    /healthz
GET    /api/threads
POST   /api/threads
GET    /api/threads/:thread_id
POST   /api/threads/:thread_id/turns
GET    /api/threads/:thread_id/events?after=:seq
POST   /api/approvals/:approval_id
POST   /api/turns/:turn_id/interrupt
```

SSE responses use the JSONL sequence as the SSE `id`. A reconnect with `after=N` replays all durable events after `N`, then switches to live broadcast events without duplicates.

## Storage

Each Thread is a human-readable event log:

```text
~/.opscodex/
  config.toml
  threads/
    <thread-id>.jsonl
```

The Store serializes appends per process, validates monotonic sequence numbers, ignores an incomplete crash tail during replay, and repairs that tail before the next append.

Turn execution and approvals are intentionally process-local in v0.1. If the server
is restarted while a Turn is running or waiting for approval, the JSONL history is
kept for inspection but the in-flight operation is not resumed; start a new Turn
after the process is back.

## Development

Common commands:

```sh
just test
just check
just web-dev
just serve-fake
just demo-test
```

Without `just`, run the underlying checks directly:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cd web && npm test && npm run build
python3 -m unittest discover -s demo/order-service/tests -v
python3 -m unittest discover -s scripts/tests -v
```

The full live-environment acceptance, release, and rollback procedure is in
[RELEASING.md](RELEASING.md). Release notes are maintained in
[CHANGELOG.md](CHANGELOG.md).

The OpenAI boundary is tested against a local SSE fixture, so tests never require or transmit an API key. Its wire mapping was cross-checked against the official [Responses API](https://developers.openai.com/api/reference/resources/responses/methods/create) and [function calling](https://developers.openai.com/api/docs/guides/function-calling) documentation. CI also builds the two-container demo and verifies the complete database-pool incident; the real OpenAI investigation remains a release acceptance gate.

## License

OpsCodex is available under the [MIT License](LICENSE).
