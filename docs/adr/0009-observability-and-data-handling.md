# ADR-0009: Product Event、Telemetry 与 Security Audit 分离

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.2+`

## Context

OpsCodex 会接触生产日志、Trace attributes、Tool 参数和模型输入。为了调试而记录全部内容，会把
Secret、个人数据和业务数据扩散到日志、metrics、trace 或 CI artifact。另一方面，只依赖 JSONL
又无法观察 queue、model/tool latency、store error 和 SSE lag。

## Decision

1. 明确三条数据面：Product Event、Runtime telemetry、Security Audit，不互相替代。
2. Product Event 保存重建 Thread 所需的语义内容，按 Workspace retention/redaction 管理。
3. telemetry 使用稳定 span 层级 `thread -> turn -> model/tool/policy/store`，可选 OTLP exporter。
4. metrics 至少覆盖 queue、turn、model/tool、store、recovery、SSE lag、token 和估算 cost。
5. Prompt、Tool 原始参数/输出、Secret 和原始 Evidence 默认不进入 telemetry；记录分类、大小、状态、
   hash 或脱敏摘要。
6. 高基数 Thread/Turn/Evidence ID 只可进入 trace/log 字段，不能成为 metric label。
7. Security Audit 只保存 Policy/Approval/Action 的必要身份、hash 和结果，不复制原始 Evidence。
8. 发布产物使用显式 allowlist；永不上传原始 Thread/Event store。
9. 数据分类、retention、artifact quota 和 redaction failure 都必须可配置且 fail closed。

## Consequences

正面：Runtime 可运维而不默认泄露生产数据；审计和产品历史各自有清晰目的；metrics 可稳定聚合。

代价：调试时需要通过受控 Evidence UI，而不是在日志中搜索原文；redaction 可能损失信息；需要维护
字段字典、retention 和 Secret fixture。

## Alternatives considered

- 把 Product Event 当应用日志：混合生命周期和权限，拒绝。
- 默认记录完整 Prompt/Tool output：隐私风险不可接受，拒绝。
- 不提供 telemetry：生产故障无法区分模型、工具、存储或队列，拒绝。
- 绑定单一 observability vendor：违背本地优先，拒绝。

## Enforcement and verification

- telemetry schema 使用字段 allowlist，CI fixture 注入 Secret/PII 并扫描所有输出。
- metrics test 拒绝高基数 label。
- audit export 验证 hash chain 和 redaction。
- release artifact manifest 列出唯一允许文件、大小和敏感扫描结果。
