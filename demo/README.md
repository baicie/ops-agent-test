# OpsCodex demo

This demo runs exactly two containers: the deterministic `order-service` fault
simulator and Prometheus. The service generates two orders per second by
default so rate and latency queries have fresh samples without a third load
generator.

## Start

```sh
docker compose -f demo/docker-compose.yml up --build
```

The service is available at `http://localhost:8080` and Prometheus at
`http://localhost:9090`.

## Inject a fault

```sh
curl -X POST http://localhost:8080/debug/fault/db-pool
curl http://localhost:8080/health
curl http://localhost:8080/metrics
docker logs --tail 50 order-service
```

The supported modes are `normal`, `errors`, `latency`, and `db-pool`. Switching
back to normal is deterministic and does not reset accumulated metrics:

```sh
curl -X POST http://localhost:8080/debug/fault/normal
```

`ORDER_SERVICE_LATENCY_SECONDS`, `ORDER_SERVICE_TRAFFIC_RPS`, and
`ORDER_SERVICE_MAX_IN_FLIGHT` tune the demo. Set traffic RPS to `0` to disable
the in-process workload.

Useful PromQL queries:

```promql
sum(rate(http_requests_total{service="order-service",route="/orders",status=~"5.."}[1m]))
/
sum(rate(http_requests_total{service="order-service",route="/orders"}[1m]))

histogram_quantile(0.95,
  sum by (le) (rate(http_request_duration_seconds_bucket{service="order-service",route="/orders"}[1m]))
)
```

## Test without Docker

```sh
python3 -m unittest discover -s demo/order-service/tests -v
```
