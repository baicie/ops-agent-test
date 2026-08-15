from __future__ import annotations

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "verify_live_acceptance.py"
SPEC = importlib.util.spec_from_file_location("verify_live_acceptance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)


def event(sequence: int, event_type: str, **values: object) -> dict[str, object]:
    return {
        "seq": sequence,
        "thread_id": "01900000-0000-7000-8000-000000000001",
        "turn_id": "01900000-0000-7000-8000-000000000002",
        "timestamp": "2026-08-16T10:00:00Z",
        "type": event_type,
        **values,
    }


def valid_events() -> list[dict[str, object]]:
    calls = [
        (
            "errors",
            "promql_query",
            {
                "query": (
                    'sum(rate(http_requests_total{status=~"5.."}[1m])) / '
                    "sum(rate(http_requests_total[1m]))"
                )
            },
            {"status": "success", "data": {"result": [{"value": [0, "0.5"]}]}},
            "prometheus",
        ),
        (
            "latency",
            "promql_query",
            {
                "query": (
                    "histogram_quantile(0.95, "
                    "sum by (le) (rate(http_request_duration_seconds_bucket[1m])))"
                )
            },
            {"status": "success", "data": {"result": [{"value": [0, "2.4"]}]}},
            "prometheus",
        ),
        (
            "logs",
            "docker_logs",
            {"container": "order-service", "since": "10m", "tail": 200},
            {
                "container": "order-service",
                "logs": "ERROR database pool exhausted",
            },
            "docker",
        ),
        (
            "health",
            "http_get",
            {"url": "http://127.0.0.1:8080/health"},
            {"status": 200, "body": {"status": "degraded", "mode": "db-pool"}},
            "http",
        ),
    ]
    events: list[dict[str, object]] = [event(1, "thread_created")]
    sequence = 2
    for call_id, tool, arguments, output, source in calls:
        events.append(
            event(
                sequence,
                "tool_started",
                call_id=call_id,
                tool=tool,
                arguments=arguments,
            )
        )
        sequence += 1
        events.append(
            event(
                sequence,
                "tool_completed",
                call_id=call_id,
                tool=tool,
                output=output,
                evidence={
                    "source": source,
                    "query": str(arguments),
                    "timestamp": "2026-08-16T10:00:00Z",
                    "duration_ms": 12,
                    "truncated": False,
                },
                success=True,
            )
        )
        sequence += 1
    events.append(
        event(
            sequence,
            "assistant_completed",
            content=(
                "## Summary\nOrder failures are caused by exhausted DB connections.\n"
                "## Evidence\n5xx errors, P95 latency, logs, and degraded health agree.\n"
                "## Diagnosis\nDatabase connection pool exhaustion is the likely cause.\n"
                "## Confidence\nHigh.\n"
                "## Recommended next actions\nRestore pool capacity and inspect leaks."
            ),
        )
    )
    events.append(event(sequence + 1, "turn_completed"))
    return events


def valid_observations() -> dict[str, object]:
    return {
        "baseline": {
            "health_status": "ok",
            "mode": "normal",
            "error_rate": 0.0,
            "p95_latency_seconds": 0.01,
        },
        "fault": {
            "health_status": "degraded",
            "mode": "db-pool",
            "order_http_status": 500,
            "log_contains_pool_exhaustion": True,
            "error_rate": 0.5,
            "p95_latency_seconds": 2.4,
            "db_pool_idle": 0.0,
            "db_pool_waiting": 12.0,
        },
    }


class VerifyLiveAcceptanceTests(unittest.TestCase):
    def test_complete_real_incident_evidence_passes(self) -> None:
        result = verifier.verify_acceptance(valid_events(), valid_observations(), 0)

        self.assertTrue(result["passed"])
        checks = {check["name"]: check["passed"] for check in result["checks"]}
        self.assertTrue(checks["promql_error_query_succeeded"])
        self.assertTrue(checks["promql_latency_query_succeeded"])
        self.assertTrue(checks["docker_logs_found_pool_exhaustion"])
        self.assertTrue(checks["http_get_observed_degraded_health"])
        self.assertTrue(checks["final_answer_has_required_sections"])
        self.assertTrue(checks["final_answer_diagnoses_db_pool_exhaustion"])

    def test_missing_latency_query_fails(self) -> None:
        events = [item for item in valid_events() if item.get("call_id") != "latency"]

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("promql_latency_query_succeeded", failed)

    def test_error_and_latency_require_distinct_promql_calls(self) -> None:
        events = [item for item in valid_events() if item.get("call_id") != "latency"]
        error_started = next(
            item
            for item in events
            if item.get("call_id") == "errors" and item["type"] == "tool_started"
        )
        error_started["arguments"] = {
            "query": (
                'sum(rate(http_requests_total{status=~"5.."}[1m])) + '
                "histogram_quantile(0.95, "
                "sum by (le) (rate(http_request_duration_seconds_bucket[1m])))"
            )
        }

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("promql_error_and_latency_are_distinct_calls", failed)

    def test_promql_call_without_numeric_result_fails(self) -> None:
        events = valid_events()
        completed = next(
            item
            for item in events
            if item.get("call_id") == "errors" and item["type"] == "tool_completed"
        )
        completed["output"] = {"status": "success", "data": {"result": []}}

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("promql_error_query_succeeded", failed)

    def test_bare_metric_selectors_are_not_rate_or_p95_queries(self) -> None:
        events = valid_events()
        for item in events:
            if item.get("call_id") == "errors" and item["type"] == "tool_started":
                item["arguments"] = {"query": 'http_requests_total{status=~"5.."}'}
            if item.get("call_id") == "latency" and item["type"] == "tool_started":
                item["arguments"] = {"query": "http_request_duration_seconds_bucket"}

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("promql_error_query_succeeded", failed)
        self.assertIn("promql_latency_query_succeeded", failed)

    def test_health_call_requires_degraded_db_pool_response(self) -> None:
        events = valid_events()
        completed = next(
            item
            for item in events
            if item.get("call_id") == "health" and item["type"] == "tool_completed"
        )
        completed["output"] = {
            "status": 200,
            "body": {"status": "ok", "note": "previously degraded"},
        }

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("http_get_observed_degraded_health", failed)

    def test_health_call_requires_exact_incident_url(self) -> None:
        events = valid_events()
        started = next(
            item
            for item in events
            if item.get("call_id") == "health" and item["type"] == "tool_started"
        )
        started["arguments"] = {"url": "http://127.0.0.1:8080/health?cached=true"}

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("http_get_observed_degraded_health", failed)

    def test_incomplete_final_answer_fails(self) -> None:
        events = valid_events()
        assistant = next(
            item for item in events if item["type"] == "assistant_completed"
        )
        assistant["content"] = "Database pool exhaustion is likely."

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("final_answer_has_required_sections", failed)

    def test_negated_pool_exhaustion_diagnosis_fails(self) -> None:
        events = valid_events()
        assistant = next(
            item for item in events if item["type"] == "assistant_completed"
        )
        assistant["content"] = assistant["content"].replace(
            "Database connection pool exhaustion is the likely cause.",
            "Database connection pool exhaustion is not the likely cause.",
        )

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("final_answer_diagnoses_db_pool_exhaustion", failed)

    def test_diagnosis_section_must_identify_pool_exhaustion(self) -> None:
        events = valid_events()
        assistant = next(
            item for item in events if item["type"] == "assistant_completed"
        )
        assistant["content"] = (
            "## Summary\nOrders are failing.\n"
            "## Evidence\nLogs mention database connection pool exhaustion.\n"
            "## Diagnosis\nA traffic spike is the likely cause.\n"
            "## Confidence\nLow.\n"
            "## Recommended next actions\nInspect request volume."
        )

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("final_answer_diagnoses_db_pool_exhaustion", failed)

    def test_opposing_pool_exhaustion_language_fails(self) -> None:
        diagnoses = (
            "Database connection pool exhaustion wasn't the likely cause; DNS was.",
            "Database connection pool exhaustion is merely a symptom; DNS is the root cause.",
            "Database connection pool exhaustion cannot be considered the cause.",
        )
        for diagnosis in diagnoses:
            with self.subTest(diagnosis=diagnosis):
                events = valid_events()
                assistant = next(
                    item for item in events if item["type"] == "assistant_completed"
                )
                assistant["content"] = assistant["content"].replace(
                    "Database connection pool exhaustion is the likely cause.",
                    diagnosis,
                )

                result = verifier.verify_acceptance(events, valid_observations(), 0)

                self.assertFalse(result["passed"])
                failed = {
                    check["name"] for check in result["checks"] if not check["passed"]
                }
                self.assertIn("final_answer_diagnoses_db_pool_exhaustion", failed)

    def test_failed_tool_call_fails(self) -> None:
        events = valid_events()
        completed = next(
            item
            for item in events
            if item.get("call_id") == "logs" and item["type"] == "tool_completed"
        )
        completed["success"] = False
        completed["output"] = {"error": "tool is forbidden"}

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("only_allowed_tools_succeeded_without_approval", failed)

    def test_approval_or_unexpected_tool_fails(self) -> None:
        events = valid_events()
        events.insert(
            -2,
            event(
                9,
                "approval_required",
                approval_id="01900000-0000-7000-8000-000000000003",
                tool="exec",
                arguments={"command": "true"},
            ),
        )

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("only_allowed_tools_succeeded_without_approval", failed)

    def test_events_from_multiple_turns_fail(self) -> None:
        events = valid_events()
        completed = next(
            item
            for item in events
            if item.get("call_id") == "logs" and item["type"] == "tool_completed"
        )
        completed["turn_id"] = "01900000-0000-7000-8000-000000000099"

        result = verifier.verify_acceptance(events, valid_observations(), 0)

        self.assertFalse(result["passed"])
        failed = {check["name"] for check in result["checks"] if not check["passed"]}
        self.assertIn("single_thread_turn_event_sequence", failed)

    def test_written_artifacts_redact_exact_and_key_shaped_secrets(self) -> None:
        secret = "EXACT_TEST_SECRET_VALUE"
        key_shaped_secret = "sk-TEST_ONLY_KEY_SHAPED_VALUE"
        events = valid_events()
        completed = next(
            item
            for item in events
            if item.get("call_id") == "logs" and item["type"] == "tool_completed"
        )
        completed["output"]["logs"] += f" token={secret} token={key_shaped_secret}"

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            previous = os.environ.get("OPSCODEX_REDACT_VALUE")
            os.environ["OPSCODEX_REDACT_VALUE"] = secret
            try:
                verifier.write_artifacts(
                    output,
                    verifier.verify_acceptance(events, valid_observations(), 0),
                    events,
                    valid_observations(),
                    f"completed with {secret}",
                )
            finally:
                if previous is None:
                    os.environ.pop("OPSCODEX_REDACT_VALUE", None)
                else:
                    os.environ["OPSCODEX_REDACT_VALUE"] = previous

            artifact_text = "\n".join(
                path.read_text(encoding="utf-8") for path in output.iterdir()
            )
            self.assertNotIn(secret, artifact_text)
            self.assertNotIn(key_shaped_secret, artifact_text)
            self.assertIn("[REDACTED]", artifact_text)


if __name__ == "__main__":
    unittest.main()
