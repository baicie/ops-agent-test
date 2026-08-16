# Architecture Decision Records

本目录记录 OpsCodex 已接受的关键架构决策。ADR 一旦 Accepted 就不直接重写结论；如果决策
变化，新增 ADR 并在旧文档顶部标记 `Superseded by ADR-XXXX`。

## 状态

- `Proposed`：方案可评审，尚不能作为实现约束。
- `Accepted`：当前实现和后续阶段必须遵守。
- `Deprecated`：仍用于兼容，但不再用于新设计。
- `Superseded`：已被后续 ADR 替代。

## 索引

| ADR | 状态 | 决策 | 主要阶段 |
| --- | --- | --- | --- |
| [0001](0001-headless-runtime-and-client-protocol.md) | Accepted | Headless Runtime 与 REST/SSE 客户端边界 | v0.1、v1.0 |
| [0002](0002-bounded-single-agent-runtime.md) | Accepted | Thread/Turn/Item/Event 与有界单 Agent | v0.1、全部阶段 |
| [0003](0003-provider-neutral-model-boundary.md) | Accepted | Provider-neutral 模型协议 | v0.1、v0.2 |
| [0004](0004-event-store-and-recovery.md) | Accepted | Event 事实源、SQLite 与恢复语义 | v0.2、v0.5 |
| [0005](0005-evidence-and-claims.md) | Accepted | Evidence 一等公民和 Claim 引用 | v0.2、全部阶段 |
| [0006](0006-workspace-and-target-boundary.md) | Accepted | Workspace 是运行环境信任边界 | v0.3、全部阶段 |
| [0007](0007-extension-trust-boundary.md) | Accepted | MCP/Custom Tool/Skill 不可信扩展边界 | v0.4 |
| [0008](0008-capability-policy-and-remediation.md) | Accepted | Capability Policy 与安全 remediation | v0.3、v0.6 |
| [0009](0009-observability-and-data-handling.md) | Accepted | Product Event、telemetry、audit 分离 | v0.2、v1.0 |
| [0010](0010-scenario-evaluation-gate.md) | Accepted | 场景化评测和发布门禁 | 全部阶段 |
| [0011](0011-no-chain-of-thought-persistence.md) | Accepted | 不持久化或展示 Chain of Thought | 全部阶段 |

## 新增 ADR 的条件

满足任一条件时需要 ADR：

- 改变 Runtime、客户端、存储、Provider、Tool 或 Policy 的长期边界。
- 引入一种新的信任边界、外部副作用或恢复语义。
- 改变事件/API/持久化兼容承诺。
- 在两个都有明显后果的方案之间做不可轻易逆转的选择。

普通实现细节、可局部替换的 library 和单一 endpoint 不需要 ADR，写入阶段设计和任务即可。

## ADR 模板

```markdown
# ADR-XXXX: 标题

- Status: Proposed
- Date: YYYY-MM-DD
- Deciders: OpsCodex maintainers
- Applies to: version/phase

## Context
## Decision
## Consequences
## Alternatives considered
## Enforcement and verification
```
