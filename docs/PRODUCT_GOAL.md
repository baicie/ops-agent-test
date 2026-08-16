# OpsCodex 最终产品目标

- 状态：Approved baseline
- 目标版本：`v1.0`
- 最近更新：2026-08-16

## 一句话定义

OpsCodex 是一个本地优先的 AIOps Agent Runtime：它像 Coding Agent 调查代码仓库一样，
通过受约束的工具调查运行环境，并产出可追溯、可复核、可安全执行后续动作的故障结论。

## 最终目标

到 `v1.0`，单个运维工程师应能把一个告警或自然语言问题交给 OpsCodex，随后由一个
有界、可中断、可恢复的 Agent 自主完成：

```text
Incident context
  -> choose observations
  -> collect metrics/logs/traces/runtime state/runbooks
  -> normalize and correlate evidence
  -> produce evidence-linked diagnosis
  -> optionally propose a bounded remediation plan
  -> execute only after exact, durable approval
  -> verify the result and preserve an audit trail
```

这里的“自主”只覆盖调查和计划。任何改变运行环境的动作都必须经过 Policy 强制、参数绑定
的人工批准和执行后验证；模型不能自行扩大目标、权限或 blast radius。

## 目标用户与核心问题

主要用户是需要在本地或受控跳板环境中调查服务故障的 SRE、平台工程师和后端工程师。
产品解决三个问题：

1. 观测数据分散，人工需要在指标、日志、Trace、Kubernetes 和 Runbook 之间来回切换。
2. 普通 Chatbot 会给出无法证明的判断，无法区分事实、推断和建议。
3. 诊断和变更工具缺少统一的范围、审批、取消、恢复和审计语义。

## 北极星结果

成功不是“支持多少数据源”，而是同一个 Runtime 能稳定完成以下结果：

- 用最少且相关的工具调用收集足够证据。
- 每个关键诊断 Claim 都引用真实 Evidence ID，用户可展开查看原始来源和时间范围。
- 证据不足时明确 abstain，并说明缺少什么，而不是补全一个听起来合理的答案。
- 网络断开或进程重启后，历史可重放；执行可从安全 checkpoint 恢复或进入人工对账。
- 未经批准的变更操作始终为零。
- CLI、Web 和版本化 API 看到相同的 Thread、Turn、Evidence、Approval 和终态。

## 产品原则

### Runtime First

Thread、Turn、Item、Event、Context、Model、Tool、Policy、Evidence、Cancellation 是产品核心。
UI、模型供应商和数据源都是适配器，不能反向定义 Runtime 领域模型。

### Local First

默认部署是一个 Rust 进程和静态 Web 资源，状态保存在用户控制的本地目录。默认只监听
loopback。`v1.0` 不要求云控制面、多租户或高可用集群。

### Evidence First

工具输出不是聊天附件，而是一等领域对象。诊断必须区分 Observation、Inference、Claim 和
Recommendation，并保持从 Claim 到 Evidence 的可追溯关系。

### Read-only by Default

结构化调查工具默认只读。`exec` 是默认关闭、每次审批的开发逃生口，不属于安全修复保证。
从 `v0.6` 开始只允许经过结构化计划、精确批准和验证的 remediation 工具改变环境。

### Bounded and Recoverable

每个 Turn 都有 step、时间、输出、并发和成本预算，并且恰好进入一个终态。外部副作用不
宣称 exactly-once；不确定状态必须进入 `NeedsReconciliation`，禁止盲目重试。

### Minimal Infrastructure

在单进程和嵌入式存储足够时，不引入消息队列、工作流引擎、微服务或分布式协调系统。
新组件必须直接增强 Agent Loop 的正确性、证据质量、安全性或恢复能力。

## v1.0 范围

### 必须具备

- 一个 Rust 自研的单 Agent Runtime，不依赖 Agent Framework 或 Workflow Graph。
- Provider-neutral 模型协议以及通过契约测试的 Responses-compatible Provider。
- Metrics、Logs、Trace、HTTP、Docker、Kubernetes 和本地 Runbook 的结构化调查能力。
- Alert/Incident Context、Workspace 目标边界和凭据引用。
- 带稳定 ID、来源、时间窗、内容哈希和敏感级别的 Evidence。
- 诊断 Claim 与 Evidence 的机器可读引用关系。
- 有界上下文、摘要压缩、Thread fork、安全 checkpoint 和进程重启恢复。
- 受约束的 MCP、外部 Custom Tool 和 Skill 扩展边界。
- 结构化 remediation 的 plan/approve/execute/verify/reconcile 生命周期。
- CLI、Web、版本化 REST API 和可重连 SSE。
- 运行指标、脱敏日志、安全审计、升级迁移和回滚说明。

