
## 事务原子边界（本批实现）

| 写入 | 边界 | 崩溃后果 |
| --- | --- | --- |
| cancel 绑定（`record_cancel_intent`） | 单条条件 UPDATE（`AND cancel_operation_id IS NULL`）+ 失败重读 | 要么绑定、要么没有；不会半绑 |
| cancel 终态（`record_cancel_term`） | 单条 `INSERT OR REPLACE` | 幂等；重写同一终态无副作用 |
| 清绑定 + 读回（`resolve_cancel_intent`） | **一个事务**内 SELECT + UPDATE | 不会"已解析却仍显示 pending" |
| 行状态迁移（`transition_remote_prompt`） | 单条条件 UPDATE（CAS） | 旧 tick 无法复活已 settle 的行 |
| 派发成功后的 cancel 收敛 | 先迁移行、再写终态（顺序可恢复） | 行已 `dispatched` 而终态未写 → 重启后查询按 submission 权威重新收敛 |

关键顺序：**submission 权威先于 cancel 兑现**。入口先读 submission，只有 `Preparing`
（可证明从未交驱动）才允许取消；`Dispatching`/`Accepted` 收敛为 `dispatched`（太晚）；
`NotSent`/`Rejected` 收敛为 `cancelled`；`Unknown` 或缺失收敛为 `unknown`。

## 可达性（谁能问到终态）

- 设备只持 prompt 的 operationId，`QueryRemoteCancel{operationId}` 走
  `cancel_term_by_prompt`（按 prompt 查，取最新终态）。
- host 侧重放 C：同一查询，不重发。
- host 重启：`ListRemotePrompts` 全行扫描出 `cancel_operation_id`，孤儿取消进
  `orphaned_cancels`，tick 里查询并答复，不再依赖 `queued|dispatching` 过滤。
- 行已删：只剩 `remote_cancel_terms`；无记录则 `unknown`，绝不推断 cancelled。
