# OpsCodex 工程交付契约

- 状态：Approved baseline
- 适用范围：所有后续实现任务
- 最近更新：2026-08-16

本契约把[最终产品目标](PRODUCT_GOAL.md)和[路线图](ROADMAP.md)转换为工程执行规则。阶段详细
设计可以增加更严格的要求，但不能降低这里的边界。

## 技术栈

| 领域 | 基线 | 约束 |
| --- | --- | --- |
| Runtime | Rust 2024、Tokio | Agent Loop 自研，不引入 Agent Framework |
| HTTP client | Reqwest + rustls | 统一 timeout、cancel、target policy、bounded body |
| Serialization | Serde/serde_json/TOML | 外部协议和持久化 schema 必须版本化 |
| Error/telemetry | thiserror、tracing | 不自建 logging framework；敏感内容默认不记录 |
| App Server | Axum、tower-http | `/api/v1` REST command + replayable SSE |
| Storage | JSONL (`v0.1-v0.4`)、SQLite (`v0.5+`) | 通过 EventStore port；不引入远程数据库 |
| Web | React、TypeScript、Vite、Tailwind/shadcn primitives | useReducer event projection；不引入全局状态框架 |
| Demo/Eval | Docker Compose、Python unittest、deterministic fixtures | 真实 Provider 只是受保护 release gate |

新增运行时依赖前必须说明：解决的阶段验收、维护状态、许可证、安全面和为什么标准库/现有依赖
不足。数据库、消息队列、Agent Framework、Vector DB 或 Workflow Engine 需要新 ADR。

## 标准命令

在仓库根目录执行：

```sh
# 快速开发验证
just test

# 格式、Clippy、全部测试和 Web build
just check

# 完整发布前门禁，包括依赖审计和 release build
just release-check

# 确定性本地 UI
just serve-fake

# Demo 启停
just demo-up
just demo-down
```

没有 `just` 时使用：

```sh
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cd web && npm ci && npm test && npm run build
python3 -m unittest discover -s demo/order-service/tests -v
python3 -m unittest discover -s scripts/tests -v
cargo audit
cargo deny --locked check advisories licenses bans sources
cd web && npm audit --audit-level=high --registry=https://registry.npmjs.org
```

阶段设计必须补充自己的 scenario、migration、security 或 fault-injection 命令；不能用手工点击替代
可以自动化的门禁。

## 项目结构

```text
src/
  runtime/       Thread/Turn/Agent Loop/Event/Context/Recovery
  model/         Provider-neutral contract 和 wire adapters
  tools/         Built-in Tool、descriptor 和 registry
  policy/        Policy、Approval、Action authorization
  store/         EventStore port、JSONL/SQLite adapters、migration
  server/        REST/SSE adapter 和 projection API
  evidence/      v0.2 起的 Evidence/Claim/Artifact domain
  workspace/     v0.3 起的 Workspace/target/credential references
  extensions/    v0.4 起的 MCP/Custom Tool/Skill adapters
web/src/         React client、event reducer、focused UI components
tests/           Rust cross-module integration/contract tests
demo/            Deterministic incident infrastructure
scripts/         Acceptance verifiers and release tooling
docs/            Goal、architecture、roadmap、phase designs、ADRs
```

保持单 Cargo crate，直到出现第二个需要独立发布的 Rust consumer。新模块按领域边界创建，不按
“helpers”“common”堆放无所有权代码。

## 代码风格

Rust 使用显式领域类型、Result 和异步取消；错误不能通过字符串判断控制流：

```rust
pub async fn append_event(
    &self,
    command: AppendEvent,
    cancellation: CancellationToken,
) -> Result<EventEnvelope> {
    validate_transition(&command)?;
    tokio::select! {
        _ = cancellation.cancelled() => Err(OpsCodexError::Cancelled),
        result = self.store.append(command) => result,
    }
}
```

TypeScript reducer 必须是对 Event 的纯投影，未知可选字段不能使页面崩溃：

```ts
export function applyEvent(state: OpsState, event: RuntimeEvent): OpsState {
  if (event.seq <= state.lastSeq) return state;
  return projectKnownEvent({ ...state, lastSeq: event.seq }, event);
}
```

