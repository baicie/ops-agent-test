import json
import sys
import threading
import unittest
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path


SERVICE_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SERVICE_DIR))

from app import (  # noqa: E402 - service directory name is fixed by the demo design.
    Application,
    MetricsRegistry,
    OrderServiceState,
    SyntheticTraffic,
    create_server,
)


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def decode_json(response):
    return json.loads(response.body.decode("utf-8"))


class ApplicationTests(unittest.TestCase):
    def setUp(self):
        self.clock = FakeClock()
        self.state = OrderServiceState(latency_seconds=2.4)
        self.metrics = MetricsRegistry()
        self.app = Application(
            state=self.state,
            metrics=self.metrics,
            clock=self.clock.monotonic,
            sleeper=self.clock.sleep,
        )

    def set_fault(self, mode):
        response = self.app.handle("POST", f"/debug/fault/{mode}", b"")
        self.assertEqual(200, response.status)
        self.assertEqual(mode, decode_json(response)["mode"])

    def test_normal_mode_accepts_orders_and_reports_healthy(self):
        order = self.app.handle("POST", "/orders", b"{}")
        health = self.app.handle("GET", "/health", b"")

        self.assertEqual(201, order.status)
        self.assertEqual(
            {"id": "order-000001", "status": "accepted"},
            decode_json(order),
        )
        self.assertEqual(
            {"status": "ok", "mode": "normal"},
            decode_json(health),
        )

    def test_errors_mode_returns_a_deterministic_server_error(self):
        self.set_fault("errors")

        response = self.app.handle("POST", "/orders", b"{}")

        self.assertEqual(500, response.status)
        self.assertEqual(
            {"error": "simulated order processing error", "mode": "errors"},
            decode_json(response),
        )
        self.assertEqual(0.0, self.clock.now)

    def test_latency_mode_delays_then_accepts_the_order(self):
        self.set_fault("latency")

        response = self.app.handle("POST", "/orders", b"{}")

        self.assertEqual(201, response.status)
        self.assertEqual(2.4, self.clock.now)
        self.assertEqual("accepted", decode_json(response)["status"])

    def test_db_pool_mode_is_slow_errors_and_logs_the_root_cause(self):
        self.set_fault("db-pool")

        with self.assertLogs("order-service", level="ERROR") as logs:
            response = self.app.handle("POST", "/orders", b"{}")

        self.assertEqual(500, response.status)
        self.assertEqual(2.4, self.clock.now)
        self.assertEqual(
            {"error": "database pool exhausted", "mode": "db-pool"},
            decode_json(response),
        )
        self.assertTrue(
            any("database pool exhausted" in message for message in logs.output)
        )

    def test_fault_mode_controls_health_and_pool_gauges(self):
        self.set_fault("db-pool")

        health = self.app.handle("GET", "/health", b"")
        metrics = self.app.handle("GET", "/metrics", b"").body.decode("utf-8")

        self.assertEqual(
            {"status": "degraded", "mode": "db-pool"},
            decode_json(health),
        )
        self.assertIn('db_pool_active{service="order-service"} 10', metrics)
        self.assertIn('db_pool_idle{service="order-service"} 0', metrics)
        self.assertIn('db_pool_waiting{service="order-service"} 12', metrics)

    def test_metrics_include_counter_and_histogram_for_order_requests(self):
        self.set_fault("latency")
        self.app.handle("POST", "/orders", b"{}")

        metrics = self.app.handle("GET", "/metrics", b"").body.decode("utf-8")

        labels = 'method="POST",route="/orders",service="order-service",status="201"'
        self.assertIn(f"http_requests_total{{{labels}}} 1", metrics)
        self.assertIn(
            'http_request_duration_seconds_bucket{le="2.5",' + labels + "} 1",
            metrics,
        )
        self.assertIn(f"http_request_duration_seconds_count{{{labels}}} 1", metrics)
        self.assertIn(f"http_request_duration_seconds_sum{{{labels}}} 2.4", metrics)

    def test_metrics_expose_zero_order_series_before_the_first_request(self):
        metrics = self.app.handle("GET", "/metrics", b"").body.decode("utf-8")

        for status in ("201", "500"):
            labels = (
                'method="POST",route="/orders",service="order-service",'
                f'status="{status}"'
            )
            self.assertIn(f"http_requests_total{{{labels}}} 0", metrics)
            self.assertIn(f"http_request_duration_seconds_count{{{labels}}} 0", metrics)

    def test_concurrent_orders_do_not_lose_metric_updates(self):
        with ThreadPoolExecutor(max_workers=10) as executor:
            list(
                executor.map(
                    lambda _: self.app.handle("POST", "/orders", b"{}"),
                    range(50),
                )
            )

        metrics = self.app.handle("GET", "/metrics", b"").body.decode("utf-8")
        self.assertIn(
            'http_requests_total{method="POST",route="/orders",'
            'service="order-service",status="201"} 50',
            metrics,
        )

    def test_switching_back_to_normal_restores_pool_without_resetting_metrics(self):
        self.set_fault("db-pool")
        self.app.handle("POST", "/orders", b"{}")
        self.set_fault("normal")

        metrics = self.app.handle("GET", "/metrics", b"").body.decode("utf-8")

        self.assertIn('db_pool_active{service="order-service"} 2', metrics)
        self.assertIn('db_pool_idle{service="order-service"} 8', metrics)
        self.assertIn('db_pool_waiting{service="order-service"} 0', metrics)
        self.assertIn(
            'http_requests_total{method="POST",route="/orders",'
            'service="order-service",status="500"} 1',
            metrics,
        )

    def test_synthetic_traffic_uses_the_same_order_path(self):
        self.set_fault("errors")
        traffic = SyntheticTraffic(self.app, requests_per_second=2.0)

        traffic.generate_once()

        metrics = self.app.handle("GET", "/metrics", b"").body.decode("utf-8")
        self.assertIn(
            'http_requests_total{method="POST",route="/orders",'
            'service="order-service",status="500"} 1',
            metrics,
        )

    def test_invalid_json_and_unknown_fault_modes_are_rejected(self):
        invalid_order = self.app.handle("POST", "/orders", b"not-json")
        unknown_fault = self.app.handle("POST", "/debug/fault/random", b"")

        self.assertEqual(400, invalid_order.status)
        self.assertEqual(404, unknown_fault.status)


