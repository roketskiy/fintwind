# 第七批失败矩阵与最小修复范围（2026-10-09，reviewer F21-F25 后续 P1）

审查事实已逐条对照源码确认（非推论）。reviewer 要求先验证触发序列，结论如下。

## 崩溃序列核验（不以 host crash 默认杀 daemon）

daemon 的派发跑在**自己的连接线程**上：workspace 准备、runtime 启动、`driver.submit` 都不依赖
host 的存亡。host 单独崩溃时 daemon 的在飞请求**会正常完成**，行迁移到 `dispatched`，cancel 收敛。
所以"host crash 中断 daemon dispatch"这一前提**不成立**；真正需要收敛的是下列四种：

| 场景 | 行状态 | submission 状态 | 权威证据 | 应收敛为 |
| --- | --- | --- | --- | --- |
| host 单独崩溃（daemon 在飞） | `dispatching`→`dispatched` | `Dispatching`→… | submission |  prompt 已运行，C 得"太晚"；host 重启后必须能查到并答复 C |
| daemon 崩溃于 submit 前 | `dispatching` | `Preparing` | submission = 从未交驱动 | 可兑现取消 |
| daemon 崩溃于 submit 后、行迁移前 | `dispatching` | `Dispatching` | submission = 已交驱动 | **不得取消**，C 得"太晚" |
| daemon 错误退出 / 线程 error | 同 daemon 崩溃 | 同 | submission | 同 |
| 双方崩溃 | `dispatching` | 任一 | submission 是唯一证据 | 同上 |

**reviewer 的"dispatching 行 CAS 失败"推断不成立**：现 dispatch 入口（daemon.rs:748）已有
hydrate + submission 状态复检，`Dispatching` 状态会被拦下并回报实情。但该检查位于 cancel 兑现
**之后**，于是引出下面的 F26。

| # | 失败方式 | 证据 | 修法 |
| --- | --- | --- | --- |
| F26 | **取消先于 submission 权威检查**：入口 728 行 `honour_durable_cancel` 在 748 行 submission 状态复检**之前**。daemon 崩溃于 submit 后（行 `dispatching`、submission `Dispatching`）时，恢复派发会把一个**已交驱动**的 prompt 取消并回报 `cancelled` —— 直接违反"已交驱动不能宣 cancelled/notSent" | daemon.rs:724-730（cancel）早于 :742-750（submission 复检） | 入口改为**先问 submission 权威**：`Preparing`（可证明从未交驱动）才允许兑现取消；其余状态按 submission 实情收敛 cancel，绝不 `cancelled` |
| F27 | **dispatched 行 + pending cancel 被 recover 过滤，C 永久失联**：host `recover_queues` 只恢复 `queued \| dispatching` 行；行已到 `dispatched` 而 cancel 仍 pending 时，C 永远停在 `Accepted` | state.rs:713 `matches!(prompt.state.as_str(), "queued" \| "dispatching")` | recover 扫描**全部**行，凡带 `cancel_operation_id` 的一律纳入 C 的待解析集合（与是否可派发解耦）；daemon 侧在 `dispatching→dispatched` 迁移点把 cancel 收敛为终态并落 terminal 记录 |
| F28 | **行删除后 `absent` 被当成 cancelled**：`honour_pending_cancel` 把 `absent` 映射为"未运行→取消成功"。但 Gone/失败路径本是 `Unknown`，缺权威状态时推断成功即假承诺 | main.rs `honour_pending_cancel` 的 `"absent" \| … => cancelled` 分支 | **移除 absent→cancelled 推断**：无 terminal 记录且行不存在 → `Unknown`，不是成功。终态只能来自 daemon 亲自写下的记录 |
| F29 | **终态随行删除而消失，Crash 后 C 无法查询**：`remote_prompts` 行被删（会话删除/清扫/派发后清理）时 cancel 绑定随之消失，`QueryRemoteCancel` 只能回 `absent` | daemon.rs delete_remote_prompt 无 cancel 保全 | 新增 **migration 0007 `remote_cancel_terms`**：`cancel_operation_id` 主键，绑定 `prompt_operation_id / submission_id / host_id / device_id / grant_version / state / settled_at_ms`，**不含正文**。取消收敛时同事务写入；`QueryRemoteCancel` 行缺失时以它为权威。0005/0006 不重写 |
| F30 | **C 重放永远 Accepted**：host 在 RPC **前** `record_accepted(C)`；若 host 崩溃于"落 accepted 之后、daemon 记录之前"，重启后无绑定，设备重放 C 得到 `accepted` 回放，而 prompt 随后被正常派发 —— Accepted 被当成保证 | main.rs `run_cancel_queued` 的 record_accepted 早于 RPC；重放路径直接回记录状态 | 重放一个仍为 `accepted` 的 C 时，host **只查询** daemon（`QueryRemoteCancel`）并按权威状态答复，不重发取消副作用；无绑定且无 terminal → `Unknown`。 Accepted 的语义改为"意图已持久，结局未定" |
| F31 | **0006 注释过度承诺**：SQL 注释称取消绑定使"prompt will not run"成为保证 | db/migrations/0006 头部注释 | 改注释：绑定保证的是**意图不丢**（Crash 后可查询、可收敛），不保证取消成功。SQL 不变 |

不在本批：F17/F18（reviewer 已确认）；PWA/supervisor/schema 源（main）；公网验收。

## 收敛规则（authority = submission store + terminal record）

`QueryRemoteCancel` 与 dispatch 入口共用同一条判据，**先 submission、后 cancel**：

| submission 状态 | 含义 | cancel 收敛为 |
| --- | --- | --- |
| `Preparing` | 可证明从未交驱动 | 可兑现 → `cancelled` |
| `Dispatching` / `Accepted` | 已交驱动（或驱动已确认） | `dispatched`（太晚） |
| `NotSent` / `Rejected` | 驱动拒绝/未发 | prompt 未运行 → `cancelled` |
| `Unknown` | 真相未知 | `unknown`（**不**推断成功） |
| submission 缺失 | 无权威记录 | `unknown` |
| 行缺失 | 看 terminal 记录：有 → 其 state；无 → `unknown` |

铁律：不存在行**绝不**推出 cancelled；已交驱动**绝不**宣 notSent；未知**绝不**自动重发；
终态查询在 Crash 后仍可用（0007 表），并绑定 original C / target P / submission / host / device / grant。
