# 第八批失败矩阵与最小闭合范围（2026-10-09，reviewer F26-F31 收尾）

> **实现状态（2026-10-10，GLM 收尾批）**：F32–F36 已全部静态实现并通过
> 编译（`cargo check --locked -p fintwind-core -p fintwind-remote-host` 与
> `--workspace`）。逐条落点见 `remote-control-progress.md` 同日小节。
> **仅静态编译，未运行验证**，不据此宣称行为通过。

reviewer 已撤回"host crash 中断 daemon flight"并无静态 P0/P1；本批只闭合四条**产品恢复要求**
边界，不做重写、不加审计/框架。

| # | 失败方式 | 证据 | 修法 |
| --- | --- | --- | --- |
| F32 | **查询按 target P 取最新 C，旧 C 被跳过**：`QueryRemoteCancel{operationId}` 的 operationId 是 **prompt** id，终态查 `cancel_term_by_prompt` 按 `settled_at_ms DESC` 取最新。同一 prompt 上有 C1、C2 两个取消时，问 C1 会拿到 C2 的结局 | daemon.rs QueryRemoteCancel 臂走 `cancel_term_by_prompt`；protocol 命令无 cancel 操作 id | 命令增 `cancel_operation_id: Option<Uuid>`：给出时**按该 id 精确**查终态并校验它确属此 prompt/主体；缺省时维持现语义。查询**仍只读**，无副作用、不重发 cancel |
| F33 | **墓碑后 host 无恢复源，只能靠手机重放**：daemon 清绑定/删行后，`ListRemotePrompts` 不再带 `cancel_operation_id`；host dedup 的 C 记录**没有 target 绑定**，重启后无法把"未决 C"关联到 prompt | state.rs `recover_queues` 只认 daemon 行上的绑定；dedup `record_accepted` 不存 cancel target | host dedup `operations` 增 `cancel_target` 列（幂等迁移，同 F17 模式）；`run_cancel_queued` 受理时写入；启动扫描 `kind='CancelQueuedPrompt' AND state='accepted' AND cancel_target IS NOT NULL` 主动进恢复队列。主体 device/grant 取记录与实时 grant，后台清扫权限**不**混同于当前执行授权 |
| F34 | **orphaned take 先删后查，RPC 失败即丢**：`take_orphaned_cancels` 在查询**之前** drain；查询失败/被拒时条目已不在集合里，下一 tick 不会重试，C 永远停在 accepted | main.rs `resolve_orphaned_cancels` 首行 `take_orphaned_cancels()` | 改为**先查后取**：只有拿到确定性答复才移除；查询失败/被拒则原样保留，下一 tick 再次**只查询**。不清 hold、不重新派发原 P、不 autoredrive |
| F35 | **cancel 收敛未过 live 闸，活请求可被误取消**：入口在 CAS claim **之前**就按 submission 收敛 pending cancel。行 `dispatching` 既可能是崩溃残留，也可能是另一连接正在飞的 dispatch；后者被收敛即取消一个活请求 | daemon.rs:756（cancel 检查早于 785 的 CAS claim） | 增 `live_dispatches` 内存集合：claim 成功即入、所有出口即出。入口仅当该 operation **不在** live 集（可证明非 live）才按 submission 权威收敛；在飞则原样回报行状态。**拒绝** reviewer "query pending 即 cancel Preparing" 的无条件建议 |
| F36 | **终态/清绑定/删行是三个独立事务，却宣称原子**：`converge_pending_cancel` 依次 `record_cancel_term` → `resolve_cancel_intent`(自带事务) → `delete_remote_prompt`，任一步崩溃留下中间态 | daemon.rs converge 实现；persistence.rs 三个各自开连接的方法 | 合并为**单事务** `settle_cancel_and_delete_row(operation_id, state)`：读绑定 → 写终态 → 清绑定 → 删行，一个 `transaction()` 内完成 |

不在本批：重写 0005/0006/0007（SQL 均不改，0006 注释已修）；PWA/supervisor/schema；公网验收。

## 铁律（本批不复改）

- `Accepted` = 意图已持久，**不是**取消成功，也**不是**终态。
- `QueryRemoteCancel` 是**只读**查询；任何情况下不因查询产生副作用、不重新提交 cancel。
- 缺失 prompt 行绝不推出 cancelled；无终态记录 → `unknown`。
- 已交驱动绝不宣 cancelled/notSent。
- 未知不 autoredrive；不清 hold 换取 daemon 收敛。
