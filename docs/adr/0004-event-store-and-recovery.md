# ADR-0004: Event 事实源、存储演进与恢复语义

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.1+`，主要在 `v0.2` 和 `v0.5` 实施

## Context

`v0.1` 使用每 Thread append-only JSONL，简单、可读并支持 SSE replay，但每次 append 重读全文件、
全局串行，且 active Turn/Approval 只在内存。长 Thread、fork、resume 和写操作恢复需要事务、索引、
lease 和 checkpoint。

外部系统无法普遍提供 exactly-once。进程可能在操作已提交但 result 未落盘时崩溃，简单重试会重复
副作用。

## Decision

1. 持久 Domain Event 是事实源；Thread/Turn/Item/Evidence 视图是可重建投影。
2. Assistant delta/进度等 Delivery Event 不具备权威状态语义，但也必须先持久化再广播，保证执行中
   SSE 重连。完成 Item 落盘并经过兼容 retention 后可清理 delivery payload，`seq` 不复用。
3. 通过 `EventStore` port 隔离存储；`v0.1-v0.4` 保留 JSONL backend。
4. `v0.5` 使用 SQLite WAL 作为默认 store，事务写 Event、checkpoint、approval、lease 和投影。
5. JSONL 保留为兼容输入、备份和导出格式；迁移必须 dry-run、hash 验证和可回滚。
6. EventEnvelope v2 增加 schema/event/item/causation/workspace identity，`seq` 继续 Thread 内单调。
7. model/tool 边界写 checkpoint；active Turn 使用本地 lease 和 fencing token 防双执行。
8. 恢复为 at-least-once：只读且明确 retryable 的 operation 可用稳定 ID 重试。
9. 已启动但结果未知的 change operation 进入 `NeedsReconciliation`，禁止自动重试。
10. 多实例、PostgreSQL、消息队列和分布式 lease 不在 `v1.0` 范围内。

## Consequences

正面：历史、SSE、恢复和审计共享事实；SQLite 解决长日志索引和原子状态；外部副作用语义诚实。

代价：需要 event migration、projection rebuild、crash fault matrix 和 store contract suite；SQLite
文件必须防多进程误用；开发者需要区分 Event、Item 和 delivery delta。

## Alternatives considered

- 永久使用 JSONL：无法高效事务化 checkpoint/approval 和长历史，拒绝。
- 只保存最新 snapshot：丢失审计、replay 和迁移依据，拒绝。
- 立即 PostgreSQL/Kafka：违背本地优先和最小基础设施，拒绝。
- 宣称 exactly-once Tool：外部系统无法统一保证，拒绝。
- 崩溃后一律重试：对 change operation 不安全，拒绝。

## Enforcement and verification

- JSONL 和 SQLite 必须通过同一 append/read/replay/projection contract tests。
- 每个持久化边界必须有 kill-process fault injection。
- 迁移比较 Thread/Event count、seq、ID 和 content hash，并保留只读 backup。
- change operation 在未知 commit 状态下的执行次数断言必须保持为 1 或进入 reconciliation。
- Event schema 变化需要 migration fixture 和 Rust/Web compatibility tests。
