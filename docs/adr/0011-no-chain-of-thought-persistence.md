# ADR-0011: 不持久化、传输或展示 Chain of Thought

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: 全部版本

## Context

用户需要知道 Agent 做了什么、依据什么得出结论，但这不要求保存模型内部 Chain of Thought。
Reasoning 文本可能包含敏感信息、不可稳定重放，也不是可验证 Evidence。部分 Provider 可能要求
opaque reasoning/continuation state 才能延续请求，需要与面向用户的解释区分。

## Decision

1. Chain of Thought 或隐藏 reasoning 不成为 Item、Event、Evidence、API、telemetry 或 UI 字段。
2. UI 只展示用户消息、Assistant 结果、Tool proposal/execution、Evidence、Claim、Approval 和状态。
3. “为什么”通过 Evidence-linked Claim、Tool history、limitations 和结构化决策摘要解释，不通过内部推理。
4. 默认请求 Provider 关闭可选 reasoning 持久输出；供应商不支持关闭时也不消费或记录明文 reasoning。
5. Provider 必须延续的 opaque token 只能作为敏感 checkpoint 保存，绑定 provider/model/version，
   不出现在普通导出，并服从 retention/encryption policy。
6. 如果 continuation 无法安全持久化，恢复时从最后规范化 checkpoint 重新执行 model step。

## Consequences

正面：降低敏感信息和供应商耦合；UI 关注可验证行为；诊断解释可以由 Evidence 复核。

代价：不能逐 token 重建模型内部思路；某些 Provider continuation 可能无法完全恢复；开发调试需要
依靠 request metadata、Tool/Event 和 fixture，而不是 reasoning log。

## Alternatives considered

- 把 reasoning 作为特殊 Item：会泄露不可验证内容，拒绝。
- 仅在 debug 模式记录：debug 日志同样可能扩散和被长期保存，拒绝。
- 为恢复强制保存明文 reasoning：隐私和 Provider 耦合成本过高，拒绝。
- 不提供任何解释：Evidence 和 Claim 已能提供可验证解释，不采用。

## Enforcement and verification

- Domain/API schema 不定义 reasoning/chain_of_thought 字段。
- Provider fixture 含 reasoning payload 时，Event、logs、SSE 和 artifacts 扫描结果必须为空。
- 代码审查把新增 reasoning persistence 视为 ADR 违反。
- 最终回答解释测试只要求 Evidence/Claim/limitations，不要求隐藏推理文本。
