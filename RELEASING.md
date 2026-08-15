# Releasing OpsCodex

OpsCodex v0.1 is released only after automated checks and the real incident
workflow pass. A fake-model run is useful for development, but it is not a
substitute for the Docker and Responses-compatible model acceptance test below.

## 1. Prerequisites

- A clean Git worktree on `main`.
- Docker Compose with a running Docker daemon.
- An API key and a Responses-compatible model available to the project.
- GitHub CLI authentication with `repo` and `workflow` scopes.
- A protected GitHub environment named `release`, restricted to `main`, with a
  required reviewer and an `OPENAI_API_KEY` environment secret.
- `cargo-audit` 0.22 or newer and `cargo-deny` 0.20 or newer.

Never write API keys to the repository, config file, shell history, CI logs, or
release artifacts. OpsCodex reads the key from the environment variable named by
`model.api_key_env`.

## 2. Automated release gate

Run the local automated checks:

```sh
just release-check
```

Required result: formatting, Clippy, all Rust/frontend/demo tests, type checking,
production builds, RustSec advisories, dependency policy, and npm audit pass
without policy violations or high/critical findings.

GitHub Actions must also pass the `Demo containers` job, which builds the
Compose stack and validates the Prometheus, HTTP, and Docker log evidence path.

## 3. Real incident acceptance

The required release gate is the manually dispatched
`live-openai-acceptance.yml` workflow. Configure its environment secret from an
interactive terminal; do not put the value on the command line:

```sh
gh secret set OPENAI_API_KEY --env release --repo baicie/ops-agent-test
```

For v0.1.0, the workflow pins `deepseek-v4-flash` at
`https://opencode.ai/zen/go/v1/responses`. The secret name is retained as the
provider adapter's key environment variable; it contains the selected provider's
API key.

Dispatch the workflow from the exact `main` commit intended for release:

```sh
release_sha="$(git rev-parse origin/main)"
gh workflow run live-openai-acceptance.yml --ref main \
  --repo baicie/ops-agent-test

live_run_id=""
for attempt in $(seq 1 30); do
  live_run_id="$(gh run list --workflow live-openai-acceptance.yml \
    --event workflow_dispatch --branch main --commit "${release_sha}" --limit 10 \
    --json databaseId,headSha,createdAt \
    --jq 'sort_by(.createdAt) | last | .databaseId // empty')"
  test -n "${live_run_id}" && break
  sleep 2
done
test -n "${live_run_id}"
gh run watch "${live_run_id}" --exit-status
test "$(gh run view "${live_run_id}" --json headSha --jq .headSha)" = \
  "${release_sha}"
```

Approve the pending `release` deployment in GitHub Actions when prompted. After
the run succeeds, download and inspect the sanitized evidence:

```sh
evidence_dir="$(mktemp -d)"
gh run download "${live_run_id}" --dir "${evidence_dir}"
acceptance_file="$(find "${evidence_dir}" -name acceptance.json -type f -print -quit)"
runner_file="$(find "${evidence_dir}" -name runner-status.txt -type f -print -quit)"
answer_file="$(find "${evidence_dir}" -name final-answer.md -type f -print -quit)"
test -n "${acceptance_file}" && test -n "${runner_file}" && test -n "${answer_file}"
jq --exit-status \
  '.passed == true and ([.checks[] | select(.passed != true)] | length == 0)' \
  "${acceptance_file}"
test "$(cat "${runner_file}")" = "runner_exit_code=0"
cat "${answer_file}"
```

The workflow establishes a healthy baseline, injects database-pool exhaustion,
runs the release binary against the pinned Responses-compatible API, verifies the
persisted Model -> Tool -> Evidence -> Model sequence, and uploads only bounded,
redacted evidence. It never uploads the raw JSONL event log.

### Optional local reproduction

Start the two-container environment and establish a healthy baseline:

```sh
docker compose -f demo/docker-compose.yml up --build -d
curl -fsS -X POST http://localhost:8080/debug/fault/normal
curl -fsS http://localhost:8080/health
curl -fsS http://localhost:9090/-/ready
```

Allow at least 60 seconds of normal synthetic traffic, then inject the fault:

```sh
curl -fsS -X POST http://localhost:8080/debug/fault/db-pool
sleep 30
curl -fsS http://localhost:8080/health
curl -sS -o /dev/null -w '%{http_code}\n' \
  -X POST http://localhost:8080/orders -H 'content-type: application/json' -d '{}'
docker logs --since 5m --tail 200 order-service
```

