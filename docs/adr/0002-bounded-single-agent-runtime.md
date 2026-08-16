# ADR-0002: Thread/Turn/Item/Event 与有界单 Agent Runtime

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.1+`

## Context

项目要证明的核心是模型在请求之间调用工具、把结果作为 Evidence 回灌，并最终结束 Turn。
Workflow DAG、Planner/Supervisor、多 Agent 会引入任务所有权、共享 Context、冲突和取消复杂度，
但不会提高 MVP 核心闭环的证明质量。

同时，会话内容、执行状态和流式通知需要不同语义；把所有内容压成 chat message 会妨碍恢复和审计。

## Decision

1. 使用 `Workspace -> Thread -> Turn -> Item` 领域层级，以不可变 Event 描述状态变化。
2. 一个用户输入创建一个有界 Turn；同一 Thread 最多一个 active Turn。
3. Runtime 使用单 Agent 循环：Model -> zero or more Tool calls -> results -> next Model step。
4. 每个 Turn 有 max steps、model/tool timeout、output/context/cost budget、取消和全局并发限制。
5. 一个 Turn 恰好进入 completed、failed、cancelled 或 needs_reconciliation 一个终态。
6. 一个模型 response 中的 Tool call 默认按返回顺序执行；未来并行必须由独立 ADR 定义依赖语义。
7. Tool failure 作为结构化 result 回灌模型；只有存储、协议或预算等不可继续错误才直接失败 Turn。
8. `v1.0` 之前不引入 Multi-Agent、Graph 或通用 Workflow Engine。

## Consequences

正面：执行路径容易理解、测试、限制和恢复；当前 Rust Runtime 不需要推翻即可持续增加 Tool。

代价：不能自动并行复杂调查或委派子任务；同一 Thread 的长任务会阻塞后续 Turn；必须认真设计
Context compaction，而不是把复杂度转移给多个 Agent。

## Alternatives considered

- LangGraph/Workflow DAG：预先固化流程，削弱模型按 Evidence 决策的价值，拒绝。
- Planner + Executor Agent：增加协调成本和不可见失败，推迟到 v1 后重新评估。
- 多个 Tool call 无条件并行：调用可能有顺序依赖且增加目标负载，拒绝。
- 同 Thread 并行 Turn：Context 和事件因果关系不确定，拒绝。

## Enforcement and verification

- Agent loop、max steps、timeout、cancel、同 Thread 冲突和全局并发必须有确定性测试。
- State transition property tests 拒绝双终态和非法跳转。
- 新阶段不得以“临时 orchestrator”旁路 Runtime loop。
- 引入 Multi-Agent 前必须建立新目标、量化单 Agent 无法满足的场景并新增 ADR。
