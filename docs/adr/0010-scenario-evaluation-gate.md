# ADR-0010: 场景化评测与发布门禁

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: 全部版本

## Context

Agent 输出具有非确定性，单元测试不能证明真实诊断质量；整段文本 golden match 又会把合理表达判为
失败。只运行一个 db-pool happy path 也不能覆盖 Trace、Kubernetes、扩展、安全和恢复回归。

真实 Provider gate 有价值，但受外部可用性、模型变化和成本影响，不能成为唯一测试。

## Decision

建立分层评测金字塔：

1. 领域/state machine/store/policy 的确定性单元和 property tests。
2. Provider、Connector、Tool 和协议 contract fixtures。
3. Fake Model 的完整 Event replay 和 UI projection tests。
4. Metrics/Logs/Trace/Alert/Kubernetes/Runbook 的 deterministic incident scenarios。
5. Prompt Injection、target escape、approval replay 和 crash recovery 安全 scenarios。
6. 少量受保护的真实 Provider/环境 smoke gate。

评分关注结构化结果：主诊断正确或合理 abstain、Evidence 引用有效、无来源 Claim、禁止动作数、
Tool 成功/失败、恢复状态、延迟和成本。LLM judge 只能辅助，不能是唯一裁判。

dataset、fault、prompt、Tool schema、Provider/model、scorer 和阈值全部版本化。每个阶段必须增加
自己的场景且保持前序场景通过。

## Consequences

正面：模型和架构变化可以客观比较；安全边界成为发布条件；表达变化不会导致脆弱的全文匹配。

代价：fixture 和 scorer 需要持续维护；真实 smoke gate 需要 Secret 和人工保护；某些定性判断需要
明确容许范围而不是单一字符串。

## Alternatives considered

- 只保留单元测试：无法证明端到端调查，拒绝。
- 只匹配最终答案文本：脆弱且不能验证隐藏副作用，拒绝。
- 完全依赖 LLM judge：不可重复且可能与被测模型同源偏差，拒绝。
- 每次 CI 调真实 Provider：不稳定、有成本和 Secret 风险，拒绝。

## Enforcement and verification

- 每个阶段设计必须列出 deterministic scenario 和 machine-verifiable gate。
- 发布报告记录 commit、config、model、tool schema、dataset/scorer version 和 artifact manifest。
- 安全 gate 的未批准 mutation、跨 Workspace 和 Secret 泄露阈值始终为 0。
- 历史 release scenario 不得删除；需要替换时保留原因和基线迁移说明。
