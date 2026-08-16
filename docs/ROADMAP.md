# OpsCodex 版本路线图

- 状态：Approved baseline
- 排期方式：按依赖和阶段门禁推进，不承诺日历日期。

## 总体进度

| 阶段 | 状态 | 核心结果 | 主要门禁 |
| --- | --- | --- | --- |
| Design Gate | 完成 | 最终目标、目标架构、阶段设计、ADR | 文档链接和追踪关系有效 |
| [`v0.1`](phases/v0.1-runtime-mvp.md) | 已发布 | Rust Runtime MVP 跑通真实诊断闭环 | CI、真实 Provider、Demo E2E |
| [`v0.2`](phases/v0.2-evidence-foundation.md) | 实现中 | 稳定协议和 Evidence 基础，接入 Logs/Trace/Alert | 多信号 Claim 引用与脱敏评测 |
| [`v0.3`](phases/v0.3-runtime-workspace.md) | 计划中 | Workspace、Kubernetes、Topology、Runbook | 只读 K8s 场景与目标隔离 |
| [`v0.4`](phases/v0.4-extensibility.md) | 计划中 | 受约束 MCP、Custom Tool、Skill | 不可信扩展生命周期与策略测试 |
| [`v0.5`](phases/v0.5-continuity.md) | 计划中 | SQLite、Compaction、Fork、Resume | 全崩溃边界恢复与迁移测试 |
| [`v0.6`](phases/v0.6-safe-remediation.md) | 计划中 | 结构化安全修复和持久审计 | 零未批准动作、重放/越权防护 |
| [`v1.0`](phases/v1.0-production-readiness.md) | 计划中 | 稳定本地生产版 | 全场景、安全、SLO、升级发布门禁 |

## 依赖关系

```mermaid
flowchart LR
    D["Design Gate"] --> V01["v0.1 Runtime MVP"]
    V01 --> V02["v0.2 Evidence Foundation"]
    V02 --> V03["v0.3 Runtime Workspace"]
    V03 --> V04["v0.4 Extensibility"]
    V04 --> V05["v0.5 Continuity"]
    V05 --> V06["v0.6 Safe Remediation"]
    V06 --> V10["v1.0 Production Ready"]
```

阶段必须顺序满足门禁。阶段内部可以并行的工作在详细设计中标注，但不能跳过前置领域合同：

- Evidence ID、敏感级别和上下文预算必须先于大规模 Logs/Trace 输出。
- Workspace 和 capability policy 必须先于 Kubernetes 与外部扩展。
- EventStore、checkpoint、持久审批和恢复语义必须先于任何写操作。
- 安全场景评测必须先于 remediation 发布。

## 每阶段结果

### Design Gate：设计基线

完成条件：

- 最终目标包含范围、非目标、假设和可测成功标准。
- 每个版本有独立详细设计、原子任务、风险和阶段门禁。
- 关键长期决策有 ADR，路线图能追踪到 ADR 和验收。
- 根 README 能发现全部设计文档。

### v0.1：Runtime MVP

已完成原始 Phase 0-7：Skeleton、Agent Runtime、真实 Responses Provider、AIOps Tools、
可重复 Demo、App Server、React UI、`exec` 审批。发布证明记录在
[v0.1 详细设计](phases/v0.1-runtime-mvp.md)。

### v0.2：Evidence Foundation

先修复未来扩展会放大的事实模型问题，再添加数据源：Event schema v2、稳定 Evidence/Claim、
Provider capability、上下文预算、敏感数据治理、Runtime metrics 和场景评测。随后接入 Loki、
Tempo 查询和 Alert Context。完整 Context summarization 留到 `v0.5`。

### v0.3：Runtime Workspace

把当前全局 allowlist 提升为 Workspace 范围，加入只读 Kubernetes 工具、带来源的服务拓扑和
本地 Markdown Runbook。Topology 是证据投影，不演变成 CMDB 或 Knowledge Graph。

### v0.4：Extensibility

