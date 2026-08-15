#!/usr/bin/env python3
"""Deterministic fault-injection service for the OpsCodex demo."""

from __future__ import annotations

import json
import logging
import os
import sys
import threading
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Callable
from urllib.parse import urlsplit


LOGGER = logging.getLogger("order-service")
SERVICE_NAME = "order-service"


@dataclass(frozen=True)
class FaultProfile:
    mode: str
    delay_seconds: float
    order_status: int
    error: str | None
    health_status: str
    db_pool_active: int
    db_pool_idle: int
    db_pool_waiting: int


class OrderServiceState:
    """Holds the active deterministic fault profile and order sequence."""

    MODES = ("normal", "errors", "latency", "db-pool")

    def __init__(self, latency_seconds: float = 2.4):
        if latency_seconds < 0:
            raise ValueError("latency_seconds must not be negative")
        self._latency_seconds = latency_seconds
        self._mode = "normal"
        self._next_order_number = 1
        self._lock = threading.Lock()

    def set_mode(self, mode: str) -> FaultProfile:
        if mode not in self.MODES:
            raise ValueError(f"unknown fault mode: {mode}")
        with self._lock:
            self._mode = mode
            return self._profile_unlocked()

    def profile(self) -> FaultProfile:
        with self._lock:
            return self._profile_unlocked()

    def next_order_id(self) -> str:
        with self._lock:
            order_id = f"order-{self._next_order_number:06d}"
            self._next_order_number += 1
            return order_id

    def _profile_unlocked(self) -> FaultProfile:
        if self._mode == "normal":
            return FaultProfile("normal", 0.0, 201, None, "ok", 2, 8, 0)
        if self._mode == "errors":
            return FaultProfile(
                "errors",
                0.0,
                500,
                "simulated order processing error",
                "degraded",
                2,
                8,
                0,
            )
        if self._mode == "latency":
            return FaultProfile(
                "latency", self._latency_seconds, 201, None, "degraded", 8, 2, 3
            )
        return FaultProfile(
            "db-pool",
            self._latency_seconds,
            500,
            "database pool exhausted",
            "degraded",
            10,
            0,
            12,
        )


class MetricsRegistry:
    """Small Prometheus registry containing only metrics needed by the demo."""

    BUCKETS = (0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0)

    def __init__(self):
        order_keys = [("POST", "/orders", status) for status in ("201", "500")]
        self._requests: dict[tuple[str, str, str], int] = {key: 0 for key in order_keys}
        self._duration_buckets: dict[tuple[str, str, str], list[int]] = {
            key: [0] * (len(self.BUCKETS) + 1) for key in order_keys
        }
        self._duration_count: dict[tuple[str, str, str], int] = {
            key: 0 for key in order_keys
        }
        self._duration_sum: dict[tuple[str, str, str], float] = {
            key: 0.0 for key in order_keys
        }
        self._pool = (2, 8, 0)
        self._lock = threading.Lock()

    def observe_request(
        self, method: str, route: str, status: int, duration_seconds: float
    ) -> None:
        key = (method, route, str(status))
        duration = max(0.0, duration_seconds)
        with self._lock:
            self._requests[key] = self._requests.get(key, 0) + 1
            counts = self._duration_buckets.setdefault(
                key, [0] * (len(self.BUCKETS) + 1)
            )
            for index, bucket in enumerate(self.BUCKETS):
                if duration <= bucket:
                    counts[index] += 1
            counts[-1] += 1
            self._duration_count[key] = self._duration_count.get(key, 0) + 1
            self._duration_sum[key] = self._duration_sum.get(key, 0.0) + duration

    def set_db_pool(self, active: int, idle: int, waiting: int) -> None:
        with self._lock:
            self._pool = (active, idle, waiting)

    def render(self) -> str:
        with self._lock:
            requests = dict(self._requests)
            bucket_counts = {
                key: list(value) for key, value in self._duration_buckets.items()
            }
            duration_counts = dict(self._duration_count)
            duration_sums = dict(self._duration_sum)
            active, idle, waiting = self._pool

        lines = [
            "# HELP http_requests_total Total HTTP requests handled.",
            "# TYPE http_requests_total counter",
        ]
        for key in sorted(requests):
            labels = self._request_labels(key)
            lines.append(f"http_requests_total{{{labels}}} {requests[key]}")

        lines.extend(
            [
                "# HELP http_request_duration_seconds HTTP request duration in seconds.",
                "# TYPE http_request_duration_seconds histogram",
            ]
        )
        for key in sorted(bucket_counts):
            labels = self._request_labels(key)
            counts = bucket_counts[key]
            for bucket, count in zip(self.BUCKETS, counts[:-1]):
                lines.append(
                    "http_request_duration_seconds_bucket"
                    f'{{le="{self._number(bucket)}",{labels}}} {count}'
                )
            lines.append(
                "http_request_duration_seconds_bucket"
                f'{{le="+Inf",{labels}}} {counts[-1]}'
            )
            lines.append(
                f"http_request_duration_seconds_count{{{labels}}} "
                f"{duration_counts[key]}"
            )
            lines.append(
                f"http_request_duration_seconds_sum{{{labels}}} "
                f"{self._number(duration_sums[key])}"
            )

        for name, help_text, value in (
            ("db_pool_active", "Active database pool connections.", active),
            ("db_pool_idle", "Idle database pool connections.", idle),
            ("db_pool_waiting", "Requests waiting for a database connection.", waiting),
        ):
            lines.extend(
                [
                    f"# HELP {name} {help_text}",
                    f"# TYPE {name} gauge",
                    f'{name}{{service="{SERVICE_NAME}"}} {value}',
                ]
            )
        return "\n".join(lines) + "\n"

    @staticmethod
    def _request_labels(key: tuple[str, str, str]) -> str:
        method, route, status = key
        return (
            f'method="{_escape_label(method)}",'
            f'route="{_escape_label(route)}",'
            f'service="{SERVICE_NAME}",'
            f'status="{_escape_label(status)}"'
        )

    @staticmethod
    def _number(value: float) -> str:
        return format(value, ".12g")


