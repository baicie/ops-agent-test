# ADR-0003: Provider-neutral 模型边界

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.1+`

## Context

模型供应商的 HTTP、SSE、function calling、usage、reasoning 和 continuation 语义不同。若 Runtime
直接使用供应商 SDK 类型，领域模型会被 wire protocol 污染；但把所有“兼容 endpoint”当成行为
完全一致也已在真实验收中被证明不成立。

## Decision

1. Runtime 只依赖项目自有 `ModelRequest`、`ModelOutput`、`ModelEvent` 和 `ModelProvider`。
2. Provider adapter 独占供应商 HTTP/SSE 类型、认证、错误映射和 continuation。
3. Provider 明确声明 capabilities：streaming、tool calls、parallel calls、usage、reasoning control、
   continuation、request idempotency 和 structured output。
4. 配置在启动时与 capability 校验；不支持的组合 fail fast，不静默忽略。
5. 每个 adapter 必须通过同一 contract suite；“OpenAI-compatible”是待验证协议声明，不是信任标签。
6. Provider 按协议和行为命名，不为每个品牌复制 Runtime abstraction。
7. opaque continuation 作为敏感、绑定 provider/model/version 的 checkpoint 保存，不成为普通 Item。
8. 无法安全恢复 opaque state 时，从最后一个规范化 Context checkpoint 重启 model step。

## Consequences

正面：更换模型不会改变 Agent、Tool、Evidence 或 UI；兼容性问题集中在 adapter；测试可使用本地
fixture 而不发送 Secret。

代价：内部协议需要演进；每个 Provider 有 mapping 和 contract 维护成本；最低共同抽象之外的能力
需要 capability branch。

## Alternatives considered

- Runtime 直接依赖供应商 SDK：耦合类型和生命周期，拒绝。
- 只支持一个 Provider：不符合用户可控和兼容 endpoint 目标，拒绝。
- 所有 Provider 只使用最低共同能力：会丢失 streaming/usage/continuation，拒绝。
- 按品牌创建不同 Agent Runtime：重复核心逻辑，拒绝。

## Enforcement and verification

- Provider wire type 不能出现在 runtime、store、server public domain model。
- contract suite 覆盖 stream、tool call/result、usage、cancel、error、EOF 和 capability mismatch。
- Secret 不进入 request debug、Event、telemetry 或 fixture。
- 真实 Provider 只作为受保护 release smoke gate，不能替代本地 contract suite。
