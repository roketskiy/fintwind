# 第五批失败矩阵与最小修复范围（2026-10-09，reviewer F13-F16 后续两项）

审查事实已逐条对照源码确认（非推论）：

| # | 失败方式 | 证据 | 修法 |
| --- | --- | --- | --- |
| F17 | **旧库升级即崩**：`open()` 只有 `CREATE TABLE IF NOT EXISTS`，旧 `operations` 表（无 `content_hash` 列）不会被加列；随后 `check()` 的 SELECT 与 `record_accepted()` 的 INSERT 都引用该列 → 旧库每次 open 后必 SQL 错 | dedup.rs:109-124（建表）、:155（SELECT）、:233（INSERT） | open 内**幂等事务迁移**：`PRAGMA table_info` 探测列，缺失则事务内 `ALTER TABLE ADD COLUMN`；保留全部既有行（不删除、不重建）；并发 open 的 duplicate-column 竞态按 SQLite 特定错误码放行，其余错误一律上抛 |
| F18 | **旧无 hash 行被静默采用第一份正文**：迁移后旧行 `content_hash` 为 NULL，`check()` 的 `if let Some(stored_hash)` 直接跳过比对 → 带 prompt 的重放无法鉴别正文，等于回到 F15 的静默收养 | dedup.rs:184-193 | **fail-closed**：行无 hash 而请求带 hash（prompt-carrying）→ 拒（Malformed），不猜、不放过；不带 prompt 的 operation（rename/attach/read 等）两侧皆 None，行为不变。旧行的终态（unknown/dispatched/...）仍按其记录回答案，**不**因此重新执行 |
| F19 | **取消意图丢失**：`run_cancel_queued` 只在 daemon 回 `cancelled` 时才算成功；行处于 `dispatching`（派发进行中，或 host 崩溃后遗留）时 daemon 回 `dispatching` → host 报 Rejected，意图丢弃；该行之后可能被恢复路径重新派发（对已取消的 prompt 再次运行），或永久卡在 `dispatching` 无人 settle | main.rs:1049-1093（cancel 只在 cancelled 时 settle）；daemon.rs cancel 的 `Dispatching` 早退；dispatch_ready_queues 的 `queued\|dispatching` 分支只 mark_queued | 队列项增持久意图标记：取消先记录意图再发 daemon；daemon 回 `dispatching` 时**保留意图**并回 Accepted（结局由 host 推送，不靠手机重试）；派发前/应答 `queued` 时立即兑现取消；应答 `dispatched` 时如实告知"已运行、取消太晚"，**绝不**宣 notSent |
| F20 | **新字段不重写已部署 migration**：daemon 侧 0005 已含 host_id/grant_version；若为 F17/F19 再改表结构必须同步升级旧库 | db/migrations/0005_remote_control_state.sql 已部署形态 | 本批**不新增 daemon 列**：取消意图存活于 host 调度状态 + daemon 行（权威），无需改 0005；dedup 迁移只加列、不重建 |

## F17/F18 迁移语义（旧库升级，写死在实现里）

`OperationStore::open` 的顺序：

1. `Connection::open` → WAL + synchronous=NORMAL（原样）。
2. `CREATE TABLE IF NOT EXISTS`（原样；新库直接带 `content_hash`）。
3. `ensure_content_hash_column`：
   - `PRAGMA table_info(operations)` 探测列名；已在 → 什么都不做（**幂等**，重复 open 无副作用）。
   - 缺失 → `BEGIN IMMEDIATE` 事务内**再次探测**，仍缺失才 `ALTER TABLE operations ADD COLUMN content_hash TEXT`，提交。
   - 为什么在事务内复检：两个进程同时 open 同一库都会看到"缺列"，后提交的那个若直接 ALTER 会撞 duplicate column。`IMMEDIATE` 让第二个等锁，等到的瞬间列已存在 → 它不改任何东西。**不吞 SQL 错误**：只有"确实不需要改"和"改成功"两条路，数据库锁损坏等真实错误原样上抛。
   - `ADD COLUMN` 只加列：既有行的 `operation_id/kind/summary/device_id/session_id/accepted_at_ms/expires_at_ms/state/reject_code/detail` 全部保留，dedup 记录不丢、不重建、不重播种。

**旧行（无 hash）的 fail-closed 语义**（`check`）：

| 行内 hash | 请求 hash | 判定 |
| --- | --- | --- |
| 有 | 有，不同 | 拒（Malformed）——不同正文 |
| 有 | 有，相同 | 放行（同请求重放，按其记录回答案） |
| **无**（旧行） | **有**（带 prompt 的重放） | **拒（Malformed）**——正文无法鉴别，不猜、不放过 |
| 无 | 无（rename/attach/read 等不带 prompt） | 放行，行为与升级前一致 |

被拒的是**重放请求**，不是原操作：原操作的终态（unknown/dispatched/expired/…）仍按其记录回答，`replay=true` 时 host 直接回记录状态并 `return`，**不会重新执行**。reviewer 建议的"skip hash 宽松兼容"未采纳——那等于让旧行重新获得 F15 的静默收养能力。

## F19 取消生命周期矩阵

| 时点 | daemon 行 | host 动作 | 设备得到 |
| --- | --- | --- | --- |
| 取消到达，行 `queued` | `queued` | 记录意图 → daemon cancel → `cancelled` → settle | `Executed {cancelled:true}` |
| 取消到达，行 `dispatching`（派发中/崩溃遗留） | `dispatching` | 记录意图 → daemon 拒（回 `dispatching`）→ **保留意图** | `Accepted`（结局由 host 推送，不靠手机重试） |
| 派发进行中意图已记录，派发应答 `queued`/`dispatching`（busy） | 回 `queued` | `mark_queued` → **立即兑现取消** | 本轮内 `Executed {cancelled:true}`（prompt 与 cancel 两个 operation 都推） |
| 意图已记录，下一 tick 准备派发前 | `queued` | **先兑现取消**，再考虑派发 | 同上 |
| 意图已记录，派发应答 `dispatched` | `dispatched` | prompt 已运行 → 如实告知 | prompt: `Executed {dispatched:true}`；cancel: `Rejected{Malformed}` "the prompt had already reached the driver" |
| 意图已记录，daemon 无权威应答 | 未知 | **不自动重发**，意图保留至行 settle | `Unknown` |

硬约束：已交给驱动的 prompt 绝不取消、绝不宣 notSent；未知副作用绝不自动重发；取消不扩大锁（所有 daemon 调用仍在锁外）；host 崩溃后意图不存活于队列，但 daemon 行仍在，设备重发一次取消即可（非无限重试）。

不在本批：改 desktop-owned helper/fixture、公网验收、Cloudflare、PWA 兼容（main 做）。