### 明确不做

- SaaS、多租户、用户系统、RBAC 或互联网暴露的控制面。
- 替代 Prometheus、Loki、Tempo、告警平台或 CMDB。
- Dashboard Builder、通用 Workflow Engine、Knowledge Graph 或 Vector DB。
- Kubernetes Operator、无人值守自动修复或任意命令自治。
- Multi-Agent、Planner/Supervisor Agent 或 Agent Team。
- 持久化、展示或要求模型提供 Chain of Thought。

这些能力如果未来需要，必须在 `v1.0` 稳定后通过新的产品目标和 ADR 重新立项。

## 工作空间定义

`Workspace` 是一个运行环境及其信任边界，包含：

- 环境标识，例如 `local-demo`、`staging` 或 `production-eu`。
- 可用 Provider、Connector、Tool 和 Skill。
- 目标 allowlist、凭据引用、数据保留和脱敏策略。
- Policy profile 和最大并发/成本预算。

一个 Thread 只能属于一个 Workspace。跨 Workspace 的证据合并或操作不在 `v1.0` 范围内。
本地模式提供一个隐式 `default` Workspace，以保持 `v0.1` 配置兼容。

## 可测的 v1.0 完成标准

### 诊断质量

- 发布场景集至少覆盖数据库池耗尽、服务错误激增、延迟回归、Kubernetes 工作负载失败、
  下游依赖故障五类确定性事故。
- 每个场景都必须得到正确的主诊断或明确 abstain；评分不依赖整段文本精确匹配。
- 关键 Claim 的 Evidence 引用有效率为 100%，无来源 Claim 数为 0。
- 模型未返回足够证据时，系统不能把推断升级为已确认事实。

### 安全

- 默认配置中所有自治工具均为只读，`exec` 不注册。
- 未绑定 Workspace、目标、工具版本、参数哈希、有效期和 operation ID 的批准不能执行。
- 越权目标、SSRF、DNS rebinding、命令注入、审批重放和 Prompt Injection 场景全部被拒绝。
- Secret、原始敏感 Evidence 和 Prompt 内容默认不进入 telemetry 或发布产物。

### 连续性与一致性

- API 成功确认后的领域事件在故障注入测试中不丢失。
- SSE 断线重连按 `(thread_id, seq)` 至少一次投递，客户端投影无重复结果。
- 每个持久化边界发生进程崩溃时，Turn 能安全恢复、失败或进入人工对账，不能双重执行写操作。
- JSONL 到 SQLite 的迁移可重复、可校验，并支持导出可读事件记录。

### 可用性与运维

- CLI 和 Web 都能完成调查、Evidence 展开、取消、审批、恢复和 fork 主路径。
- 在 4 个并发 Turn 下，排除外部模型/工具耗时后的本地控制面 p95 延迟小于 100 ms。
- `/healthz`、readiness 和 Runtime metrics 能区分存储、Provider、Connector 与队列故障。
- 发布包含安装、配置、数据迁移、备份、回滚、安全边界和故障排查文档。

## 假设

- `v1.0` 仍以单操作者、单进程为部署单位，不为多个不互信用户提供服务。
- 可观测后端和 Kubernetes 已由用户部署；OpsCodex 只查询，不负责采集或存储它们的数据。
- 第一个 Trace 实现以 Tempo HTTP 查询接口为目标；OpsCodex 不实现 OTLP Collector。
- Runbook 是用户控制的本地 Markdown 文件，不引入向量检索。
- 真实 Provider 的可用性和模型能力可能变化，因此发布依赖能力探测和契约测试，而非品牌判断。

## 完成定义

只有 [路线图](ROADMAP.md) 中 `v0.1` 至 `v1.0` 的阶段门禁全部通过、所有 Accepted ADR 与
实现一致、发布场景集和安全门禁通过并正式创建 `v1.0.0` Release，才能宣称最终目标完成。
