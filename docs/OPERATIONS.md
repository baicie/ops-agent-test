# OpsCodex 运维手册

- 状态：实现中
- 适用范围：单进程、单操作者、用户控制的本地环境
- 不适用范围：多实例、SaaS、多租户、高可用、无人值守自动修复

“Production Ready”在这里只表示：一名运维工程师可以安装、备份、恢复、升级，并在 loopback
上长期运行调查与人工批准的有限 remediation。它不是公网 SLA。

## 安装

1. 安装 Rust 1.88+、Node.js 20+。完整 Demo 需要 Docker Compose。
2. 构建 Web 与二进制：

```sh
cd web && npm ci && npm run build && cd ..
cargo build --locked --release
```

3. 创建数据目录并复制示例配置：

```sh
mkdir -p "${OPSCODEX_HOME:-$HOME/.opscodex}"
cp config.example.toml "${OPSCODEX_HOME:-$HOME/.opscodex}/config.toml"
```

4. 通过环境变量提供模型密钥，不要写入配置文件、Thread、Artifact 或 CI 日志。默认读取
   `OPENAI_API_KEY`（由 `[model].api_key_env` 指定）。
5. 先做只读诊断，再启动服务：

```sh
cargo run -- doctor
cargo run -- config validate
cargo run -- --fake-model serve
```

浏览器打开 `http://127.0.0.1:3000`。`GET /healthz` 只表示进程存活；`GET /readyz` 表示
store 可读取。

## Loopback 绑定

未配置 TLS 时，OpsCodex 只能绑定 loopback：`127.0.0.1`、`localhost` 或 `::1`。
`server.host = "0.0.0.0"` 或 `opscodex serve --host 0.0.0.0` 会被拒绝。

不要通过改绑定地址把单操作者实例暴露到局域网。远程多用户需要新的产品目标，而不是改一行配置。

## 日常诊断命令

这些命令不启动 Agent Loop，也不要求模型 API key：

```text
opscodex doctor
opscodex config validate
opscodex storage verify
opscodex storage backup --out DIR_OR_FILE
opscodex storage export --thread <id> --out FILE
opscodex export --thread <id> --out FILE
opscodex audit verify
```

`doctor` 在 sqlite 文件尚未创建时报告 `degraded` 并退出 0；配置非法、非 loopback 绑定、
`production_safe` 与 `exec` 同时开启等会报告 `error` 并退出非 0。

`storage verify` 运行 SQLite `integrity_check`、外键检查，并确认 schema version 1 和 2
都已应用。备份使用 `VACUUM INTO`，目标文件必须不存在。恢复前先停进程。

## 备份与恢复

默认数据布局：

```text
~/.opscodex/
  config.toml
  state.sqlite3
  state.sqlite3-wal
  state.sqlite3-shm
  artifacts/
  threads/                 # 旧 JSONL；用 opscodex migrate 导入
```

在线备份（推荐）：

```sh
# 可在进程运行时执行；目标路径不能已存在
opscodex storage backup --out /safe/backup/state.sqlite3
opscodex storage verify
```

停机拷贝时必须同时复制 `state.sqlite3`、`-wal`、`-shm` 和 `artifacts/`。不要只拷主库。

恢复：

1. 停止 OpsCodex。
2. 用备份替换 `state.sqlite3`（以及当时一起复制的 WAL/SHM，或只恢复 `VACUUM INTO` 得到的单文件）。
3. 需要时一并恢复 `artifacts/`。
4. `opscodex storage verify` 与 `opscodex audit verify`。
5. 启动后确认 `/readyz` 返回 200。

JSONL 回滚：保留 `threads/backup-<timestamp>/`，将 `[store] backend = "jsonl"` 指向备份，
或把 `.jsonl` 拷回 `threads/`。SQLite 文件不会被自动删除。

## 升级与回滚

- SQLite schema 只向前迁移。新二进制可以读旧 schema 并离线升级；不承诺旧二进制写入新 schema。
- 升级前：`opscodex storage backup --out ...`，记录当前版本与配置。
- 升级后：`opscodex doctor`、`storage verify`、`audit verify`，再用 fake-model 走一条调查路径。
- 回滚：停进程，恢复升级前的二进制与 SQLite 备份。如果新版本已经写入新 schema，旧二进制不应
  再打开该文件。
- `v1.0.0` tag 与正式 GitHub Release 仍是阶段门禁，当前仓库未将其标为已发布。

## NeedsReconciliation

观察类工具在崩溃后可以按 checkpoint 重试。变更或外部副作用一旦结果未知，Turn 进入
`needs_reconciliation`，**不会自动重试或自动 rollback**。

操作者应：

1. `GET /api/v1/turns/:id/recovery` 阅读分类、风险和建议。
2. 到目标系统确认副作用是否已经发生。
3. 若需反向操作，提出**新的** ActionPlan，而不是重放旧批准。
4. 明确 Resume 仅用于可恢复的模型/观察路径。

确定性崩溃分类由 `just continuity-test` 覆盖；它不能替代真实 OS kill 或 Provider gate。

## Secret 泄露响应

1. 立即轮换泄露的 API key、kubeconfig、tenant token 或其他凭据。
2. 从配置、环境、shell history、CI 日志和发布产物中删除明文。OpsCodex 只应保存 Secret
   **引用**（环境变量名），不保存值。
3. 不要把原始 live acceptance log、本地 Thread store 或未脱敏 Evidence 提交进 Git。
4. 若怀疑 Artifact 或 Event 中写入了密钥：导出相关 Thread、在隔离环境检查，必要时从备份
   恢复到泄露之前的状态，并重新 `audit verify`。
5. 进程日志、`/metrics` 和 UI 不得包含凭据。发现后当作缺陷，而不是靠提示词修补。

## 安全默认值

- `[tools] exec = false`。`extensions.production_safe = true` 时不能再打开 `exec`。
- `[remediation] enabled = false`。启用后仍需 Workspace `allow_remediation`、精确
  `request_hash` 批准和 kill switch。
- `exec`、MCP、Custom Tool 不能作为 remediation。
- 无认证时禁止非 loopback 绑定。

## 容量与配额

本地 command/projection p95 目标是 100ms；默认最多 4 个并发 Turn。单次 Turn 输入超过
32 KiB 或超过 `runtime.context_max_bytes` 会 fail-closed。Artifact 超过配额会拒绝写入。
检测到磁盘满错误时，SQLite/JSONL 返回可操作错误（释放空间后重试）。

`just capacity-test` 覆盖这些有界失败和 4-Turn 负载。它**不是** 24h soak，也不能当作
泄漏或发布门禁；其中 disk-full 覆盖仅验证合成错误到操作提示的映射，并未执行真实 OS
磁盘耗尽或证明零 Event 丢失。

## 发布候选 dry-run

`just release-dry-run` 构建 release 二进制和 Web 资源，并写出校验和与 CycloneDX SBOM。
产物目录默认是 `dist/release-dry-run/`。该命令**不会**创建 `v1.0.0` tag 或 GitHub
Release。`manifest.json` 中 `published` 必须为 `false`。

安装候选版后执行 `opscodex doctor`、`storage verify`、`audit verify`，并确认
`/healthz` 与 `/readyz`。回滚仍使用上一节的备份，而不是重放 tag。
