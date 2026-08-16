# ADR-0007: MCP、Custom Tool 与 Skill 的扩展信任边界

- Status: Accepted
- Date: 2026-08-16
- Deciders: OpsCodex maintainers
- Applies to: `v0.4+`

## Context

OpsCodex 需要连接用户特有系统，但把所有 connector 编译进核心不可持续。MCP、外部工具和 Skill
提供扩展性，也引入恶意内容、schema 欺骗、宿主权限、Secret 泄露和资源耗尽风险。模型提示词
不能充当这些风险的隔离层。

## Decision

1. 内建和外部 Tool 都归一化为带 namespace/version/schema hash/effect/target/budget/provenance 的
   `CapabilityDescriptor`。
2. 扩展声明不能授予权限；最终规则取扩展、Workspace 和本地 override 中最严格结果。
3. MCP 只支持受控 stdio 和 allowlisted TLS Streamable HTTP；每次调用仍经过普通 Tool/Policy。
4. Custom Tool 使用 manifest 和进程外无 shell JSON runner；不加载进程内动态 library。
5. Skill 只提供 bounded instructions/resources，不能注册能力、获得 Secret 或直接执行命令。
6. 发现不等于启用。扩展必须显式配置、pin version/hash、校验兼容性和 Workspace scope。
7. 外部内容全部视为不可信，不能覆盖 System、Policy、effect 或 target。
8. `v1.0` 不提供 marketplace、自动下载、自动升级或通用脚本执行。

## Consequences

正面：可以扩展数据源而不污染 Runtime；所有能力共享 Evidence、安全、预算和审计；恶意 Skill
不能直接成为执行权限。

代价：外部工具接入需要 manifest 和本地 policy；未提供 OS sandbox 时 Custom Tool 只能标为
trusted-local；MCP 的完整 prompts/resources 能力不会自动可用。

## Alternatives considered

- 只允许内建 Tool：扩展成本过高，拒绝。
- 进程内 Rust plugin：ABI、崩溃和权限隔离不可接受，拒绝。
- 任意 shell script plugin：等价于默认开启 exec，拒绝。
- Skill 中的命令自动执行：内容与能力边界混淆，拒绝。
- 自动信任 MCP server 的 risk 声明：服务端可自降风险，拒绝。

## Enforcement and verification

- descriptor/version/schema hash 变化使 pending approval 和 cached capability 失效。
- supervisor 测试 crash、hang、large output/stderr、cancel 和 bounded restart。
- 安全场景覆盖 Prompt Injection、env leak、path traversal、target escape 和 schema mutation。
- production-safe profile 可以完全禁用 Custom Tool 和非 allowlisted MCP。
