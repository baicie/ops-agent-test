#!/usr/bin/env python3
"""Verify and sanitize evidence from the live Docker/OpenAI acceptance run."""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import sys
from pathlib import Path
from typing import Any


MAX_ARTIFACT_STRING = 65_536
REQUIRED_SECTIONS = (
    "summary",
    "evidence",
    "diagnosis",
    "confidence",
    "recommended next actions",
)
ALLOWED_ACCEPTANCE_TOOLS = {"promql_query", "docker_logs", "http_get"}
EXPECTED_EVIDENCE_SOURCES = {
    "promql_query": "prometheus",
    "docker_logs": "docker",
    "http_get": "http",
}
EXPECTED_HEALTH_URL = "http://127.0.0.1:8080/health"
REQUIRED_DIAGNOSIS = "Database connection pool exhaustion is the likely cause."
KEY_PATTERN = re.compile(r"\bsk-[A-Za-z0-9_-]{12,}\b", re.IGNORECASE)
BEARER_PATTERN = re.compile(r"(?i)(authorization\s*[:=]\s*bearer\s+)[^\s,;\"']+")


def _as_float(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _json_text(value: Any) -> str:
    return json.dumps(value, ensure_ascii=True, sort_keys=True)


def _promql_value(call: dict[str, Any]) -> float | None:
    output = call.get("output")
    if not isinstance(output, dict) or output.get("status") != "success":
        return None
    data = output.get("data")
    if not isinstance(data, dict) or not isinstance(data.get("result"), list):
        return None
    for sample in data["result"]:
        if not isinstance(sample, dict):
            continue
        value = sample.get("value")
        if not isinstance(value, list) or len(value) < 2:
            continue
        number = _as_float(value[1])
        if number is not None and math.isfinite(number):
            return number
    return None


def _successful_calls(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    completions = {
        item.get("call_id"): item
        for item in events
        if item.get("type") == "tool_completed" and item.get("call_id")
    }
    calls = []
    for started in events:
        if started.get("type") != "tool_started":
            continue
        completed = completions.get(started.get("call_id"))
        calls.append(
            {
                "call_id": started.get("call_id"),
                "tool": started.get("tool"),
                "arguments": started.get("arguments", {}),
                "started_seq": started.get("seq"),
                "completed_seq": completed.get("seq") if completed else None,
                "success": completed.get("success") is True if completed else False,
                "output": completed.get("output") if completed else None,
                "evidence": completed.get("evidence") if completed else None,
            }
        )
    return calls


def _is_error_query(call: dict[str, Any]) -> bool:
    if call.get("tool") != "promql_query" or not call.get("success"):
        return False
    query = str(call.get("arguments", {}).get("query", "")).lower()
    has_error_status = bool(
        re.search(r"status\s*=~?\s*[\"'][^\"']*5(?:\.|x|\d)", query)
        or "5xx" in query
        or "5.." in query
    )
    value = _promql_value(call)
    return (
        "http_requests_total" in query
        and "rate(" in query
        and has_error_status
        and value is not None
        and value > 0
    )


def _is_latency_query(call: dict[str, Any]) -> bool:
    if call.get("tool") != "promql_query" or not call.get("success"):
        return False
    query = str(call.get("arguments", {}).get("query", "")).lower()
    value = _promql_value(call)
    return (
        "http_request_duration_seconds_bucket" in query
        and "rate(" in query
        and re.search(r"histogram_quantile\s*\(\s*0\.95\b", query) is not None
        and value is not None
        and value > 2.0
    )


def _section_heading(source_line: str) -> tuple[str, str] | None:
    line = re.sub(r"^#{1,6}\s*", "", source_line.strip())
    line = line.replace("**", "").replace("__", "").strip()
    lowered = line.casefold()
    for section in REQUIRED_SECTIONS:
        if lowered == section:
            return section, ""
        if lowered.startswith(f"{section}:"):
            return section, line[len(section) + 1 :].strip()
    return None


def _section_content(answer: str, target: str) -> str:
    active = False
    content: list[str] = []
    for line in answer.splitlines():
        heading = _section_heading(line)
        if heading is not None:
            section, inline_content = heading
            if active and section != target:
                break
            active = section == target
            if active and inline_content:
                content.append(inline_content)
        elif active:
            content.append(line.strip())
    return "\n".join(content).strip()


def _has_required_sections(answer: str) -> bool:
    found = {
        heading[0]
        for line in answer.splitlines()
        if (heading := _section_heading(line)) is not None
    }
    return found == set(REQUIRED_SECTIONS)


def _diagnoses_pool_exhaustion(answer: str) -> bool:
    for source_line in _section_content(answer, "diagnosis").splitlines():
        line = re.sub(r"^[-*+]\s+", "", source_line.strip())
        line = line.replace("**", "").replace("__", "").strip()
        if line.casefold() == REQUIRED_DIAGNOSIS.casefold():
            return True
    return False


def verify_acceptance(
    events: list[dict[str, Any]],
    observations: dict[str, Any],
    cli_exit_code: int,
) -> dict[str, Any]:
    """Return a machine-readable result without mutating or sanitizing inputs."""

    checks: list[dict[str, Any]] = []

    def check(name: str, passed: bool, detail: str) -> None:
        checks.append({"name": name, "passed": bool(passed), "detail": detail})

    baseline = observations.get("baseline", {})
    fault = observations.get("fault", {})
    baseline_error = _as_float(baseline.get("error_rate"))
    baseline_latency = _as_float(baseline.get("p95_latency_seconds"))
    fault_error = _as_float(fault.get("error_rate"))
    fault_latency = _as_float(fault.get("p95_latency_seconds"))
    fault_idle = _as_float(fault.get("db_pool_idle"))
    fault_waiting = _as_float(fault.get("db_pool_waiting"))

    event_sequences = [item.get("seq") for item in events]
    thread_ids = {item.get("thread_id") for item in events}
    turn_ids = {
        item.get("turn_id") for item in events if item.get("type") != "thread_created"
    }
    coherent_events = (
        bool(events)
        and events[0].get("type") == "thread_created"
        and len(thread_ids) == 1
        and None not in thread_ids
        and len(turn_ids) == 1
        and None not in turn_ids
        and all(isinstance(sequence, int) for sequence in event_sequences)
        and event_sequences == sorted(set(event_sequences))
    )
    check(
        "single_thread_turn_event_sequence",
        coherent_events,
        f"threads={len(thread_ids)}, turns={len(turn_ids)}, "
        f"events={len(event_sequences)}, unique_sequences={len(set(event_sequences))}",
    )

    check(
        "cli_completed_successfully", cli_exit_code == 0, f"exit_code={cli_exit_code}"
    )
    check(
        "normal_baseline_established",
        baseline.get("health_status") == "ok"
        and baseline.get("mode") == "normal"
        and baseline_error is not None
        and baseline_error <= 0.01
        and baseline_latency is not None
        and baseline_latency < 1.0,
        f"health={baseline.get('health_status')}, mode={baseline.get('mode')}, "
        f"error_rate={baseline_error}, p95_seconds={baseline_latency}",
    )
    check(
        "db_pool_fault_observed",
        fault.get("health_status") == "degraded"
        and fault.get("mode") == "db-pool"
        and fault.get("order_http_status") == 500
        and fault.get("log_contains_pool_exhaustion") is True
        and fault_error is not None
        and fault_error > 0
        and fault_latency is not None
        and fault_latency > 2.0
        and fault_idle == 0
        and fault_waiting is not None
        and fault_waiting > 0,
        f"health={fault.get('health_status')}, mode={fault.get('mode')}, "
        f"order_status={fault.get('order_http_status')}, error_rate={fault_error}, "
        f"p95_seconds={fault_latency}, idle={fault_idle}, waiting={fault_waiting}",
    )

    calls = _successful_calls(events)
    started_events = [item for item in events if item.get("type") == "tool_started"]
    completed_events = [item for item in events if item.get("type") == "tool_completed"]
    started_ids = [item.get("call_id") for item in started_events]
    completed_ids = [item.get("call_id") for item in completed_events]
    approval_events = [
        item
        for item in events
        if item.get("type") in {"approval_required", "approval_resolved"}
    ]
    unexpected_tools = sorted(
        {
            str(item.get("tool"))
            for item in started_events + completed_events + approval_events
            if item.get("tool") not in ALLOWED_ACCEPTANCE_TOOLS
        }
    )
    started_by_id = {
        item["call_id"]: item
        for item in started_events
        if isinstance(item.get("call_id"), str) and item["call_id"]
    }
    completed_by_id = {
        item["call_id"]: item
        for item in completed_events
        if isinstance(item.get("call_id"), str) and item["call_id"]
    }
    pairs_match = all(
        completed_by_id[call_id].get("tool") == started.get("tool")
        and isinstance(started.get("seq"), int)
        and isinstance(completed_by_id[call_id].get("seq"), int)
        and completed_by_id[call_id]["seq"] > started["seq"]
        and isinstance(completed_by_id[call_id].get("evidence"), dict)
        and completed_by_id[call_id]["evidence"].get("source")
        == EXPECTED_EVIDENCE_SOURCES.get(str(started.get("tool")))
        for call_id, started in started_by_id.items()
        if call_id in completed_by_id
    )
    paired_once = (
        all(isinstance(call_id, str) and call_id for call_id in started_ids)
        and all(isinstance(call_id, str) and call_id for call_id in completed_ids)
        and len(started_ids) == len(set(started_ids))
        and len(completed_ids) == len(set(completed_ids))
        and set(started_ids) == set(completed_ids)
        and pairs_match
    )
    all_tools_succeeded = all(item.get("success") is True for item in completed_events)
    check(
        "only_allowed_tools_succeeded_without_approval",
        bool(started_events)
        and paired_once
        and all_tools_succeeded
        and not approval_events
        and not unexpected_tools,
        f"started={len(started_events)}, completed={len(completed_events)}, "
        f"approvals={len(approval_events)}, unexpected_tools={unexpected_tools}",
    )

    error_calls = [call for call in calls if _is_error_query(call)]
    latency_calls = [call for call in calls if _is_latency_query(call)]
    log_calls = [
        call
        for call in calls
        if call.get("tool") == "docker_logs"
        and call.get("success")
        and call.get("arguments", {}).get("container") == "order-service"
        and "database pool exhausted" in _json_text(call.get("output")).casefold()
    ]
    health_calls = [
        call
        for call in calls
        if call.get("tool") == "http_get"
        and call.get("success")
        and call.get("arguments", {}).get("url") == EXPECTED_HEALTH_URL
        and isinstance(call.get("output"), dict)
        and call["output"].get("status") == 200
        and isinstance(call["output"].get("body"), dict)
        and call["output"]["body"].get("status") == "degraded"
        and call["output"]["body"].get("mode") == "db-pool"
    ]

    check(
        "promql_error_query_succeeded",
        bool(error_calls),
        f"matching_successful_calls={len(error_calls)}",
    )
    check(
        "promql_latency_query_succeeded",
        bool(latency_calls),
        f"matching_successful_calls={len(latency_calls)}",
    )
    distinct_promql_calls = any(
        error_call.get("call_id") != latency_call.get("call_id")
        for error_call in error_calls
        for latency_call in latency_calls
    )
    check(
        "promql_error_and_latency_are_distinct_calls",
        distinct_promql_calls,
        f"error_call_ids={[call.get('call_id') for call in error_calls]}, "
        f"latency_call_ids={[call.get('call_id') for call in latency_calls]}",
    )
    check(
        "docker_logs_found_pool_exhaustion",
        bool(log_calls),
        f"matching_successful_calls={len(log_calls)}",
    )
    check(
        "http_get_observed_degraded_health",
        bool(health_calls),
        f"matching_successful_calls={len(health_calls)}",
    )

    final_events = [
        item for item in events if item.get("type") == "assistant_completed"
    ]
    final_event = final_events[-1] if final_events else {}
    final_answer = str(final_event.get("content", ""))
    final_sequence = final_event.get("seq")
    required_calls = error_calls + latency_calls + log_calls + health_calls
    required_completed_sequences = [
        call["completed_seq"]
        for call in required_calls
        if isinstance(call.get("completed_seq"), int)
    ]
    check(
        "final_answer_follows_required_tool_evidence",
        isinstance(final_sequence, int)
        and len(required_completed_sequences) >= 4
        and final_sequence > max(required_completed_sequences),
        f"final_seq={final_sequence}, required_completed={required_completed_sequences}",
    )
    check(
        "final_answer_has_required_sections",
        _has_required_sections(final_answer),
        "required=Summary,Evidence,Diagnosis,Confidence,Recommended next actions",
    )
    check(
        "final_answer_diagnoses_db_pool_exhaustion",
        _diagnoses_pool_exhaustion(final_answer),
        "expected an explicit database/DB connection pool exhaustion diagnosis",
    )
    turn_completed_sequences = [
        item.get("seq") for item in events if item.get("type") == "turn_completed"
    ]
    turn_completed = (
        isinstance(final_sequence, int)
        and len(turn_completed_sequences) == 1
        and isinstance(turn_completed_sequences[0], int)
        and turn_completed_sequences[0] > final_sequence
    )
    turn_failed = any(
        item.get("type") in {"turn_failed", "turn_cancelled"} for item in events
    )
    check(
        "turn_completed_without_failure",
        turn_completed and not turn_failed,
        f"turn_completed={turn_completed}, failure_or_cancel={turn_failed}",
    )

    return {
        "passed": all(item["passed"] for item in checks),
        "checks": checks,
        "final_answer": final_answer,
        "tool_calls": calls,
    }


def _redact_text(value: str, secret: str) -> str:
    if secret:
        value = value.replace(secret, "[REDACTED]")
    value = BEARER_PATTERN.sub(r"\1[REDACTED]", value)
    value = KEY_PATTERN.sub("[REDACTED]", value)
    if len(value) > MAX_ARTIFACT_STRING:
        return value[:MAX_ARTIFACT_STRING] + "\n...[artifact text truncated]"
    return value


def _sanitize(value: Any, secret: str) -> Any:
    if isinstance(value, str):
        return _redact_text(value, secret)
    if isinstance(value, list):
        return [_sanitize(item, secret) for item in value]
    if isinstance(value, dict):
        return {
            _redact_text(str(key), secret): _sanitize(item, secret)
            for key, item in value.items()
        }
    return value


def _write_json(path: Path, value: Any) -> None:
    path.write_text(
        json.dumps(value, indent=2, ensure_ascii=True, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def write_artifacts(
    output_dir: Path,
    result: dict[str, Any],
    events: list[dict[str, Any]],
    observations: dict[str, Any],
    cli_output: str,
) -> None:
    """Write only bounded, sanitized evidence; never copy the raw event log."""

    output_dir.mkdir(parents=True, exist_ok=True)
    secret = os.environ.get("OPSCODEX_REDACT_VALUE", "")
    thread_ids = sorted(
        {str(item["thread_id"]) for item in events if item.get("thread_id")}
    )
    metadata = {
        "schema_version": 1,
        "passed": result.get("passed", False),
        "checks": result.get("checks", []),
        "source": {
            "repository": os.environ.get("GITHUB_REPOSITORY", "local"),
            "git_sha": os.environ.get("GITHUB_SHA", "local"),
            "workflow_run_id": os.environ.get("GITHUB_RUN_ID", "local"),
            "model": os.environ.get("OPSCODEX_MODEL", "gpt-5.2"),
            "thread_ids": thread_ids,
        },
    }
    sanitized_metadata = _sanitize(metadata, secret)
    sanitized_calls = _sanitize(result.get("tool_calls", []), secret)
    sanitized_observations = _sanitize(observations, secret)
    sanitized_answer = _redact_text(str(result.get("final_answer", "")), secret)
    sanitized_cli_output = _redact_text(cli_output, secret)

    _write_json(output_dir / "acceptance.json", sanitized_metadata)
    _write_json(output_dir / "tool-calls.json", sanitized_calls)
    _write_json(output_dir / "demo-observations.json", sanitized_observations)
    (output_dir / "final-answer.md").write_text(
        sanitized_answer.rstrip() + "\n", encoding="utf-8"
    )
    (output_dir / "cli-output.txt").write_text(
        sanitized_cli_output.rstrip() + "\n", encoding="utf-8"
    )

    if secret:
        for path in output_dir.iterdir():
            if path.is_file() and secret in path.read_text(encoding="utf-8"):
                raise RuntimeError(f"secret remained in sanitized artifact {path.name}")


def _load_events(path: Path) -> list[dict[str, Any]]:
    events = []
    for line_number, source in enumerate(
        path.read_text(encoding="utf-8").splitlines(), 1
    ):
        if not source.strip():
            continue
        value = json.loads(source)
        if not isinstance(value, dict):
            raise ValueError(f"event line {line_number} is not a JSON object")
        events.append(value)
    if not events:
        raise ValueError("event log is empty")
    return events


def _load_object(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("observations must be a JSON object")
    return value


def _arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--events", type=Path, required=True)
    parser.add_argument("--observations", type=Path, required=True)
    parser.add_argument("--cli-log", type=Path, required=True)
    parser.add_argument("--cli-exit-code", type=int, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = _arguments()
    events: list[dict[str, Any]] = []
    observations: dict[str, Any] = {}
    cli_output = ""
    try:
        events = _load_events(args.events)
        observations = _load_object(args.observations)
        cli_output = args.cli_log.read_text(encoding="utf-8", errors="replace")
        result = verify_acceptance(events, observations, args.cli_exit_code)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        result = {
            "passed": False,
            "checks": [
                {
                    "name": "acceptance_evidence_is_readable",
                    "passed": False,
                    "detail": str(error),
                }
            ],
            "final_answer": "",
            "tool_calls": [],
        }

    try:
        write_artifacts(args.output_dir, result, events, observations, cli_output)
    finally:
        os.environ.pop("OPSCODEX_REDACT_VALUE", None)

    status = "PASS" if result["passed"] else "FAIL"
    print(f"Live Docker/OpenAI acceptance: {status}")
    for item in result["checks"]:
        marker = "ok" if item["passed"] else "FAILED"
        print(f"- {marker}: {item['name']}")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