The health response must be `degraded`, the order request must return `500`, and
the logs must contain `database pool exhausted`. Confirm Prometheus reports a
non-zero 5xx rate, elevated P95 latency, no idle DB connections, and waiting DB
requests using the queries in `demo/README.md`.

Configure the real model provider, then run the CLI:

```sh
mkdir -p ~/.opscodex
cp config.example.toml ~/.opscodex/config.toml
printf 'Model API key: ' >&2
IFS= read -rs OPENAI_API_KEY
printf '\n' >&2
export OPENAI_API_KEY
cargo run --release -- run 'What happened to order-service?'
unset OPENAI_API_KEY
```

Acceptance criteria:

- The Agent makes distinct, successful `promql_query` calls for the non-zero 5xx
  rate and elevated P95 latency.
- The Agent calls `docker_logs` and finds the pool-exhaustion message.
- The Agent calls `http_get` for the exact health URL and observes HTTP 200 with
  `degraded` / `db-pool` state.
- Tool results are reinjected into a later model request.
- The final answer contains Summary, Evidence, Diagnosis, Confidence, and
  Recommended next actions, and identifies DB pool exhaustion as the likely cause.
- No tool fails, requests approval, or falls outside the three read-only tools.

Repeat the prompt through `cargo run --release -- serve` and verify streaming,
expanded Evidence, Stop, reconnect replay, and responsive layout in the web UI.

Clean up the demo when acceptance is complete:

```sh
docker compose -f demo/docker-compose.yml down
```

## 4. Publish v0.1.0

Advance only after GitHub Actions and the real incident acceptance are green:

```bash
set -euo pipefail

test "$(git branch --show-current)" = main
test -z "$(git status --porcelain)"
test -z "$(git tag --list v0.1.0)"
release_sha="$(git rev-parse HEAD)"
git push origin main

run_id=""
for attempt in $(seq 1 30); do
  run_id="$(gh run list --event push --commit "$release_sha" --limit 10 \
    --json databaseId,workflowName \
    --jq '[.[] | select(.workflowName == "CI")][0].databaseId // empty')"
  test -n "$run_id" && break
  sleep 2
done
test -n "$run_id"
gh run watch "$run_id" --exit-status
test "$(gh run view "$run_id" --json headSha --jq .headSha)" = "$release_sha"
test "$(gh run view "$run_id" --json conclusion --jq .conclusion)" = success

live_run_id="$(gh run list --workflow live-openai-acceptance.yml \
  --event workflow_dispatch --branch main --commit "$release_sha" --limit 20 \
  --json conclusion,createdAt,databaseId,headSha \
  --jq '[.[] | select(.conclusion == "success")] | sort_by(.createdAt) | last | .databaseId // empty')"
test -n "$live_run_id"
test "$(gh run view "$live_run_id" --json headSha --jq .headSha)" = "$release_sha"
test "$(gh run view "$live_run_id" --json conclusion --jq .conclusion)" = success

evidence_dir="$(mktemp -d)"
gh run download "$live_run_id" --dir "$evidence_dir"
acceptance_file="$(find "$evidence_dir" -name acceptance.json -type f -print -quit)"
runner_file="$(find "$evidence_dir" -name runner-status.txt -type f -print -quit)"
test -n "$acceptance_file" && test -n "$runner_file"
jq --exit-status \
  '.passed == true and ([.checks[] | select(.passed != true)] | length == 0)' \
  "$acceptance_file"
test "$(cat "$runner_file")" = "runner_exit_code=0"

git tag -a v0.1.0 "$release_sha" -m 'OpsCodex v0.1.0'
git push origin v0.1.0
gh release create v0.1.0 --title 'OpsCodex v0.1.0' \
  --generate-notes --verify-tag
```

## 5. Rollback

OpsCodex v0.1 has no database migration. To roll back a local deployment, stop
the current process, check out the last known-good commit or tag, rebuild the web
bundle and Rust binary, then verify `/healthz` and the fake-model critical path.
JSONL thread logs remain compatible and should be preserved.

Do not move a published tag. If a defect is found after publishing `v0.1.0`, fix
it on `main` and release `v0.1.1`. Delete a remote tag only when publication has
not completed and no user could have consumed it.
