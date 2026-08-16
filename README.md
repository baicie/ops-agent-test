# OpsCodex

OpsCodex is a local-first AIOps agent runtime written in Rust. It investigates runtime incidents through a bounded `Model -> Tool -> Evidence -> Model` loop and exposes the same session through a CLI or a React UI.

The MVP is deliberately small: one Rust process, append-only JSONL sessions,
REST/SSE, three structured read-only tools, an approval-gated opt-in `exec`
escape hatch, and a two-container reproducible incident.

## Product direction

`v0.1.0` is the released Runtime MVP, not the end of the product. The repository's
authoritative product goal, target architecture, staged delivery plan, and
architecture decisions live in the [design documentation](docs/README.md).

- [Final product goal](docs/PRODUCT_GOAL.md)
- [Target architecture](docs/TARGET_ARCHITECTURE.md)
- [Version roadmap and phase gates](docs/ROADMAP.md)
- [Engineering delivery contract](docs/DELIVERY_CONTRACT.md)
- [Architecture decision records](docs/adr/README.md)

Implementation work after `v0.1.0` must identify its roadmap phase, satisfy that
phase's acceptance gate, and update an ADR before changing a recorded decision.

## What is implemented

- Hand-written Agent Runtime with Thread, Turn, Item, Context, Event and Tool abstractions
- OpenAI Responses Provider with streaming text, function calls, usage and cancellation
- `promql_query`, `docker_logs`, `http_get`, Loki `log_query`, Tempo `trace_search` / `trace_get`, read-only `k8s_get` / `k8s_events` / `k8s_logs`, `runbook_search` / `runbook_read`, `topology_query`, opt-in `exec`, plus constrained MCP/Custom Tool extensions
- Local Skill packages loaded as untrusted context, never as tools or secrets
- Safe/Ask/Forbidden policy decisions, capability effects, and interactive approval
- Per-tool/model timeouts, 12-step default limit, 64 KiB output bounds and cancellation
- One active Turn per Thread and four concurrent Turns globally by default
- Append-only JSONL persistence with monotonic sequence numbers and reconnect replay
- Axum REST API and SSE event stream
- React/Vite UI for workspace selection, threads, Alert Context, streaming chat, tools, Topology, Evidence-linked Diagnosis, approvals, structured Action review and Stop
- Structured Safe Remediation (off by default): ActionPlan, request-hash approval, demo fault reset, Kubernetes scale, kill switch, and hash-chained audit
- Operations CLI (`doctor`, `config validate`, `storage verify` / `backup` / `export`, `audit verify`), `/readyz`, and loopback-only binds without TLS
- Deterministic order-service incident with Prometheus metrics and Docker logs

## Architecture

```text
React or CLI
     |
 REST/SSE
     |
Axum App Server
     |
Agent Runtime ---- SQLite Event Store (JSONL import/export)
     |
     +---- ModelProvider ---- OpenAI-compatible Responses API
     |
     +---- Tool Registry ---- Prometheus / Loki / Tempo / Kubernetes / Docker / HTTP / Runbooks / MCP / Custom / approved Exec
     |
     +---- Remediation runner ---- demo fault reset / Kubernetes scale (approval-bound)
     |
     +---- Skill Catalog ---- local SKILL.md context only
```

The Runtime only depends on the project's `ModelProvider` contract. OpenAI request and SSE types stay inside `src/model/openai.rs`.

## Prerequisites

- Rust 1.88 or newer
- Node.js 20 or newer
- Docker Compose for the complete incident demo
- An API key for OpenAI or another Responses-compatible model provider

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

## Run with a Responses-compatible provider

```sh
mkdir -p ~/.opscodex
cp config.example.toml ~/.opscodex/config.toml
printf 'Model API key: ' >&2
IFS= read -rs OPENAI_API_KEY
printf '\n' >&2
export OPENAI_API_KEY
cargo run -- run "Why is order-service failing?" \
  --workspace default \
  --service order-service --environment staging \
  --starts-at 2026-08-16T00:00:00Z --ends-at 2026-08-16T00:15:00Z
unset OPENAI_API_KEY
```

The model name and full Responses endpoint are configurable. The default example
model is not a guarantee of account availability; set `[model].model` and
`[model].endpoint` to values supported by your provider.
Set optional `[model].reasoning_effort` to `none`, `minimal`, `low`, `medium`,
`high`, or `xhigh` only when the selected model supports that Responses option.

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
| `loki.url` | `http://localhost:3100` |
| `tempo.url` | `http://localhost:3200` |
| `tools.exec` | `false` |
| `store.backend` | `sqlite` |
| `server` | `127.0.0.1:3000` |

`allowed_hosts` gates `http_get`; `allowed_containers` gates `docker_logs`. The `exec` tool is not registered unless `[tools].exec = true` or `--enable-exec` is passed, and every call still requires explicit approval.

## API

