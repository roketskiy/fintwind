# Bug 调查报告：fintwind daemon is disconnected 全局断连且无法恢复

日期：2026-09-14　|　现场版本：release 0.1.1（target/release，今日 19:39–19:41 构建）　|　状态：已定位根因，未修复

## 现场还原

| 时间 | 现象 |
| --- | --- |
| 20:07:52 | toast「无法保存本地状态： fintwind daemon is disconnected」（草稿自动保存，走 daemon RPC） |
| 20:07:58 | toast「无法从 OpenCode 同步会话： fintwind daemon is disconnected」 |
| 20:08:27 | 主会话回合已结束（12m21s，统计页脚可见）；右侧审查任务显示「已断开」 |
| 20:08:40 | 点击「继续」→ 回合内错误「无法启动智能体： fintwind daemon is disconnected」（构建 · 0ms，立即失败） |

断连持续 ≥48 秒（且用户感知为持续故障），期间应用未退出、界面正常渲染、桌面本地写入（`state.json`，20:08）照常工作——**只有经过 daemon 的 RPC 全部失败**。

## 结论（TL;DR）

一次瞬时的 WebSocket 连接中断被架构放大成了永久性全局故障：

1. **`DaemonClient` 的断开是终态。** 后台 socket 线程退出一次就把 `disconnected` 置位（`crates/fintwind-client/src/client.rs:335`），此后所有 `request`/`notify` 直接 `bail!("fintwind daemon is disconnected")`（`client.rs:156`、`client.rs:193`）。没有任何代码路径会重置它或重建连接。
2. **`DaemonSupervisor` 只在两种情况下才更换 client**：daemon 子进程退出，或（仅 debug 构建）daemon exe 被重新链接（`crates/fintwind-client/src/process.rs:610` 的 `monitor_daemon`）。「进程活着、连接死了」对它完全不可见。
3. **协议层的断线重放机制从未被桌面端使用。** 服务端有完整的 resume 设计（`Hello.resume_from` + epoch/sequence 游标 + 每会话 4096 条 replay journal，`crates/fintwind-core/src/server.rs:241`），客户端也有 `connect_with_resume` API（`client.rs:57`）——但全仓库没有任何调用者；连 daemon 热替换走的都是裸 `connect`。

结果：任何一次瞬时连接错误（一帧的写入失败、一个未分类的 websocket 错误）都会让整个应用的所有 daemon RPC 永久失败，直到用户手动重启应用。locale 里 `daemon.reconnect`（重新连接）、`daemon.phase_disconnected`（已断开）、`errors.daemon_disconnected` 等键在代码中零引用——重连 UI 规划过，从未实现。

## 错误传播链

```
socket 线程退出（任意一次未分类错误/对端关闭）
  └─ client.rs:335  disconnected = true（终态）
       ├─ request/notify 全部 bail "fintwind daemon is disconnected"
       │    ├─ 草稿自动保存（ComposerDraftStore::remote，daemon RPC）→ toast 20:07:52 (src/app/drafts.rs:210)
       │    ├─ OpenCode 会话同步 → toast 20:07:58 (src/app/native_sessions.rs:300)
       │    └─ 「继续」启动智能体 → 回合错误 20:08:40 (src/app/runtime.rs:2648)
       ├─ 事件订阅通道关闭 → src/driver/mod.rs:112 合成 ProcessExited
       │    └─ 后台审查任务标记 Lost → 右侧面板「已断开」(src/app/background_work.rs:622)
       └─ supervisor.client() 依旧返回这个死 client（target 从未更换）
            └─ 500ms 轮询的 monitor_daemon 检查的是进程存活，不是连接存活
```

## 为什么判定「进程活着、连接死了」（或等效情形）

- 若 daemon **进程**曾退出，`monitor_daemon` 会在 ~500ms 内检测到 `has_exited` 并重启 + 广播新 client（`process.rs:641`），错误应秒级自愈。实际断连持续 ≥48s。
- Windows 事件日志与 WER：今天（9/14 19:40–20:30 及前后）**没有任何** `fintwind.exe` / `fintwind-daemon.exe` 的崩溃记录（最近一次 app 崩溃是 9/13 23:44，与本案无关）。
- 桌面端本地持久化（`state.json`、composer-drafts 等）在 20:08 仍正常写入，说明桌面进程健康；挂掉的只是 daemon RPC 这一条路。

