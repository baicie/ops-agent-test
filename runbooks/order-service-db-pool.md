---
id: order-service-db-pool
title: Order service DB pool exhaustion
services: [order-service]
signals: [db_pool_waiting, database_pool_exhausted]
tags: [database, latency]
version: 1
---

# Order service DB pool exhaustion

This runbook is a local reference. Commands here are suggestions and are never
executed automatically by OpsCodex.

## Signals

- Prometheus `db_pool_waiting` rising
- Logs containing `database pool exhausted`
- Checkout 5xx and latency increase

## Next steps

1. Confirm the workload and Kubernetes events in the current Workspace only.
2. Check downstream latency before changing pool size.
3. After a human-approved change, re-query metrics and logs.
