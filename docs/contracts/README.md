# OpsCodex v1 合同

- 状态：实现中
- 兼容政策：`/api/v1`、Event schema 与 storage schema 采用 **additive** 演进。移除字段或改变语义需要新的 API major。
- 机器可读清单：[api-v1.json](api-v1.json)

`GET /healthz` 只表示进程存活。`GET /readyz` 校验 store 是否可读取。错误响应冻结为：

```json
{"error":{"code":"not_found","message":"..."}}
```

未知 Event 字段必须被忽略。客户端不能把缺失的可选字段当成协议破坏。
