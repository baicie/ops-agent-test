#!/usr/bin/env bash
set -Eeuo pipefail

umask 077

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_file="${OPSCODEX_COMPOSE_FILE:-${repo_root}/demo/docker-compose.yml}"
binary="${OPSCODEX_BINARY:-${repo_root}/target/release/opscodex}"
artifact_dir="${OPSCODEX_ACCEPTANCE_ARTIFACT_DIR:-${repo_root}/artifacts/live-openai-acceptance}"
model="${OPSCODEX_MODEL:-gpt-5.2}"
model_endpoint="${OPSCODEX_MODEL_ENDPOINT:-https://api.openai.com/v1/responses}"
reasoning_effort="${OPSCODEX_REASONING_EFFORT:-}"
baseline_seconds="${OPSCODEX_BASELINE_SECONDS:-60}"
fault_seconds="${OPSCODEX_FAULT_SECONDS:-30}"
api_key="${OPENAI_API_KEY:-}"
unset OPENAI_API_KEY

work_dir=""
stack_started=false

cleanup() {
  local exit_code=$?

  unset OPENAI_API_KEY OPSCODEX_REDACT_VALUE
  api_key=""
  if [[ "${stack_started}" == true ]]; then
    docker compose -f "${compose_file}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  fi
  if [[ -n "${work_dir}" && -d "${work_dir}" ]]; then
    rm -rf -- "${work_dir}"
  fi
  mkdir -p "${artifact_dir}"
  printf 'runner_exit_code=%s\n' "${exit_code}" >"${artifact_dir}/runner-status.txt"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

fail() {
  printf 'Live acceptance failed: %s\n' "$1" >&2
  return 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

metric_value() {
  local query=$1
  local response value
  local attempt

  for attempt in {1..20}; do
    if response="$(curl --fail --silent --show-error --get \
      --data-urlencode "query=${query}" \
      http://127.0.0.1:9090/api/v1/query)" \
      && value="$(jq --exit-status --raw-output \
        '.data.result[0].value[1] | tonumber' <<<"${response}")"; then
      printf '%s\n' "${value}"
      return 0
    fi
    sleep 2
  done
  fail "Prometheus did not return a numeric result for a required query"
}

run_agent() {
  export OPENAI_API_KEY="${api_key}"
  export OPSCODEX_HOME="${work_dir}/state"
  if command -v timeout >/dev/null 2>&1; then
    timeout --signal=TERM --kill-after=15s 15m \
      "${binary}" --config "${work_dir}/config.toml" run "${prompt}"
  else
    "${binary}" --config "${work_dir}/config.toml" run "${prompt}"
  fi
}

mkdir -p "${artifact_dir}"
if [[ -z "${api_key}" ]]; then
  fail "OPENAI_API_KEY is not configured"
fi
if [[ ! "${model}" =~ ^[A-Za-z0-9._:-]+$ ]]; then
  fail "OPSCODEX_MODEL contains unsupported characters"
fi
if [[ "${model_endpoint}" != https://*/responses \
  || "${model_endpoint}" == *\"* \
  || "${model_endpoint}" == *\\* \
  || "${model_endpoint}" =~ [[:space:]] ]]; then
  fail "OPSCODEX_MODEL_ENDPOINT must be a safe HTTPS Responses endpoint"
fi
if [[ -n "${reasoning_effort}" \
  && ! "${reasoning_effort}" =~ ^(none|minimal|low|medium|high|xhigh)$ ]]; then
  fail "OPSCODEX_REASONING_EFFORT contains an unsupported value"
fi
if [[ ! "${baseline_seconds}" =~ ^[0-9]+$ || ! "${fault_seconds}" =~ ^[0-9]+$ ]]; then
  fail "baseline and fault observation durations must be whole seconds"
fi

for command_name in curl docker jq python3; do
  require_command "${command_name}"
done
[[ -x "${binary}" ]] || fail "release binary is missing or not executable: ${binary}"
[[ -f "${compose_file}" ]] || fail "Compose file does not exist: ${compose_file}"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/opscodex-live-acceptance.XXXXXX")"
mkdir -p "${work_dir}/state"
reasoning_config=""
if [[ -n "${reasoning_effort}" ]]; then
  reasoning_config="reasoning_effort = \"${reasoning_effort}\""
fi

cat >"${work_dir}/config.toml" <<EOF
[model]
provider = "openai"
model = "${model}"
api_key_env = "OPENAI_API_KEY"
endpoint = "${model_endpoint}"
${reasoning_config}

[runtime]
max_steps = 12
max_concurrent_turns = 1
tool_timeout_seconds = 30
model_timeout_seconds = 120
max_output_bytes = 65536
context_items = 100

[prometheus]
url = "http://127.0.0.1:9090"

[targets]
allowed_containers = ["order-service"]
allowed_hosts = ["localhost", "127.0.0.1"]

[tools]
exec = false
EOF

error_query='sum(rate(http_requests_total{service="order-service",route="/orders",status=~"5.."}[1m])) / clamp_min(sum(rate(http_requests_total{service="order-service",route="/orders"}[1m])), 0.000001)'
latency_query='histogram_quantile(0.95, sum by (le) (rate(http_request_duration_seconds_bucket{service="order-service",route="/orders"}[1m])))'
prompt=$(cat <<EOF
Investigate the active order-service incident using only the read-only tools.
Before answering, you must gather all four evidence sources below:
1. Call promql_query with this current 5xx-rate query: ${error_query}
2. Call promql_query with this current P95-latency query: ${latency_query}
3. Call docker_logs for the order-service container and inspect recent logs.
4. Call http_get for http://127.0.0.1:8080/health.

Correlate the returned evidence. Your final answer must use these exact section
headings: Summary, Evidence, Diagnosis, Confidence, Recommended next actions.
Under Diagnosis, include this exact standalone sentence:
Database connection pool exhaustion is the likely cause.
EOF
)

printf 'Starting the deterministic demo stack...\n'
docker compose -f "${compose_file}" down --volumes --remove-orphans >/dev/null 2>&1 || true
docker compose -f "${compose_file}" up --build --detach --wait --wait-timeout 120
stack_started=true

curl --fail --silent --show-error \
  --retry 20 --retry-all-errors --retry-delay 2 \
  http://127.0.0.1:9090/-/ready >/dev/null

baseline_health="$(curl --fail --silent --show-error --request POST \
  http://127.0.0.1:8080/debug/fault/normal)"
jq --exit-status '.mode == "normal"' <<<"${baseline_health}" >/dev/null
printf 'Holding normal mode for %s seconds to establish the baseline...\n' "${baseline_seconds}"
sleep "${baseline_seconds}"

baseline_health="$(curl --fail --silent --show-error http://127.0.0.1:8080/health)"
jq --exit-status '.status == "ok" and .mode == "normal"' \
  <<<"${baseline_health}" >/dev/null
baseline_error_rate="$(metric_value "${error_query}")"
baseline_p95="$(metric_value "${latency_query}")"
jq --exit-status --null-input --argjson value "${baseline_error_rate}" \
  '$value <= 0.01' >/dev/null
jq --exit-status --null-input --argjson value "${baseline_p95}" \
  '$value < 1.0' >/dev/null

printf 'Injecting the database-pool fault...\n'
fault_response="$(curl --fail --silent --show-error --request POST \
  http://127.0.0.1:8080/debug/fault/db-pool)"
jq --exit-status '.mode == "db-pool"' <<<"${fault_response}" >/dev/null
sleep "${fault_seconds}"

fault_health="$(curl --fail --silent --show-error http://127.0.0.1:8080/health)"
jq --exit-status '.status == "degraded" and .mode == "db-pool"' \
  <<<"${fault_health}" >/dev/null
order_status="$(curl --silent --show-error --output "${work_dir}/order-response.json" \
  --write-out '%{http_code}' --request POST \
  --header 'content-type: application/json' --data '{}' \
  http://127.0.0.1:8080/orders)"
[[ "${order_status}" == 500 ]] || fail "faulted order endpoint did not return HTTP 500"
jq --exit-status '.error == "database pool exhausted" and .mode == "db-pool"' \
  "${work_dir}/order-response.json" >/dev/null

docker compose -f "${compose_file}" logs --no-color order-service \
  >"${work_dir}/order-service.log" 2>&1
grep --quiet --fixed-strings 'database pool exhausted' "${work_dir}/order-service.log" \
  || fail "order-service logs do not contain the pool-exhaustion evidence"

fault_error_rate="$(metric_value "${error_query}")"
fault_p95="$(metric_value "${latency_query}")"
fault_pool_idle="$(metric_value 'db_pool_idle{service="order-service"}')"
fault_pool_waiting="$(metric_value 'db_pool_waiting{service="order-service"}')"
jq --exit-status --null-input --argjson value "${fault_error_rate}" \
  '$value > 0' >/dev/null
jq --exit-status --null-input --argjson value "${fault_p95}" \
  '$value > 2.0' >/dev/null
jq --exit-status --null-input \
  --argjson idle "${fault_pool_idle}" --argjson waiting "${fault_pool_waiting}" \
  '$idle == 0 and $waiting > 0' >/dev/null

jq --null-input \
  --arg baseline_status "$(jq --raw-output '.status' <<<"${baseline_health}")" \
  --arg baseline_mode "$(jq --raw-output '.mode' <<<"${baseline_health}")" \
  --argjson baseline_error "${baseline_error_rate}" \
  --argjson baseline_p95 "${baseline_p95}" \
  --arg fault_status "$(jq --raw-output '.status' <<<"${fault_health}")" \
  --arg fault_mode "$(jq --raw-output '.mode' <<<"${fault_health}")" \
  --argjson order_status "${order_status}" \
  --argjson fault_error "${fault_error_rate}" \
  --argjson fault_p95 "${fault_p95}" \
  --argjson pool_idle "${fault_pool_idle}" \
  --argjson pool_waiting "${fault_pool_waiting}" \
  '{
    baseline: {
      health_status: $baseline_status,
      mode: $baseline_mode,
      error_rate: $baseline_error,
      p95_latency_seconds: $baseline_p95
    },
    fault: {
      health_status: $fault_status,
      mode: $fault_mode,
      order_http_status: $order_status,
      log_contains_pool_exhaustion: true,
      error_rate: $fault_error,
      p95_latency_seconds: $fault_p95,
      db_pool_idle: $pool_idle,
      db_pool_waiting: $pool_waiting
    }
  }' >"${work_dir}/observations.json"

printf 'Running the real Responses-compatible OpsCodex investigation...\n'
set +e
(run_agent) >"${work_dir}/cli.log" 2>&1
cli_exit_code=$?
set -e
unset OPENAI_API_KEY

shopt -s nullglob
event_logs=("${work_dir}/state/threads/"*.jsonl)
if [[ ${#event_logs[@]} -eq 1 ]]; then
  events_file="${event_logs[0]}"
else
  events_file="${work_dir}/events.jsonl"
  : >"${events_file}"
fi

set +e
OPSCODEX_REDACT_VALUE="${api_key}" python3 \
  "${repo_root}/scripts/verify_live_acceptance.py" \
  --events "${events_file}" \
  --observations "${work_dir}/observations.json" \
  --cli-log "${work_dir}/cli.log" \
  --cli-exit-code "${cli_exit_code}" \
  --output-dir "${artifact_dir}"
verification_exit_code=$?
set -e
api_key=""
unset OPENAI_API_KEY OPSCODEX_REDACT_VALUE

if [[ ${cli_exit_code} -ne 0 ]]; then
  printf 'OpsCodex exited with status %s; sanitized details are in the artifact.\n' \
    "${cli_exit_code}" >&2
fi
exit "${verification_exit_code}"
