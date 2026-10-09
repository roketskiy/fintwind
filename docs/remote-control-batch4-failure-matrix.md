# 第四批失败矩阵与最小修复范围（2026-10-09）

审查事实已逐条对照源码确认（非推论）：

| # | 失败方式 | 证据 | 修法 |
| --- | --- | --- | --- |
| F1 | `admit_remote_prompt` 持 `task_state` 锁跨 `accept_remote_submission` 重锁 → **parking_lot 死锁** | daemon.rs:466 guard 活到 524 调用；helper 571 再 `.lock()` | 短锁只做目标校验后释放；提交走独立短锁的 `commit_remote_admission`（submission 行 + remote 行同一事务）；**不克隆会话、不预留 turn**（见 F10） |
| F2 | `dispatch_remote_prompt` 持锁跨 prepare/capture_checkpoint/ensure_runtime/dispatch_inner 三重重锁 → 死锁 | 687 持锁；743→332、747→830、748→904 均 `.lock()` | 短锁快照后释放；prepare/runtime/dispatch 期间复检目标/TTL/archived，CAS claim 持久 dispatch；失败 Unknown 不自动重发 |
| F3 | `RemotePromptRow.device_id` 恒 `Uuid::nil()`，`caller` 常量 `"remote-host"` | daemon.rs:516；恢复后结果路由不到原设备，授权复检无对象 | 新增 `RemoteProvenance{host_id, device_id, grant_version}` 贯穿 admit/cancel/dispatch/list，持久列 + host 侧复检 |
| F4 | admit 回应 `operation_id` 填成 submission id | daemon.rs:637 | 回原 operation id |
| F5 | admit 不按 operation_id 查旧行，`INSERT OR REPLACE` 可覆盖同 operation | daemon.rs:512-523 | 入口按 operation_id 查既有行：同请求回 existing truth；异 session/device/summary 拒；已 dispatched/unknown 绝不新 POST |
| F6 | 过期队首 `continue` 静默跳过、卡死 FIFO、daemon 行残留 | main.rs:1186 | settle 为 Expired：daemon cancel + 清正文 + 通知原 device + 推进 next；`expire_due` 同样释放 daemon 行 |
| F7 | `createSession` payload 只拒 4 个 key，`runtimeMode`/未知 key 放过 | adapter.rs:271 | 严格白名单：仅 `projectId` |
| F8 | archive 时 stop 失败仍返回成功并存档；且**持 `task_state` 跨 `driver.cancel()`**、循环内调自取锁的 `abandon_remote_prompt` → 带等待 prompt 的归档必死锁 | daemon.rs archive | 先读行再释放锁逐个 abandon；driver 调用在无锁时进行；无运行时可停即拒且不写 `archived`（cancel 为 fire-and-forget，不宣称同步确认） |
| F9 | 双设备审批唯一 driver 决定 / 双 host / QueueFull 后 transcript+offer 恢复 | 未验 | 本批只补 persistent-control 真实 daemon 矩阵；双设备审批仍需 provider turn，明确记为缺口 |
| F10 | **（实现期发现的更深缺陷，评审未覆盖）** 受理时在会话上 `begin_turn`：busy 会话被追加第二个 Running turn，而事件转发器只结束**最后一轮** → 真正在跑那轮永不 settle、会话永久 busy、队列堵死；且 accept 的 active-submission 检查会直接拒绝 busy 普通发送 | daemon.rs 旧 admit/accept + `spawn_remote_event_forwarder` 的 `turns.last()` | **受理不建轮**：受理只写 durable 记录（busy 也受理并回 `queued`）；轮在派发时由 `materialize_remote_turn` 用受理时绑定的 submission id 建一次，且派发前有 daemon 侧 busy 复检（`is_busy() \|\| has_unsettled_submissions()` → 行保持 `queued`）。顺带修掉 checkpoint off-by-one（先建轮再取序号） |
| F11 | **（实现期发现）** daemon 自持 runtime 不向 hub 注册、事件不回灌 → host 收不到 `turnStarted/turnFinished`，既不知忙闲也看不到 permission/question | `EventSink::detached` 自建空 Hub | `EventSink::rebound` + `open_runtime`：派发时把请求 sink 重绑到该 runtime 坐标并在 hub 上开启，转发器同时落 task state 与发 hub |
| F12 | **（实现期发现）** 撤销/重新配对只 drop host 队，不取消 daemon 侧持久行 → 被撤销设备在这台电脑账上留 prompt，重启还会恢复 | relay_session.rs  grant 变更分支 | `drop_device` 后经 `HostState::cancel_remote_prompt`（带该设备 provenance）取消 daemon 侧全部行 |
| F13 | **（reviewer 最终报告 confirmed TOCTOU）** dispatch 的 busy 检查在短锁内（~751）完成后即释放，到 `materialize_remote_turn`（~808）之间要过 CAS(SQLite) + target 复检(SQLite)；本机 GUI 的 `AcceptSubmission`（daemon.rs:1548-1636，客户端已 begin_turn 的 session 在 `task_state` 临界区内整体提交，自带 busy 卫 1593-1607）可在此窗口提交 Running turn。`materialize_remote_turn`（1032-1038）只按**同一 submission id** 幂等，不挡**不同** submission → 追加第二个 Running turn；事件转发器只 settle `turns.last()` → 本机那轮永不 settle、会话永久 busy。远端 row CAS 挡不住本机发送 | daemon.rs 快照 751→materialize 808；AcceptSubmission 1567-1629 | **建轮点权威复检**：`materialize_remote_turn` 在既有的 `task_state` 临界区内（打开 turn 之前）复检 `is_busy() \|\| has_unsettled_submissions()`。因本机 check+commit 与本次 check+append 同在 `task_state` 下原子，窗口闭合：我之前开轮的我看得见，我之后开轮的看得见我。返回三态——`Opened` 继续；`Busy` 把行 CAS 回 `dispatching→queued` 并回 `queued`（保 queued 重试、不误标 Unknown、不丢原 operationId）；`Gone`（会话被删/归档，行已被该路径 settle）按 submission store 的记录回标签，绝不重驱。不新增跨 I/O 的锁 |
| F14 | **（reviewer confirmed）** dispatch 的 grant 校验是 `provenance.grant_version < row.grant_version`（daemon.rs:671）：重配对后新版本 V2>V1 仍放行旧 row，设备在新 grant 下"收养"并执行旧 prompt；cancel（1336）则完全没有版本校验 | daemon.rs:671、1336；pair.rs:373 `grant_version: now_ms()` | **精确相等**：dispatch/cancel 均要求 `provenance.grant_version == row.grant_version`（device/host 早已精确）。**执行与清扫分界**：dispatch 用 host **当前** grant 版本（重配后 V2≠V1 → 旧行拒绝执行、保持 queued 等清扫）；清扫/过期/撤销/archive 的 cancel 引用**行自身记录的** grant_version（host 队列项随 admit 记录、recover 时从 `ListRemotePrompts` 刷新），因此清扫不会被 `!=` 挡死。可信 host 可清旧行，设备不能在新 grant 下执行——两者是不同来源，不扩宽任意请求 scope |
| F15 | **（reviewer confirmed）** admit 幂等只比 session/device/summary（488-492），不比 prompt：同 operationId 不同正文被静默按第一份回答；host SQLite dedup 也只存 summary，会拦在 daemon 前面 | daemon.rs:488-492；dedup.rs `check()` 比 kind+summary+device | daemon 侧补 `existing.prompt`/`host_id`/`grant_version` 精确比对，不同请求同 id 一律拒绝。host 侧 `operations` 增 `content_hash` 列（仅 `sendPrompt`，sha256(trimmed prompt)）：同 id 不同正文在 host 层即拒，不静默采用第一份；hash 不出主机、不存正文、不缓存读取；`operationId` 查询语义与终态持久查询不变。不做超出此范围的口令/密文模块改动 |
| F16 | **（reviewer 要求核实的 truth 字段）** `delete_sessions` 的 `stopped` 实为"daemon 接受了 cancel"（fire-and-forget），字段名宣称已停止 | adapter.rs delete_sessions | fire-and-forget 不宣停止确认：archive 路径本就只回 `{sessionId, archived}`（无停止宣称，保持）；delete 结果改报 `stopRequested`（停止已被运行时接受，非轮次结束的确认；结束以事件为准） |

不在本批：改 desktop-owned helper/fixture（需求交 main）、公网验收、Cloudflare。

实现期新增发现（F10–F12）说明一件事：评审只做了语言级 guard 生命周期验证，没有真实 daemon
行为，因此"busy admit 只是风险"实际是必现缺陷。凡未经真实 daemon 运行证实的结论，本批一律
按代码事实修、并按未验收记录，不把编译通过当行为证据。
