# ADR-0008: Capability Policy 与结构化安全 Remediation

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.3+`，写操作从 `v0.6` 开始

## Context

`v0.1` 的 `Safe/Ask/Forbidden` 只按 Tool 静态判断；它不能区分 namespace、目标、参数、读写 effect、
blast radius 或 operation recovery。用户点击 Allow 也不能证明执行参数与屏幕显示完全一致。

安全 remediation 需要比“模型建议 + 用户同意”更强的状态、授权和恢复保证。

## Decision

1. Tool descriptor 声明 effect：observe、change_reversible、change_irreversible、
   external_side_effect；未知 effect 默认最严格并拒绝。
2. Policy 输入包含 Workspace、operator、Tool/version/schema、规范化目标/参数、effect、预算、时间和
   当前 operation；提示词不是决策输入的唯一来源。
3. 默认 deny。observe 可按 Workspace allow；change 在 `v1.0` 前始终需要精确人工批准。
4. Approval 绑定 Workspace、Tool/version/schema hash、target、parameters、preconditions、verification、
   operation ID、expiry 和 approver，且只能消费一次。
5. Remediation 必须使用 ActionPlan -> dry-run -> approval -> precondition -> execute -> verify ->
   reconcile 状态机。
6. 写操作使用独立最小权限 runner、kill switch、idempotency/reconciliation contract 和 blast-radius
   budget。
7. 执行结果未知时进入 NeedsReconciliation，不自动重试。Rollback 是新的 Action，需重新批准。
8. `exec` 是默认关闭的开发 escape hatch，不属于安全 remediation，也不能被 Runbook/Skill 包装绕过。
9. 首批仅允许明确的结构化 reversible action；不可逆操作不在 `v1.0` 范围。

## Consequences

正面：批准内容与实际执行可验证；参数替换、重放、越界和 TOCTOU 有明确控制；调查与变更权分离。

代价：新增 Action/Approval/Audit 状态和 runner；Tool 作者必须实现 effect、dry-run、precondition、
verify 和 recovery metadata；可用 remediation 数量会刻意很少。

## Alternatives considered

- 继续三级静态风险：无法表达参数和目标，拒绝。
- 所有写操作都走 exec + Approval：没有结构化验证、最小权限或恢复，拒绝。
- 用户批准后自动重试：可能重复副作用，拒绝。
- 自动 rollback：rollback 也可能危险且目标已变化，拒绝。
- 仅依赖 Kubernetes RBAC：不能绑定用户看到的具体意图，拒绝。

## Enforcement and verification

- request hash 与执行参数必须逐字段重算；swap/replay/expiry/version change tests 必须失败。
- production-safe profile mutation count 在无批准场景中必须为 0。
- 每个写 Tool 有 precondition、idempotency/reconciliation 和 before/after Evidence tests。
- kill switch、crash matrix、Prompt Injection 和跨 Workspace 攻击进入 release gate。
