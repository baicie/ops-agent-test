# ADR-0006: Workspace 是运行环境与目标信任边界

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.3+`

## Context

`v0.1` 使用全局 Prometheus URL、host/container allowlist，足够调查单个 Demo，但无法表达 staging
和 production、多个 cluster、不同 credentials、不同 retention 或 Policy。Kubernetes 和扩展如果
没有先建立环境边界，会把一个 Thread 的权限扩散到所有目标。

另一方面，项目明确不在 `v1.0` 做 SaaS、多租户或 RBAC，不能为了假想云控制面提前加入 Tenant
和分布式 worker。

## Decision

1. `Workspace` 是一个运行环境、连接能力、凭据引用、预算、retention 和 Policy 的信任边界。
2. 一个 Thread 创建时绑定一个 Workspace，生命周期内不可切换；不支持跨 Workspace 调查。
3. 本地兼容模式提供隐式 `default` Workspace，旧配置和 Thread 自动映射到它。
4. Event、Evidence、Artifact、Tool operation、Approval 和 Audit 都带 Workspace scope。
5. Tool target 必须经过规范化、解析后 allowlist 和 capability check；全局规则只能进一步收紧。
6. credentials 只保存引用并在 adapter/runner 最后一刻注入，不进入模型、Event 或 UI。
7. 本地无认证服务只能监听 loopback。非 loopback 需要独立认证/TLS 设计，不在 `v1.0` 范围。
8. `v1.0` 不引入 Tenant、用户、RBAC、跨 Workspace 数据或托管控制面。

## Consequences

正面：每次调查和工具调用都有清晰 blast radius；Kubernetes、MCP 和 remediation 可以复用同一
scope；仍保持本地单进程简单性。

代价：Thread 不能临时查询另一个环境；配置和所有领域 ID 需要 scope migration；用户需要显式
选择 Workspace；远程协作必须等待新的产品目标。

## Alternatives considered

- 保留全局 allowlist：无法隔离多个环境，拒绝。
- 现在加入 Tenant/RBAC：超出单操作者目标，拒绝。
- 每个 Workspace 启动完整独立进程：可以作为部署选择，但不应成为领域模型要求。
- 允许 Thread 动态切环境：Evidence 和批准的 scope 变得不清晰，拒绝。

## Enforcement and verification

- Store key、API path/authorization context、Policy input 和 Audit 必须包含 Workspace ID。
- 跨 Workspace ID、target、credential 和 artifact 访问必须有负向测试。
- DNS、Kubernetes GVK/namespace 和 MCP endpoint 在解析后再次检查。
- 未配置认证/TLS 的非 loopback bind 必须 fail closed。