class HttpIntegrationTests(unittest.TestCase):
    def setUp(self):
        state = OrderServiceState(latency_seconds=0.0)
        self.server = create_server("127.0.0.1", 0, state=state)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def request(self, method, path, body=None):
        data = body.encode("utf-8") if body is not None else None
        request = urllib.request.Request(
            self.base_url + path,
            data=data,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=2) as response:
                return response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            try:
                return error.code, error.headers, error.read()
            finally:
                error.close()

    def test_http_server_exposes_the_complete_db_pool_incident(self):
        status, _, body = self.request("POST", "/debug/fault/db-pool")
        self.assertEqual(200, status)
        self.assertEqual("db-pool", json.loads(body)["mode"])

        status, _, body = self.request("POST", "/orders", "{}")
        self.assertEqual(500, status)
        self.assertEqual("database pool exhausted", json.loads(body)["error"])

        status, _, body = self.request("GET", "/health")
        self.assertEqual(200, status)
        self.assertEqual("degraded", json.loads(body)["status"])

        status, headers, body = self.request("GET", "/metrics")
        self.assertEqual(200, status)
        self.assertIn("text/plain", headers["Content-Type"])
        self.assertIn(b"http_requests_total", body)
        self.assertIn(b"db_pool_waiting", body)

    def test_http_server_returns_405_for_a_known_route_with_the_wrong_method(self):
        status, _, body = self.request("PUT", "/orders", "{}")

        self.assertEqual(405, status)
        self.assertEqual("method not allowed", json.loads(body)["error"])


if __name__ == "__main__":
    unittest.main()
