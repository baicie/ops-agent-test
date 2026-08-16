# OpsCodex 设计文档

- 状态：Active baseline
- 基线日期：2026-08-16
- 当前发布：`v0.1.0`

本目录是 OpsCodex 产品目标、目标架构、实施阶段和架构决策的事实来源。根目录
`README.md` 负责使用说明，`CHANGELOG.md` 记录已经发布的变化；未来需求不能只存在于
Issue、聊天记录或代码注释中。

## 文档层级

1. [最终产品目标](PRODUCT_GOAL.md)：定义为什么做、为谁做、v1.0 到底什么叫完成。
2. [目标架构](TARGET_ARCHITECTURE.md)：定义跨版本不变量、组件边界和关键数据流。
3. [交付契约](DELIVERY_CONTRACT.md)：定义技术栈、命令、目录、风格、测试和工程边界。
4. [版本路线图](ROADMAP.md)：定义阶段依赖、状态、交付物和验收门禁。
5. [阶段详细设计](phases/)：定义每一阶段具体做什么、怎么做、怎么验证。
6. [ADR](adr/README.md)：记录具有长期约束力的技术决策、备选方案和后果。
7. [运维手册](OPERATIONS.md)：安装、loopback、备份恢复、升级回滚与事故响应。
8. [v1 合同](contracts/README.md)：冻结的 `/api/v1` 路径、error envelope 和 Event 类型。

发生冲突时，优先级为：最终产品目标 > 已接受 ADR > 目标架构 > 路线图 > 阶段设计。
如果产品范围改变，先更新最终目标；如果技术决策改变，先新增或替代 ADR，再同步下游文档。

## 阶段文档

| 阶段 | 状态 | 详细设计 |
| --- | --- | --- |
| Design Gate | 完成 | 本目录及 ADR 基线 |
| `v0.1` Runtime MVP | 已发布 | [v0.1](phases/v0.1-runtime-mvp.md) |
| `v0.2` Evidence Foundation | 实现中 | [v0.2](phases/v0.2-evidence-foundation.md) |
| `v0.3` Runtime Workspace | 实现中 | [v0.3](phases/v0.3-runtime-workspace.md) |
| `v0.4` Extensibility | 实现中 | [v0.4](phases/v0.4-extensibility.md) |
| `v0.5` Continuity | 实现中 | [v0.5](phases/v0.5-continuity.md) |
| `v0.6` Safe Remediation | 实现中 | [v0.6](phases/v0.6-safe-remediation.md) |
| `v1.0` Production Ready | 实现中 | [v1.0](phases/v1.0-production-readiness.md) |

## 状态定义

- `计划中`：目标和边界已明确，尚未进入实现。
- `实现中`：存在负责人和可追踪任务，代码尚未通过阶段门禁。
- `候选`：功能完成，正在执行完整验收和发布审查。
- `已发布`：阶段门禁、发布门禁和版本发布全部完成。
- `阻塞`：有明确外部依赖，阶段文档必须写明解除条件。

百分比不用于表示阶段完成度。只有阶段门禁全部通过，状态才可前进。

## 设计工作规则

- 每个实现任务必须链接一个阶段交付物和验收条件。
- 每个阶段必须保持可运行、可回滚；不能依赖下一阶段才能完成当前验收。
- 新增工具、协议字段、存储语义或执行权限前，必须核对对应 ADR。
- 安全边界不能只依赖模型提示词；必须由 Runtime、Policy 和执行环境强制。
- 不持久化或展示模型 Chain of Thought。只保存用户内容、可观察行为、证据和结果。
- 默认仍是本地优先、单进程、单操作者。远程多用户或 SaaS 不在 v1.0 范围内。
- Multi-Agent 不在 v1.0 范围内；只有单 Agent 基线达到可靠性和安全门禁后才重新评估。

## 当前事实

`v0.1.0` 已证明 Rust 自研 Runtime 可以在真实 Responses-compatible 模型和 Docker
故障环境中完成 `Model -> Tool -> Evidence -> Model -> Diagnosis`。当前最重要的限制是：真实 Provider live gate、cluster/MCP smoke 和崩溃 fault suite
尚未作为发布门禁跑过，因此 `v0.2` 到 `v0.5` 都不能标为已发布。重启后的安全续跑由
`v0.5` 提供，在阶段门禁通过前不能把系统宣传为可恢复执行。`v0.2` Evidence Foundation 正在实现中：
协议、Evidence/Claim、Loki/Tempo 和评测骨架已经落地，真实 Provider live gate 尚未作为
发布门禁跑过，因此该阶段不能标为已发布。`v0.3` Runtime Workspace 正在实现中：Workspace
隔离、只读 Kubernetes、Topology 投影和本地 Runbook 已经落地，真实 cluster smoke 与
Provider gate 尚未跑过，因此也不能标为已发布。`v0.4` Extensibility 正在实现中：
Capability Descriptor、受控 MCP/Custom Tool 和本地 Skill 已经落地，真实 MCP smoke 与
Provider gate 尚未跑过，因此该阶段不能标为已发布。`v0.5` Continuity 正在实现中：SQLite
默认存储、JSONL 迁移/导出、checkpoint/lease/durable approval、recovery、compaction 和
Thread Fork 已经落地。崩溃分类由 `just continuity-test` 在 durable checkpoint 提交后模拟
进程退出；真实 OS kill-process suite 与 Provider gate 尚未作为发布门禁跑过，因此该阶段不能标为
已发布。`v0.6` Safe Remediation 正在实现中：结构化 ActionPlan、参数绑定批准、Demo fault reset、
Kubernetes scale、kill switch 和 hash-chained Audit 已经落地。默认配置 mutation count 仍为 0；
真实环境 gate 与受保护 cluster smoke 尚未作为发布门禁跑过，因此该阶段不能标为已发布。
`v1.0` Production Readiness 正在实现中：doctor/verify/backup/audit CLI、`/readyz`、无 TLS 时
非 loopback fail-closed、合同 fixture、运维文档、release dry-run（校验和/SBOM）、本地容量/
配额 harness 和八类 deterministic 场景骨架已经落地。
24h soak、真实 Provider gate 与 `v1.0.0` tag 尚未作为发布门禁跑过，因此该阶段不能标为已发布。
