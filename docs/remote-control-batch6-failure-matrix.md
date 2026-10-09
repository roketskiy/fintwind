# 第六批失败矩阵与最小修复范围（2026-10-09，reviewer F17-F20 后续 P1）

审查事实已逐条对照源码确认（非推论）：

| # | 失败方式 | 证据 | 修法 |
| --- | --- | --- | --- |
| F21 | **取消只活在内存**：`QueuedPrompt.cancel_requested: Option<Uuid>` 是纯内存字段，`recover_queues` 重建时写 `None`。host 崩溃后 daemon 行仍是 `queued` → 重启 `dispatch_ready_queues` 直接派发 P，"Accepted 的取消不丢"被违反 | queue.rs:95（内存字段）；state.rs recover 构造 `cancel_requested: None`；main.rs dispatch 循环只读内存标记 | 取消意图落 **daemon 侧 durable**：`remote_prompts` 增 `cancel_operation_id`/`cancel_state`（migration 0006，旧库自动升级）。host 内存标记降级为缓存，权威在 daemon 行 |
| F22 | **host 崩溃后 C 永远 Accepted**：行 `dispatching` 崩溃遗留，恢复后 submission 已 settle 为 `NotSent`，但没有任何路径再回答 C；设备侧 C 停在 `accepted`  forever | daemon.rs dispatch 的 submission 状态复检只回 prompt 真相，不带 cancel 真相；host 恢复只重建 queued 项 | daemon 在每个行状态迁移点**原子解析**挂起的 cancel；新增 `QueryRemoteCancel` 让 host 查询 cancel 真相（不靠 prompt 查询冒充当取消已执行证明） |
| F23 | **take 先于 RPC，错后丢 intent**：`honour_pending_cancel` 先 `take_cancel_request`（清内存）再发 RPC；RPC 失败/非权威应答时 intent 已丢，项回到 Queued，下一 tick 照常派发 P | main.rs:1190-1214（take 在 1190，RPC 在 1203） | host 不再"先取后问"：一律**先问 daemon 再动本地状态**；未知时本地标 unknown 并 hold P，绝不在未知副作用下自动重发 |
| F24 | **取消操作完全不幂等**：`run_operation` 对 `CancelQueuedPrompt` 在 dedup 检查**之前**就 return，C 从不 `record_accepted`/`record_finished` → 重放会再跑一遍副作用，终态不去重 | main.rs run_operation 开头的 CancelQueuedPrompt 早退；dedup 无 C 的记录 | C 与其他 operation 一视同仁：副作用前 `record_accepted`，终态 `record_finished`；重放从记录回答 |
| F25 | **迁移不能重写已部署文件**：daemon migration 按 tag 记录已应用，直接改 0005 的 SQL 不会升级旧库 | persistence.rs `apply_migrations`（tag  EXISTS 检查）；build.rs 按文件名 glob + 数字前缀排序 | 新增 `db/migrations/0006_remote_cancel_intent.sql`（`ALTER TABLE ADD COLUMN`），旧库启动时自动应用；不碰 0005 |

不在本批：F17/F18（reviewer 已确认，不重做）；PWA 兼容（main）；relay/supervisor（他人）。

## 取消生命周期矩阵（durable 版）

权威在 daemon 行；`cancel_operation_id`/`cancel_state` 与行状态同生共死。

| 时点 | daemon 行 | daemon 动作 | host 动作 | C 得到 |
| --- | --- | --- | --- | --- |
| C 到达，行 `queued` | `queued` | abandon + 删行 + cancel_state=`cancelled` | settle P/C | `Executed {cancelled:true}` |
| C 到达，行 `dispatching` | `dispatching` | **持久记录** C（CAS：同 C 幂等，异 C 拒） | 答 C `Accepted`（dedup 落 accepted） | `Accepted`（结局由 daemon 推送） |
| C 到达，行已有异 C | — | 拒（Malformed） | 答 C 拒绝 | `Rejected{Malformed}` |
| 派发应答 `dispatched`，行有挂起 cancel | `dispatched` | 原子解析 cancel_state=`dispatched`（**已交驱动，不取消**） | settle P=Executed；C=Rejected "已运行" | `Rejected{Malformed}` |
| 派发 Busy 回 `queued`，行有挂起 cancel | 回 `queued` | **立即兑现**：abandon + 删行 + `cancelled` | settle P/C | `Executed {cancelled:true}` |
| 恢复派发前行有挂起 cancel | `queued`/`dispatching` | **先于任何派发**兑现取消 | 不派发 P，先查询解析 | 按查询真相 |
| host 崩溃后恢复 | 任意 | — | `ListRemotePrompts` 带 cancel 字段重建；有挂起 cancel 的项**不派发**，先 `QueryRemoteCancel` | 按查询真相 |
| C 的 RPC 未知（连接断/无应答） | 未知 | — | **不自动重发**；本地标 unknown 并 hold P；查询原 C/P 权威状态 | `Unknown` |
| 行已不在（归档/会话删除/清扫） | absent | — | 查询回 `absent` → P 不会运行，C 按"未运行"结算 | `Executed {cancelled:true}` |
| grant 变更（重配/撤销） | — | provenance 精确校验拒 C；清扫用行自带版本释放行 | 行释放后 cancel 随之结算 | 按行释放真相 |

