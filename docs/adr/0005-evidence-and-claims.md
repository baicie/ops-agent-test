# ADR-0005: Evidence 一等公民与 Claim 引用

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.1+`，完整合同在 `v0.2`

## Context

AIOps Agent 与普通 Chatbot 的核心差异不是能调用 Tool，而是诊断可以被复核。`v0.1` 已保存
source/query/timestamp/duration，但 Evidence 没有稳定 ID，最终答案引用依赖 Markdown 文本约定，
无法自动发现无来源结论、内容变化或过期数据。

## Decision

1. 每个成功观察 ToolResult 生成稳定 `EvidenceRecord`，带来源、目标、查询、时间窗、摘要、大小、
   truncated、sensitivity、artifact ref 和 SHA-256。
2. Alert、Runbook 或模型陈述不是自动 Evidence；必须标记 provenance，告警中的判断需要 Tool 验证。
3. 大结果存入有界 content-addressed artifact，Context 只使用摘要和 Evidence ID。
4. 最终 Diagnosis 由 Claim 组成，kind 为 observed、inferred 或 recommended。
5. observed/inferred Claim 必须引用存在且可访问的 Evidence ID；inferred 还必须显式标注推断。
6. Confidence 使用 low/medium/high，不提供未经校准的精确概率。
7. Evidence 可以过期或被 retention 删除内容，但 ID、hash、metadata 和引用关系必须保留。
8. UI、CLI 和 verifier 使用同一结构化 Diagnosis，不从自由文本反向猜引用。

## Consequences

正面：诊断可追溯、可评分、可审计；大输出不必全部进入模型；用户能区分事实、推断和建议。

代价：Tool adapter 需要规范化和 redaction；模型输出需要 schema/parser；artifact retention 与引用失效
需要明确 UX；不是所有 Provider 都稳定遵循 structured output。

## Alternatives considered

- 只保留 Markdown Evidence section：不可机器验证，拒绝。
- 把完整 Tool output 永久内联：Context 和存储成本不可控，拒绝。
- 用模型精确概率表示 confidence：未经校准，容易误导，拒绝。
- Alert/Runbook 直接视为事实：来源可能过期或恶意，拒绝。

## Enforcement and verification

- verifier 拒绝不存在、跨 Workspace、hash 不匹配或权限不可见的 Evidence 引用。
- 场景评测统计关键 Claim 引用有效率和无来源 Claim 数。
- artifact/read API 强制 byte、range、sensitivity 和 retention policy。
- Tool success、Tool failure、truncated 和 redacted Evidence 都有 fixture。