@dataclass(frozen=True)
class Response:
    status: int
    body: bytes
    content_type: str


class Application:
    def __init__(
        self,
        state: OrderServiceState | None = None,
        metrics: MetricsRegistry | None = None,
        clock: Callable[[], float] = time.monotonic,
        sleeper: Callable[[float], None] = time.sleep,
    ):
        self.state = state or OrderServiceState()
        self.metrics = metrics or MetricsRegistry()
        self.clock = clock
        self.sleeper = sleeper
        self._update_pool_metrics(self.state.profile())

    def handle(self, method: str, raw_path: str, body: bytes) -> Response:
        method = method.upper()
        path = urlsplit(raw_path).path
        route = self._metric_route(path)
        started = self.clock()
        response = self._dispatch(method, path, body)
        elapsed = self.clock() - started
        self.metrics.observe_request(method, route, response.status, elapsed)
        return response

    def _dispatch(self, method: str, path: str, body: bytes) -> Response:
        if method == "GET" and path == "/health":
            profile = self.state.profile()
            return self._json(
                200, {"status": profile.health_status, "mode": profile.mode}
            )
        if method == "GET" and path == "/metrics":
            return Response(
                200,
                self.metrics.render().encode("utf-8"),
                "text/plain; version=0.0.4; charset=utf-8",
            )
        if method == "POST" and path == "/orders":
            return self._create_order(body)
        if method == "POST" and path.startswith("/debug/fault/"):
            mode = path.removeprefix("/debug/fault/")
            if mode not in OrderServiceState.MODES:
                return self._json(404, {"error": "unknown fault mode"})
            profile = self.state.set_mode(mode)
            self._update_pool_metrics(profile)
            LOGGER.info("fault mode changed mode=%s", mode)
            return self._json(200, {"status": "configured", "mode": mode})
        if path in ("/health", "/metrics", "/orders") or path.startswith(
            "/debug/fault/"
        ):
            return self._json(405, {"error": "method not allowed"})
        return self._json(404, {"error": "not found"})

    def _create_order(self, body: bytes) -> Response:
        if body:
            try:
                payload = json.loads(body)
            except (UnicodeDecodeError, json.JSONDecodeError):
                return self._json(400, {"error": "request body must be valid JSON"})
            if not isinstance(payload, dict):
                return self._json(400, {"error": "request body must be a JSON object"})

        profile = self.state.profile()
        if profile.mode == "latency":
            LOGGER.warning("database connection slow")
        elif profile.mode == "db-pool":
            LOGGER.error("database pool exhausted")
        elif profile.mode == "errors":
            LOGGER.error("simulated order processing error")

        self.sleeper(profile.delay_seconds)
        if profile.error is not None:
            return self._json(
                profile.order_status, {"error": profile.error, "mode": profile.mode}
            )

        order_id = self.state.next_order_id()
        LOGGER.info("order accepted order_id=%s", order_id)
        return self._json(profile.order_status, {"id": order_id, "status": "accepted"})

    def _update_pool_metrics(self, profile: FaultProfile) -> None:
        self.metrics.set_db_pool(
            profile.db_pool_active, profile.db_pool_idle, profile.db_pool_waiting
        )

    @staticmethod
    def _metric_route(path: str) -> str:
        if path.startswith("/debug/fault/"):
            return "/debug/fault/:mode"
        if path in ("/health", "/metrics", "/orders"):
            return path
        return "unmatched"

    @staticmethod
    def _json(status: int, payload: dict[str, object]) -> Response:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8") + b"\n"
        return Response(status, body, "application/json; charset=utf-8")