硬约束：已交驱动绝不取消、绝不宣 notSent；未知绝不自动重发；必须查**原** operationId，不生成新 id；
同 id 异 target 拒；双设备按 provenance 隔离；旧无 hash 行按 F18 fail-closed（本批不改）。

## 持久化设计：为什么在 daemon 行上，不在 host 内存

取消意图的权威必须是**已经在崩溃中存活下来的那一层**。host 队列是内存投影；daemon 的
`remote_prompts` 行是 durable 事实，且已携带 device/host/grant 绑定。因此：

- `remote_prompts` 增两列（migration 0006）：`cancel_operation_id text`、`cancel_state text`。
  admit 时两者为 NULL；取消到达而行处于 `dispatching` 时写入 `(cancel_operation_id,'pending')`。
- host 内存的 `QueuedPrompt.cancel_requested` 降级为**缓存**：`recover_queues` 从
  `ListRemotePrompts` 的 `cancelOperationId` 重建，Crash 后不再丢失。
- host 自发释放（过期/撤销/会话消失/派发未确认）传 `cancel_operation_id: None`：daemon 记空串
  表示"无设备操作待答"，行仍不得运行，但不会有人被无谓答复。
- 设备发起取消传 `Some(自身 operationId)`：daemon 绑到行上，host 事后凭它答复 C。

### 授权与幂等

| 规则 | 实现 |
| --- | --- |
| 精确 provenance | `record_cancel_intent` 前已过 device/host/grant **全等**校验（与 dispatch 同一条） |
| 同 C 重放幂等 | 行上已是同一 `cancel_operation_id` → `Recorded`，不重复写、不拒绝 |
| 异 C 拒绝 | 行上已是别的 `cancel_operation_id` → `Conflict` → bail，两设备不能同时持有同一 prompt 的取消 |
| 行不存在 | `RowMissing` → bail |
| 并发竞态 | UPDATE 带 `AND cancel_operation_id IS NULL` 条件；失败后重读判定胜者，不吞错误 |

### 查询而非重发（reviewer 特别要求）

host 已答 `Accepted` 后又需要真相时（Crash 恢复、RPC 无应答），**只查询、不重发**：
`Command::QueryRemoteCancel { provenance, operation_id }` → `RemoteCancelState { operationId,
submissionId, cancelOperationId, cancelState }`。`cancelState` 取值
`pending | cancelled | dispatched | expired | notSent | unknown | absent`，或行自身状态。
它不是 prompt 查询冒充当取消已执行证明——daemon 报的是它亲自记录的 cancel 绑定。
`unknown` 时 host **hold 原 P 不派发**，绝不自动重发副作用。

### 原子/可恢复顺序

daemon 在每个行状态迁移点同帧解析挂起 cancel：

| 迁移 | cancel 解析 |
| --- | --- |
| admit（新行） | 无 cancel（NULL） |
| `queued → dispatching` claim | 不动（派发中） |
| `dispatching → dispatched` | `resolve_cancel_intent(op,"dispatched")` —— **已交驱动，不取消** |
| `dispatching → queued`（Busy） | 若挂起则立即 `honour_durable_cancel`：abandon + 删行 + `cancelled` |
| dispatch 入口发现挂起 | **先于任何派发** `honour_durable_cancel` |
| 终态未知/失败 | abandon + 删行；cancel 随之消失（行不在了，prompt 不会运行） |

`resolve_cancel_intent` 在**一个事务**内读取并清除绑定，Crash 不会留下"已解析却显示 pending"。