也存在等效亚型：daemon 进程确实退了，但重启持续失败——`replace_local_daemon` 失败后 target 永远停在 `Restarting(死 client)`，只向 stderr 打一行 `could not restart rebuilt fintwind daemon`（`process.rs:644`）后无限重试。两种亚型的共同点是同一个：**桌面端没有任何机制把应用从死 client 里救出来，也没有任何 UI 反馈**。区分二者需要 daemon 的 stderr（release 从 Explorer 启动时丢失），这是当前的诊断盲区。

## 断连的触发点为什么没能 100% 定位

两端的读循环都用兜底分支把「一切未分类错误」当作致命错误静默断开：

- 客户端 `client.rs:331`：`Err(_) => break`（覆盖 Protocol/Utf8/Capacity/WriteBufferFull 及非 retryable IO 等所有错误），断开后**不记录任何原因**；
- 服务端 `server.rs:628`：`return Err(...)`，仅在连接线程退出时 `eprintln` 一行——release 下 stderr 不可见；
- 双方均无 ping/keepalive，半开连接无法被主动探测（对 localhost 影响小，对「暴露到局域网」模式是真实风险）。

没有日志 + 静默丢弃错误原因 ⇒ 只能锁定架构性根因，无法锁定那一帧的具体错误。**「断连原因被静默丢弃」本身就是本次要修的缺陷之一。**

## 已排查并排除的假设

| 假设 | 结论 |
| --- | --- |
| 工作区未提交改动（TurnStats/重试卡片）引入崩溃 | 排除。opencode.rs 的统计累加全部 `saturating_add`，`event_to_wire` 编码失败会被映射为 `error` 事件而不是断连；改动均为 UI/事件编解码层，与 socket 无关 |
| 协议版本不匹配导致握手失败 | 排除。`PROTOCOL_VERSION` 未改，且两端同源码构建 |
| daemon 硬崩溃（access violation） | 排除。WER/事件日志无今日记录 |
| dev watcher 热替换 daemon | 不适用。本案是 release 实例（`watch_for_rebuilds = false`） |
| `MAX_CONNECTIONS`(64) 耗尽 | 排除。只拒绝新连接，不影响既有连接 |
| 机器休眠/网络切换 | 排除。系统事件日志 19:40–20:30 无记录 |
| 运行中的 `opencode serve --service`（12:59 启动） | 无关。独立服务，不是 daemon 子进程 |

## 修复建议（按优先级）

**P0 — 断连自动恢复（核心修复）**
1. `DaemonClient` 暴露 `is_disconnected()`。
2. `monitor_daemon` 的存活检查从「进程退出」扩展为「进程退出 **或** `target.client().is_disconnected()`」：
   - 本地 daemon：进程若真死了，现有重启路径即正解；若进程活着只是 socket 死了，重启进程同样能救回来（代价：会话 runtime 重建，现有 `runtime_attach`/`restart_task_state_sync`/`subscribe_clients` 链路本就为此设计）。
   - 远程 daemon：用 `connect_with_resume(self.last_sequences())` 原地重连，服务端 journal 重放（每会话 ≤4096 条）让恢复近似无损——这套机制已实现，只缺调用者。
3. 恢复成功后沿用现有 `client_updates` 广播，应用侧无需新机制。

**P1 — 可观测性**
- 客户端 `run_client` 退出时记录断连原因（服务端已有 `eprintln`，客户端完全静默）；考虑接入持久化日志，避免 release 下诊断盲区。
- 断连期间的 toast 文案区分「正在重连/重连失败」，而不是把裸错误抛给用户。

**P2 — 加固**
- 可选 ping/keepalive + 空闲超时，主动探测半开连接（对暴露到局域网的 daemon 与 web 客户端价值最大）。
- 审视两端 `Err(_) => break` 的错误分类：至少把 Capacity（单条超限）改为跳过该消息而不是杀死整条连接。

## 附注

事件日志显示 9/13 有两次 `fintwind.exe` 的 `0xc0000005` 崩溃（0.1.0 与 0.1.1 各一次），与本案无关，建议另开 issue 跟踪。