```text
GET    /healthz
GET    /readyz
GET    /metrics
GET    /api/v1/workspaces
GET    /api/v1/threads
POST   /api/v1/threads
GET    /api/v1/threads/:thread_id
GET    /api/v1/threads/:thread_id?after=:seq&limit=:n&stream_kind=domain
POST   /api/v1/threads/:thread_id/turns
GET    /api/v1/threads/:thread_id/events?after=:seq
GET    /api/v1/threads/:thread_id/topology
GET    /api/v1/threads/:thread_id/evidence/:evidence_id
POST   /api/v1/threads/:thread_id/forks
GET    /api/v1/artifacts/:sha256
POST   /api/v1/approvals/:approval_id
POST   /api/v1/turns/:turn_id/interrupt
POST   /api/v1/turns/:turn_id/resume
GET    /api/v1/turns/:turn_id/recovery
```

`/api` remains an alias for `/api/v1` during the compatibility window. Turns may include
`incident_context`; Alert Context is an investigation hint and is not stored as Evidence.
`POST /api/v1/threads` accepts `workspace_id` and defaults to `default`. A Thread cannot
change Workspace after creation. `GET /api/v1/threads/:id/topology` returns the current
Evidence projection, not a CMDB.

SSE responses use the JSONL sequence as the SSE `id`. A reconnect with `after=N` replays all durable events after `N`, then switches to live broadcast events without duplicates.

## Storage

The default store is SQLite WAL at `~/.opscodex/state.sqlite3`. Artifacts remain in the
content-addressed directory. JSONL is retained for import, backup, and export:

```text
~/.opscodex/
  config.toml
  state.sqlite3
  threads/                 # legacy JSONL; import with `opscodex migrate`
    backup-<timestamp>/
  artifacts/
    <workspace-id>/<sha256-prefix>/<sha256>
```

`opscodex migrate --dry-run` / `--verify` imports JSONL threads into SQLite, compares
event counts and content hashes, then moves the original files into a timestamped
read-only backup. `opscodex export --thread ID --out FILE` writes one human-readable
JSONL file and never exports secrets. `opscodex storage backup --out PATH` writes a
consistent SQLite snapshot with `VACUUM INTO`. Only one OpsCodex process may open a
given SQLite file; a second process fails fast on the lock.

Without TLS the process binds loopback only. `opscodex doctor`, `config validate`,
`storage verify`, and `audit verify` do not require a model API key. Day-to-day
backup, restore, upgrade, `NeedsReconciliation`, and secret-leak response are in
[docs/OPERATIONS.md](docs/OPERATIONS.md). The frozen `/api/v1` path list lives in
[docs/contracts/](docs/contracts/README.md).

Stop OpsCodex before copying or restoring store files. A backup is the SQLite
database plus WAL sidecars and artifacts:

```sh
# Stop the process first. Copy all three SQLite files together.
cp ~/.opscodex/state.sqlite3 ~/.opscodex/state.sqlite3-wal ~/.opscodex/state.sqlite3-shm /safe/backup/
cp -R ~/.opscodex/artifacts /safe/backup/artifacts
```

To roll back a failed or unwanted JSONL migration, keep the `threads/backup-<timestamp>/`
directory. Point `[store] backend = "jsonl"` at that backup (or copy the `.jsonl`
files back to `threads/`) and start again; SQLite is not deleted automatically.
To restore a SQLite backup, stop the process, replace `state.sqlite3` together
with `-wal`/`-shm`, then start once. If the disk is full, appends fail closed;
free space and retry. Do not delete the JSONL backup or a SQLite copy to make
room until a verify pass (`opscodex migrate --verify` or a successful export)
has confirmed the active store.

Checkpoints, approvals, and leases are durable. After a restart, queued/model turns
become `interrupted` and wait for explicit Resume. Observe tools may be retried;
change or side-effecting tools whose result is unknown enter `needs_reconciliation`
and are never retried automatically. Recovery classification is covered by
`just continuity-test`; that suite simulates a kill after each durable checkpoint
commit. It does not replace a live Provider gate.

Structured remediation is disabled by default (`[remediation] enabled = false`).
When enabled per Workspace, the model can only propose an ActionPlan. Execution
requires an exact request-hash approval, a separate runner, and a process kill
switch that blocks new change operations. `exec`, MCP, and custom tools cannot
be used as remediation. Verify with `just remediation-test`.

## Development

Common commands:

```sh
just test
just check
just web-dev
just serve-fake
just demo-test
just ops-test
just capacity-test
just release-dry-run
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

The OpenAI-compatible boundary is tested against a local SSE fixture, so tests
never require or transmit an API key. Its wire mapping was cross-checked against
the official [Responses API](https://developers.openai.com/api/reference/resources/responses/methods/create)
and [function calling](https://developers.openai.com/api/docs/guides/function-calling)
documentation. CI also builds the two-container demo and verifies the complete
database-pool incident; a real Responses-compatible investigation remains a
release acceptance gate.

## License

OpsCodex is available under the [MIT License](LICENSE).