class SyntheticTraffic:
    """Produces bounded, regular order traffic so Prometheus rates are observable."""

    def __init__(
        self,
        application: Application,
        requests_per_second: float = 2.0,
        max_in_flight: int = 16,
    ):
        if requests_per_second <= 0:
            raise ValueError("requests_per_second must be positive")
        if max_in_flight <= 0:
            raise ValueError("max_in_flight must be positive")
        self.application = application
        self.interval_seconds = 1.0 / requests_per_second
        self._available = threading.BoundedSemaphore(max_in_flight)
        self._stopped = threading.Event()
        self._scheduler: threading.Thread | None = None

    def start(self) -> None:
        if self._scheduler is not None:
            raise RuntimeError("synthetic traffic is already started")
        self._scheduler = threading.Thread(
            target=self._run, name="synthetic-traffic", daemon=True
        )
        self._scheduler.start()

    def stop(self) -> None:
        self._stopped.set()
        if self._scheduler is not None:
            self._scheduler.join(timeout=max(1.0, self.interval_seconds * 2))

    def generate_once(self) -> None:
        self.application.handle("POST", "/orders", b"{}")

    def _run(self) -> None:
        while not self._stopped.is_set():
            if self._available.acquire(blocking=False):
                threading.Thread(
                    target=self._generate_and_release,
                    name="synthetic-order",
                    daemon=True,
                ).start()
            self._stopped.wait(self.interval_seconds)

    def _generate_and_release(self) -> None:
        try:
            self.generate_once()
        finally:
            self._available.release()


class _OrderServiceHandler(BaseHTTPRequestHandler):
    application: Application
    protocol_version = "HTTP/1.1"
    server_version = "order-service/0.1"

    def do_GET(self) -> None:
        self._serve()

    def do_POST(self) -> None:
        self._serve()

    def do_DELETE(self) -> None:
        self._serve()

    def do_PATCH(self) -> None:
        self._serve()

    def do_PUT(self) -> None:
        self._serve()

    def _serve(self) -> None:
        try:
            content_length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self.send_error(400, "invalid Content-Length")
            return
        if content_length < 0:
            self.send_error(400, "invalid Content-Length")
            return
        if content_length > 1_048_576:
            self.send_error(413, "request body too large")
            return

        body = self.rfile.read(content_length) if content_length else b""
        response = self.application.handle(self.command, self.path, body)
        self.send_response(response.status)
        self.send_header("Content-Type", response.content_type)
        self.send_header("Content-Length", str(len(response.body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(response.body)

    def log_message(self, message: str, *args: object) -> None:
        LOGGER.info("http " + message, *args)


def create_server(
    host: str,
    port: int,
    *,
    state: OrderServiceState | None = None,
    metrics: MetricsRegistry | None = None,
    application: Application | None = None,
) -> ThreadingHTTPServer:
    if application is not None and (state is not None or metrics is not None):
        raise ValueError("pass application or state/metrics, not both")
    application = application or Application(state=state, metrics=metrics)

    class Handler(_OrderServiceHandler):
        pass

    Handler.application = application
    return ThreadingHTTPServer((host, port), Handler)


def _escape_label(value: str) -> str:
    return value.replace("\\", "\\\\").replace("\n", "\\n").replace('"', '\\"')


def main() -> None:
    logging.basicConfig(
        level=logging.INFO, format="%(levelname)s %(message)s", stream=sys.stdout
    )
    host = os.environ.get("ORDER_SERVICE_HOST", "0.0.0.0")
    port = int(os.environ.get("ORDER_SERVICE_PORT", "8080"))
    latency = float(os.environ.get("ORDER_SERVICE_LATENCY_SECONDS", "2.4"))
    traffic_rps = float(os.environ.get("ORDER_SERVICE_TRAFFIC_RPS", "2"))
    max_in_flight = int(os.environ.get("ORDER_SERVICE_MAX_IN_FLIGHT", "16"))
    application = Application(state=OrderServiceState(latency))
    server = create_server(host, port, application=application)
    traffic = None
    if traffic_rps > 0:
        traffic = SyntheticTraffic(application, traffic_rps, max_in_flight)
        traffic.start()
        LOGGER.info("synthetic traffic started requests_per_second=%s", traffic_rps)
    LOGGER.info("listening host=%s port=%d", host, port)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        if traffic is not None:
            traffic.stop()
        server.server_close()


if __name__ == "__main__":
    main()