统一约定：

- Rust `cargo fmt`、Clippy warnings-as-errors；TypeScript strict mode。
- Domain ID 使用 newtype，不在核心层传裸字符串。
- 外部 JSON 在 adapter 边界验证，核心层不接受未规范化 `Value` 作为安全决策依据。
- 注释解释不明显的约束和原因，不复述代码。
- 不将 Provider/Connector wire type 泄漏到 Runtime domain。
- schema、config、Event 和 migration 变化必须附兼容 fixture。

## 测试策略

| 层级 | 负责证明 | 最低要求 |
| --- | --- | --- |
| Unit/property | 状态转换、预算、解析、policy、redaction | 边界值、非法输入、取消、错误分支 |
| Contract | Provider、Tool、Connector、EventStore、API/Event | 所有 adapter 运行同一行为 suite |
| Integration | Model/Tool/Event/Store/Server/UI 联动 | Fake Model 可重复、无真实 Secret |
| Scenario | 诊断质量、Evidence、恢复和安全 | 每阶段新增，历史场景持续通过 |
| Live gate | 真实 Provider 和真实协议差异 | 受保护环境、bounded/redacted artifacts |
| Release | Build、audit、migration、rollback、soak | 阶段门禁和 release checklist 全通过 |

行为变更遵循 red-green-refactor：先写能证明缺失行为的测试，再实现最小改动。纯文档变更至少执行
相对链接、格式和追踪矩阵检查。

覆盖率百分比不是单独门禁；高风险状态机、Policy、迁移和 recovery 要求所有已定义转换与故障边界
都有测试。

## 工程边界

### Always

- 先定位对应阶段、Acceptance 和 ADR，再修改实现。
- 先持久化领域状态，再对外发布 Event。
- 为所有外部 I/O 设置 timeout、cancel、size limit 和错误分类。
- 对目标、参数、重定向、DNS 解析和 Workspace scope 做代码级校验。
- 保持 Secret、Chain of Thought 和原始敏感 Evidence 不进入 telemetry/发布产物。
- 在交付前运行与风险相称的自动测试并记录未执行项。

### Ask first

- 改变最终目标、非目标、版本顺序或 Accepted ADR。
- 新增远程基础设施、数据库、消息队列或常驻服务。
- 改变公共 API/Event、存储兼容窗口或数据 retention 默认值。
- 新增 change/irreversible capability、扩大 target allowlist 或降低审批要求。
- 引入远程监听、认证、多用户或跨 Workspace 行为。

### Never

- 提交 Secret、真实生产 Evidence、用户本地 Thread store 或原始 live acceptance log。
- 用 Prompt 代替 Policy、Sandbox、Approval 或 target validation。
- 自动执行任意 `exec`、Runbook command、Skill instruction 或未分类外部 Tool。
- 在未知外部副作用状态下自动重试或自动 rollback。
- 保存/展示 Chain of Thought，或把隐藏 reasoning 当 Evidence。
- 删除失败测试、迁移 backup 或历史场景来让门禁通过。
- 在 `v1.0` 前引入 Multi-Agent、SaaS、多租户或通用 Workflow Engine。

## 变更流程

1. 在阶段设计中选择一个 Task；若没有匹配 Task，先更新设计。
2. 确认关联 ADR；发现决策冲突时先新增 superseding ADR。
3. 把 Task 拆成单个专注会话可完成的垂直切片，通常不超过 5 个文件。
4. 写 Acceptance test，实施，运行局部和全局门禁。
5. 更新阶段状态、CHANGELOG 和运维/迁移文档。
6. 阶段 Gate 全部有证据后才发布版本，不能按主观百分比宣布完成。

## 尚待阶段内决策的问题

以下问题不阻塞设计基线，但必须在对应 Task 开始前收敛：

- `v0.2`：Tempo 目标版本和 structured Diagnosis 的 Provider fallback parser。
- `v0.4`：不同平台 Custom Tool 的具体 OS isolation 能力；未解决前 production profile 禁用。
- `v1.0`：首批发行平台和 artifact signing 机制。

如果选择会改变信任边界或稳定合同，必须新增 ADR；否则记录在阶段设计和实现 PR 中。