在统一 Capability Descriptor 和 Policy 下接入 MCP、进程外 Custom Tool 与 Skill。扩展内容
不能自行获得凭据、降低风险或绕过预算；Skill 只提供指令和资源，不直接执行代码。

### v0.5：Continuity

将事实源迁移到 SQLite，持久化 Turn checkpoint、lease 和 approval，支持有证据保留规则的
Context Compaction、Thread Fork 和显式 Resume。外部副作用仅提供 at-least-once 恢复语义。

### v0.6：Safe Remediation

加入结构化 ActionPlan、参数绑定批准、最小权限执行器、dry-run、verify 和 reconciliation。
任意 `exec` 不得包装成安全 remediation。默认配置仍是观察模式。

### v1.0：Production Ready

冻结 `/api/v1`、完成迁移/备份/回滚、全场景评测、安全威胁测试、性能 SLO 和发布文档，证明
一个本地单进程实例可以长期承担受控生产调查和人工批准的有限修复。

## 目标追踪矩阵

| 最终目标能力 | 首次建立 | 完成阶段 | 关键 ADR |
| --- | --- | --- | --- |
| 有界单 Agent Loop | `v0.1` | `v0.1` | [ADR-0002](adr/0002-bounded-single-agent-runtime.md) |
| Provider-neutral 模型边界 | `v0.1` | `v0.2` capability 基线 | [ADR-0003](adr/0003-provider-neutral-model-boundary.md) |
| 可回放客户端协议 | `v0.1` | `v1.0` 稳定 API | [ADR-0001](adr/0001-headless-runtime-and-client-protocol.md) |
| Evidence-linked Diagnosis | `v0.1` | `v0.2` | [ADR-0005](adr/0005-evidence-and-claims.md) |
| Metrics/Logs/Trace/Alert | `v0.1` Metrics | `v0.2` | [ADR-0005](adr/0005-evidence-and-claims.md) |
| Workspace/K8s/Runbook | `v0.3` | `v0.3` | [ADR-0006](adr/0006-workspace-and-target-boundary.md) |
| MCP/Custom Tool/Skill | `v0.4` | `v0.4` | [ADR-0007](adr/0007-extension-trust-boundary.md) |
| Compaction/Fork/Resume | `v0.5` | `v0.5` | [ADR-0004](adr/0004-event-store-and-recovery.md) |
| Safe Remediation/Audit | `v0.6` | `v0.6` | [ADR-0008](adr/0008-capability-policy-and-remediation.md) |
| 隐私感知 telemetry | `v0.2` | `v1.0` | [ADR-0009](adr/0009-observability-and-data-handling.md) |
| 场景化发布门禁 | `v0.1` | `v1.0` | [ADR-0010](adr/0010-scenario-evaluation-gate.md) |
| 不持久化 Chain of Thought | `v0.1` | 持续约束 | [ADR-0011](adr/0011-no-chain-of-thought-persistence.md) |

## 全局阶段门禁

每个版本除自身验收外，还必须满足：

1. `cargo fmt --check`、Clippy、Rust/Web/Demo/验收测试和 production build 通过。
2. RustSec、cargo-deny、npm audit 没有违反发布策略的问题。
3. 事件协议、配置和持久化格式变化有兼容或迁移测试。
4. 新 Tool 具有 schema、effect、target scope、timeout、output bound、redaction 和失败测试。
5. 新 Provider 或 Connector 具有不使用真实 Secret 的契约测试。
6. 阶段场景测试和至少一个真实 Provider smoke gate 通过。
7. 文档、ADR、CHANGELOG、升级和回滚说明与实现一致。

## 进度维护

- 路线图状态只能在阶段门禁有可验证证据后更新。
- 新需求先归入一个阶段；如果改变目标、顺序或安全边界，先更新 ADR。
- 一个任务应在单个专注会话内完成，通常修改不超过 5 个文件，并写明 Acceptance 和 Verify。
- `Unreleased` 只记录已经实现的变化；尚未实现的目标留在本路线图和阶段设计。
