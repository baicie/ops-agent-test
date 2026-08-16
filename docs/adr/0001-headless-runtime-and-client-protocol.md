# ADR-0001: Headless Runtime 与客户端协议边界

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.1+`

## Context

OpsCodex 同时提供 CLI、React Web，未来还可能有外部 API client。若 Runtime 依赖 Axum DTO、
React 状态或某个 transport，Agent Loop 会随客户端变化，无法独立测试和复用。

`v0.1` 已证明 App Server 可以把 Runtime Event 映射为 REST/SSE，但协议没有版本，Runtime 仍
直接接收 broadcast sender。需要明确长期边界和何时拆 crate。

## Decision

1. Agent Runtime 是 headless application core，不依赖 UI、Axum DTO 或网络 transport。
2. App Server 把版本化 REST command 映射为 Runtime command，把 EventStore 投影映射为 SSE。
3. CLI 可以直接嵌入 Runtime；Web 和外部 client 使用同一 REST/SSE 语义。
4. REST + SSE 保持到 `v1.0`。不为了模仿其他 Agent 产品切换 JSON-RPC、WebSocket 或多 transport。
5. SSE 为 at-least-once；`seq` 是 Thread 内游标，客户端按 `(thread_id, seq)` 去重。
6. `v0.2` 开始新接口使用 `/api/v1`，旧无版本路由保留一个兼容周期。
7. 在第二个需要独立发布的 Rust consumer 出现前保持单 crate；触发后再拆
   `opscodex-core`、`opscodex-protocol`、`opscodex-server`。
8. Runtime 通过 EventSink/EventStore port 发布结果，不直接持有 transport sender。

## Consequences

正面：Runtime 可用 Fake Model/Tool 独立测试；CLI 和 Web 共享状态语义；协议可以稳定演进。

代价：Server 需要明确 DTO mapping 和契约测试；Rust/TypeScript schema drift 必须由生成或 CI
检查；at-least-once 要求所有客户端 reducer 幂等。

## Alternatives considered

- UI 直接调用 Runtime：耦合客户端生命周期，拒绝。
- CLI 也强制经过 Server：增加本地调试和故障面，拒绝。
- 现在改为 JSON-RPC/WebSocket：没有当前需求能抵消迁移成本，拒绝。
- 立即拆 Cargo workspace：当前只有一个发布 binary，过早拆分，拒绝。

## Enforcement and verification

- Runtime crate/module 不得 import Axum 或 Web DTO。
- API/Event fixtures 同时验证 Rust producer 和 TypeScript consumer。
- SSE replay/live handoff、lag recovery、Last-Event-ID 和重复 event 必须有测试。
- 新客户端不得定义绕过 Runtime command/event 的私有执行路径。
