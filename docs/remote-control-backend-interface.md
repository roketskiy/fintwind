# 后端接口交接（2026-10-09，第四批）

消费方：PWA（`src/api/session.ts`）、relay（`RejectedFrame`）、主代理（typegen / schema）。
本文只记**已冻结的线上契约**与归属；实现状态见 `progress.md`。协议仍为 **version 2，未发布**，
字段一律 camelCase，未知字段一律 `deny_unknown_fields` 拒绝。

本批变更（相对第三批）：`caller: String` 全部改为 `provenance: RemoteProvenance`；受理不再预留
turn；派发在会话空闲时才建轮；busy 普通发送只入队不 steer；过期队首落 durability；撤销释放
daemon 侧持久行。文末列出**未验收项**与验收步骤。

## 1. 新 daemon 命令（`fintwind-protocol`，core 实现）

远端只可信命名 **sessionId / operationId / projectId / prompt 文本 / TTL / provenance**。
`project`、`AgentSession`、`WireDriverStartOptions`、`binary`、`cwd` 一律不接受。

`provenance` 由 **host 自己**填写，手机不给：host 端到端已核验设备，经其本就信任的 loopback
IPC 告知 daemon。设备因此只能"请求"，不能点名别的设备、别的 grant 版本或任何可信对象。

```rust
pub struct RemoteProvenance {   // serde: camelCase, deny_unknown_fields
    pub host_id: Uuid,           // host 自己的 identity.host_id（relay 注册身份）
    pub device_id: Uuid,         // 已核验设备
    pub grant_version: u64,      // host 当前持有的该设备 grant 版本（现读 grant 文件）
}
```

| Command | payload | 返回 |
| --- | --- | --- |
| `CreateRemoteTask` | `{provenance, projectId}` | `RemoteTaskCreated {sessionId}` |
| `AdmitRemotePrompt` | `{provenance, sessionId, operationId, prompt, expiresAtMs, summary}` | `RemotePromptAdmitted {operationId, submissionId, state}` |
| `DispatchRemotePrompt` | `{provenance, operationId}` | `RemotePromptState {operationId, submissionId, state}` |
| `CancelRemotePrompt` | `{provenance, operationId, cancelOperationId?}` | `RemotePromptState {…}` |
| `ListRemotePrompts` | `{provenance}` | `RemotePrompts {prompts: [RemotePromptRecord]}` |
| `QueryRemoteCancel` | `{provenance, operationId}` | `RemoteCancelState {operationId, submissionId, cancelOperationId, cancelState}` |
| `ArchiveRemoteSession` | `{provenance, sessionId}` | `Ack` |
| `RestoreRemoteSession` | `{provenance, sessionId}` | `Ack` |
| `SetRemoteSessionOptions` | `{provenance, sessionId, options: WireSessionOptions}` | `OptionsApplied {applied}` |

`RemotePromptRecord { operationId, submissionId, sessionId, deviceId, state, summary,
enqueuedAtMs, expiresAtMs, grantVersion, cancelOperationId?, cancelState? }` —— **不含 prompt
正文**；正文只存在于 daemon 的 durable 行，派发时由 daemon 自己取出。`state ∈ queued |
dispatching | dispatched | cancelled | expired | notSent | unknown`，常量见
`fintwind_protocol::submission::RemotePromptState`。`cancelOperationId`/`cancelState` 是本批新增的
**durable 取消绑定**：前者是正在等待结局的设备取消操作，后者是 `pending | cancelled |
dispatched | expired | notSent | unknown`。恢复时 host 据此重建"不得派发"集合。

持久化：`db/migrations/0005_remote_control_state.sql`（`remote_prompts` 表 + `sessions.archived`
列，默认 0）。接受 submission 与 remote 行**同一事务**；`PRAGMA synchronous=FULL`。
`remote_prompts` 增列 `host_id text NOT NULL`、`grant_version integer NOT NULL DEFAULT 0`
（0005 未发布，直接改；无旧库需迁移）。
**本批新增 `db/migrations/0006_remote_cancel_intent.sql`**：`ALTER TABLE remote_prompts ADD COLUMN
cancel_operation_id text` + `cancel_state text`。0005 已按 tag 记录为已应用，直接改它不会升级旧库，
故取消绑定走新 migration，旧库启动时自动应用、只加列不重建。
**第七批新增 `db/migrations/0007_remote_cancel_terms.sql`**：`remote_cancel_terms` 表
（`cancel_operation_id` 主键 + `prompt_operation_id` / `submission_id` / `host_id` /
`device_id` / `grant_version` / `state` / `settled_at_ms`，**不含正文**）。prompt 行会被删除，
取消终态不能随之消失，否则 Crash 后设备只能得到猜测。0005/0006 均未重写。

### 1.1 受理（Admit）语义——本批修正

- **不建轮。** 受理只写 submission 记录 + remote 行，一个事务，不动会话 transcript。
  轮在**派发时**才建（`begin_turn_with_id(submission_id, prompt)`，model 新增 API），
  且只在会话空闲时建一次；重放/崩溃重入发现轮已存在则什么都不加。
- **busy 也受理。** 会话忙时普通发送照样落 durable 行并回 `queued`：它排队，绝不 steer，
  绝不会被拒（第三批的"session already has an active submission"拒绝已从远端路径移除）。
- **幂等按 operationId。** 入口先查既有行：同 session/device/host/grant/summary/**prompt**
  → 回既有真相（`RemotePromptState`，携带原 submissionId）；任一项不同（尤其 prompt 正文不同）
  → 拒绝（"operation id is already used by a different request"），**不**静默按第一份正文回答。
- **回应回显原 operationId**（第三批误填 submissionId，已修）。

### 1.2 派发（Dispatch）语义——本批修正

按序，任一步不满足即停，**绝不自动重发/重执行**：

1. provenance 复检：`row.device_id == provenance.device_id`、`row.host_id == provenance.host_id`，
   且 **`provenance.grant_version == row.grant_version`（精确相等，不是"不更旧"）**。重配对给设备的
   是*更新*的版本，"更新即放行"会让设备在新 grant 下收养并执行旧 prompt；撤销/重新配对后旧行失效。
2. 行状态非 pending（dispatched/cancelled/expired/notSent/unknown）→ 回记录状态。
3. 行已过 TTL → 删行，回 `expired`。
4. **submission 状态复检**：只有仍 `Preparing` 的行才允许派发。崩溃会把行留在 `dispatching`，
   而"驱动是否收到过"是 submission store 里的事实不是猜测；已 settle 的一律按记录回真相。
5. 短锁快照 + **busy 复检**：`is_busy() || has_unsettled_submissions()` → 行保持 `queued`，
   回 `queued`。host 的"空闲"是从事件学来的，不是证据；daemon 查自己的。
6. CAS claim：`queued → dispatching`（`transition_remote_prompt` 比较交换），失败回当前状态。
7. 目标复检：会话被删/归档 → 不派发。
8. 建轮（`materialize_remote_turn`，返回该轮序号；建轮临界区内有**权威** busy 复检，见 1.2.1）
   → 解析 workspace + 抓该轮 checkpoint → `ensure_remote_runtime`（按会话自身记录 + daemon 可信
   probe 启动驱动，远端永不指定 binary）→ `dispatch_submission_inner`（driver.submit）。
9. 成功：`dispatching → dispatched`。失败：abandon（settle 该轮与 submission）+ 删行 + 回
   `unknown`；后续 tick 不会重试。

#### 1.2.1 建轮点互斥（TOCTOU，reviewer 最终报告 confirmed）

快照里的 busy 检查只是快速路径：它到建轮之间隔着一次持久 CAS 和两次加锁，**本机 GUI 可以在此
窗口提交一个 Running turn**（`AcceptSubmission` 在 `task_state` 临界区内整体提交客户端已
`begin_turn` 的 session）。远端 row 的 CAS 挡不住它——本机提交根本不碰 remote row；挡不住的后果
是会话出现两个 Running turn，而事件转发器只 settle `turns.last()`，另一轮永久 busy。

因此权威复检放在 `materialize_remote_turn` 的**同一个 `task_state` 临界区内、append 之前**。
本机的 check+commit 与本次的 check+append 同在 `task_state` 下原子，窗口闭合。三态：

| 结果 | 行状态 | 回应 |
| --- | --- | --- |
| `Opened(turn_count)` | `dispatching` | 继续派发 |
| `Busy` | CAS 回 `dispatching → queued` | `queued`：保重试、**不**误标 Unknown、不丢原 operationId |
| `Gone`（会话被删/归档，行已被该路径 settle） | 删行 | 按 submission store 的记录回标签，绝不重驱 |

事件通路：daemon 自持 runtime 时，事件既落 task state 也发往 hub（`EventSink::rebound` +
`open_runtime`），否则 host 收不到 `turnStarted/turnFinished`，既不知道忙闲也看不到
permission/question。host 收到 `queued`/`dispatching` 应答时把队标记回 Queued 等下个 tick，
**不**上报 settled。

#### 1.2.1 建轮点互斥（TOCTOU，reviewer 最终报告 confirmed）

快照里的 busy 检查只是快速路径：它到建轮之间隔着一次持久 CAS 和两次加锁，**本机 GUI 可以在此
窗口提交一个 Running turn**（`AcceptSubmission` 在 `task_state` 临界区内整体提交客户端已
`begin_turn` 的 session）。远端 row 的 CAS 挡不住它——本机提交根本不碰 remote row；挡不住的后果
是会话出现两个 Running turn，而事件转发器只 settle `turns.last()`，另一轮永久 busy。

因此权威复检放在 `materialize_remote_turn` 的**同一个 `task_state` 临界区内、append 之前**。
本机的 check+commit 与本次的 check+append 同在 `task_state` 下原子，窗口闭合。三态：

| 结果 | 行状态 | 回应 |
| --- | --- | --- |
| `Opened(turn_count)` | `dispatching` | 继续派发 |
| `Busy` | CAS 回 `dispatching → queued` | `queued`：保重试、**不**误标 Unknown、不丢原 operationId |
| `Gone`（会话被删/归档，行已被该路径 settle） | 删行 | 按 submission store 的记录回标签，绝不重驱 |

### 1.3 取消（Cancel）语义

- provenance 与 dispatch **同一条精确规则**：device + host + **grant 精确相等**。重配后的设备
  在新 grant 下取消不了旧行（那不是它现在持有的授权），撤销后同样取消不了。
- 只有 `queued` 行可取消：abandon（settle submission 为 NotSent、按剩余未决提交重算会话状态）
  + 删行 + 回 `cancelled`。
- `dispatching` 行**不立即取消**：结果正在决定中。daemon 改为把取消**持久绑定到行上**
  （`cancel_operation_id` + `cancel_state='pending'`）并回 `dispatching`；host 答 C `Accepted`，
  由派发迁移点解析。同 C 重放幂等；异 C 拒绝；行不存在拒绝。
- **查询而非重发**：`QueryRemoteCancel` 是 host 获取取消真相的唯一途径（Crash 恢复、RPC 无应答）。
- **`Accepted` 的语义是"意图已持久"，不是"取消会成功"**。已交驱动的 prompt 如实回报"太晚";
  证据耗尽的收敛为 `unknown`；两者都不会被升级成成功。
- **权威顺序**：submission 状态先于 cancel 兑现。只有 `Preparing`（可证明从未交驱动）才允许取消;
  `Dispatching`/`Accepted` 收敛为 `dispatched`；`NotSent`/`Rejected` 收敛为 `cancelled`;
  `Unknown` 或缺失收敛为 `unknown`。缺失的 prompt 行**绝不**推出 cancelled。
  只查询、绝不重发取消副作用；`unknown` 时 host hold 原 P 不派发。
- host 侧取消意图生命周期（durable 版，见 `docs/remote-control-batch6-failure-matrix.md`）：
  恢复时从 `ListRemotePrompts` 重建；派发前与派发应答 `queued` 后立即兑现；应答 `dispatched` 时
  如实告知"已运行、取消太晚"，**绝不**宣 notSent；host 自发释放行时按"未运行"结算 C。

### 1.4 归档（Archive）语义

- 忙任务先尝试停止：**不持 state 锁跨 driver 调用**（driver 自己的线程会取该锁），也无运行时
  可停即拒绝归档、不写 `archived`。cancel 是 fire-and-forget：本调用保证"对存在的运行时发起过
  停止"，不保证同步确认；轮由自身事件 settle。
- 归档取消该会话全部等待中的 prompt（先读行再逐个 abandon——abandon 自取锁）。
- 恢复只清标志，不启动任何轮。`archived` 经独立语句写入，旧投影抹不掉。

## 2. 远端操作（PWA → host）

| OperationKind | payload | detail（成功） |
| --- | --- | --- |
| `sendPrompt` | `{prompt}` | 空闲：`{dispatched:true}`；忙：`{queued:true, position, submissionId}` |
| `cancelQueuedPrompt` | `{operationId}` | `{cancelled, operationId, message?}` |
| `createSession` | **仅 `{projectId}`** | `{sessionId}` |
| `applySessionOptions` | `{interactionMode?, model?, agentPreset?, reasoningEffort?, serviceTier?, contextWindow?}` | `{applied, agentPreset}` |
| `loadOptionsCatalog` | `{}` | `null`，随后独立 `OptionsCatalog` |
| `renameSession` | `{title}` | `{sessionId, renamedOnProvider, localTitleSynced, providerError?}` |
| `deleteSessions` | `{sessionIds:[…]}` | `{results:[{sessionId,state,stopRequested,providerSessionRemoved,providerError?}]}` |
| `forkFromResponse` | **`{turnId}`** | `{sessionId, turnCount, warning?}` |
| `rewindToMessage` | **`{turnId}`** | 同上 |
| `archiveSession` / `restoreArchivedSession` | `{}` | `{sessionId, archived}` |
| `readTaskOutput` | `{}` | `{sessionId, content, truncated}` |
| `readFile` / `readDirectory` / `readDiff` | 见 `workspace_*` 类型 | 分页正文 |

**`createSession` payload 严格白名单**（本批修正）：只允许 `projectId` 一个 key。出现任何其它
key（含 `mode`、`runtimeMode`、`workspace`、`binary`、`cwd` 或未知字段）一律 `Malformed`
拒绝，不静默忽略——调用方要被告知它的请求没有生效。

**`stopRequested` 不是"已停止"。** driver 的 cancel 是 fire-and-forget：该字段只表示"停止已被一个活运行时接受"，
轮次是否结束由事件告知，没有任何字段宣称确认停止。同理 archive 只回 `{sessionId, archived}`，
不携带任何停止结论。

**`turnCount` 不再由设备提供。** `forkFromResponse` / `rewindToMessage` 只传 `turnId`
（即 `TranscriptMessage.turn.turnId`）。host 在派发前用**可信 hydrated session** 解析该
turn 当前的 `turn_count` 并核对：turn 已被回退消失即拒（`Malformed`，"that message is no
longer part of this task"），旧确认无法落到后来复用同序号的新 turn 上。

语义（core 真值，勿再解释成"从最新往回数 N 轮"）：
- `fork_session_from_response` 保留 source **前 `turn_count` 轮**；
- `rewind_session_to_message` 保留 **`turn_count - 1` 轮**。

### 2.1 host 队列与 daemon 的一致性（本批修正）

- 队首过期：**不再静默 `continue`**。host 取消 daemon 侧行 + 本队置 Cancelled 并清正文 +
  上报原设备 `Expired` + 推 `SessionQueue`，然后继续取下一项——过期项不再堵死 FIFO。
- `expire_due` 周期同样释放 daemon 侧行：daemon 的书与 host 的书不会对一个"永不会跑"的
  prompt 各说各话。
- 撤销/重新配对：`drop_device` 后**同时**取消 daemon 侧该设备全部行——被撤销设备不在这台
  电脑的账上留 prompt，重启也不会被恢复。
- **清扫与授权执行是两个来源**（reviewer 要求明确边界，勿机械 `cancel !=`）：

  | 路径 | provenance 里的 grant 版本 | 效果 |
  | --- | --- | --- |
  | 设备自己的 cancel（`run_cancel_queued`） | 该设备**当前** grant | 重配对后与行不符 → 被拒（旧 prompt 不属于新 grant） |
  | host 清扫（过期/撤销/会话消失/派发未确认） | **行自带**的 `grant_version`（host 队列项随 admit 记录、recover 时从 `ListRemotePrompts` 刷新） | 旧行照样释放，不会被 `!=` 挡死 |

  daemon 对两者施加**同一条精确相等规则**（device + host + version 全等）；不同的是"谁在问、
  它如实持哪个版本"。可信 host 因此总能清自己的账，而设备不能在新 grant 下收养并执行旧行——
  清扫只释放行，从不派发它们，任意请求的 scope 没有因此扩大。
- host dedup 的 prompt 绑定：`operations` 表增 `content_hash` 列（仅 `sendPrompt`），
  同 operationId 不同正文在 host 层即拒。**只存 hash**：prompt 正文不进该存储、不记日志、
  不被读回；`operationId` 查询语义与终态持久查询不变。
- **取消操作的幂等**（本批修正）：`CancelQueuedPrompt` 不再 bypass dedup——副作用前
  `record_accepted`、终态 `record_finished`，重放从记录回答，终态去重。
- **旧库升级**（`OperationStore::open` 幂等迁移）：`PRAGMA table_info` 探测 → 缺失则在
  `BEGIN IMMEDIATE` 事务内复检并 `ALTER TABLE ADD COLUMN`，只加列、不重建、不丢既有 dedup
  记录；并发 open 由事务内复检消解 duplicate-column 竞态，真实错误上抛不吞。
  **旧行无 hash 时 fail-closed**：带 prompt 的重放被拒（正文无法鉴别），不带 prompt 的
  operation 行为不变；原操作终态仍按记录回答，不重新执行。
- host 重启恢复：`ListRemotePrompts` 只恢复 `queued | dispatching` 行；daemon 侧的
  submission 状态复检保证"驱动没收到过"才会真的再派。

#### 2.1.1 两种取消来源，一条 daemon 规则（grant 精确化后必须分清）

daemon 要求 grant **精确相等**。因此 host 有两条取消路径，**引用不同的版本来源**：

| 路径 | 引用的 grant 版本 | 用途 |
| --- | --- | --- |
| `cancel_remote_prompt`（设备发起） | 设备**当前**的 grant 版本 | 设备自己的取消请求；重配后与旧行不符即被 daemon 拒——正确，因为旧行不属于它现在持有的授权 |
| `cancel_remote_prompt_as_recorded`（host 清扫） | **行自带**的 grant 版本（host 队列项随 admit 记录、recover 时由 `ListRemotePrompts` 刷新） | 过期、撤销、会话消失等后台清扫 |

为什么必须分两条：若清扫也引用"当前版本"，重配后 V2≠V1，被撤销设备的旧行将**永远无法释放**，
烂在这台电脑的账上。行自带版本是 daemon 受理时亲自记录的事实，host 只是转述它——这不是伪造
设备请求，也不扩宽任何 scope：清扫只**释放**行，永不派发；设备也无法让 host 引用一个行不携带的版本。

## 3. host → device 消息

| MessageBody | 形状 |
| --- | --- |
| `ProjectList` | `{revision, projects:[{projectId, name}]}` —— **无路径**，随每次 `RequestSnapshot` 下发 |
| `SessionList` | `{revision, sessions:[SessionSummary]}`；`SessionSummary` 增 `projectName?`、`archived` |
| `Transcript` | `{sessionId, messages:[TranscriptMessage]}` |
| `TranscriptMessage` | `{role, text, activities, createdAtMs, turn?}` |
| `TranscriptTurn` | `{turnId, turnCount, canFork, canRewind}` —— 服务端解析，设备**不得**数消息猜 turn |
| `SessionQueue` | `{sessionId, items:[QueueItemEntry]}` |
| `OptionsCatalog` | `{sessionId, models:[…], agentPresets:[…]}` |
| `Resync` | `{sessionId?, reason}` |

`canFork` = 该 turn `provider_turn_started` 且会话 Idle/Failed；`canRewind` = 该消息是 user
且会话 Idle/Failed。host 从本机状态计算，设备不猜资格。

## 4. relay 拒绝帧（relay 代理接入）

`ControlFrame::Rejected { code, message, frame? }`，新增结构化 `frame: Option<RejectedFrame>`
（`deny_unknown_fields`，缺省 `None`，旧 relay 不发也能解析）。

```rust
pub struct RejectedFrame {
    pub host_id: Uuid,
    pub device_id: Option<Uuid>,   // 无法归属时为 None
    pub seq: u64,                  // 被拒帧自己的 relay 可见序号
    pub reason: RejectedFrameReason,
}
pub enum RejectedFrameReason { QueueFull, EndpointOffline, Unlinked, Oversized, Malformed, Other }
```

约束（relay 侧必须遵守）：

1. **不要**只把 `; seq=N` 追加进 `message`。host 不解析 `message` 字符串找目标；
   同一 host 下多 device 各自编号，`seq` 单独不能定位帧。
2. `RelayAck` 只是 relay 收到，**不释放** host 侧 endpoint 恢复记录。
3. 拒绝不是业务结果：host 不会把它当作 `executed`，也不会因它重跑 operation。

host 收到后的行为（已实现）：按 `(host_id, device_id)` 找到该设备通道 → `buffer.release(seq)`
销毁该帧 ciphertext（**不在新 Noise 周期重发**）→ 只对该设备发 `Resync` + 重推
`SessionList`；其他设备通道、序号、缓冲不动，连接不断。若该 seq 已被设备 ack 或属于别的
device 编号，则记录后忽略。

## 5. 权限与拒绝（不变部分，重申）

- `mode` / `runtimeMode` / `workspace` / `binary` / `cwd` 出现在 `applySessionOptions`、
  `archiveSession` payload 中一律 `Malformed` 拒绝，不静默忽略；`createSession` 只允许
  `projectId`（见 §2）。
- `ResponsePayload::Error` 不是成功：所有命令必须匹配权威 response 变体。
- 归档不删文件、不改 provider；忙任务先停再归档并取消其队列；恢复只整理、不自动运行。
  `AgentSession.archived` 带 `skip_serializing_if = "Not::not"`，daemon 的 `SaveTaskState`
  合并不允许旧投影把它抹掉。
- 队列 bound：每 session 16 项 / 64KiB，global 256 项；terminal 项立即清正文。

## 6. 归属

- 我：host 全部模块、`remote-protocol` message/frame、最小 core（daemon/persistence/server）、
  `fintwind-protocol` model/protocol/submission、`db/migrations/0005`。
- 主代理：lib re-export、schema、typegen、部署与验收包。**typegen 新增/变更**：
  `RemoteProvenance{hostId,deviceId,grantVersion}`、`RemotePromptRecord.grantVersion` +
  `cancelOperationId?`/`cancelState?`、`Command::CancelRemotePrompt` 增 `cancelOperationId?`、
  新 `Command::QueryRemoteCancel` 与新 `ResponsePayload::RemoteCancelState`；
  命令字段 `caller` → `provenance`（PWA wire Operation 不变）。
  **另需同步 `db/schema.ts` 与 `bun run db:generate`**：本批手写了
  `db/migrations/0006_remote_cancel_intent.sql`（两列 ALTER），schema 源需对齐以避免下次生成漂移。
- relay 代理：`RejectedFrame` 发送端、QueueFull 真实外进程黑盒。
- PWA 代理：按本文 payload 接入；`api/session.ts:175` 旧注释（"从最新往回数 N 轮"）须删。

## 7. 本批未验收项（代码已编译，未运行）

编译证据：`cargo check --locked --workspace` 与 `--workspace --tests` 均通过（仅既有测试文件
的既存 warning）。以下**均未运行验证**，不得据此宣称验收通过：

1. **真实 daemon + synthetic provider 全链路**：idle 首条真实启动 runtime、busy 普通发送只
   queue、轮 settle、host 重启后 queued 恢复、同 operationId 重放无重复、TTL 队首释放、
   撤销/归档/drop 后不得再派发、双设备审批唯一 driver 决定。
2. **本批新增、尚无运行证据的具体判据**：
   - **TOCTOU 建轮互斥**：本机 GUI 提交与远端派发建轮交错时，会话**不得**出现两个 Running turn；
     busy 时行回到 `queued` 且原 operationId 仍可查询，绝不回 `unknown`。
   - **grant V1 撤销后 V2 重配**：旧行不得被新 grant 派发；同时 host 清扫必须能用行自带版本
     释放它（撤销后 `ListRemotePrompts` 不得再返回该行）。
   - **同 operationId 不同 prompt**：host 层与 daemon 层都必须拒绝，且**不**按第一份正文回答。
   - **archive 与 dispatch 交错**：归档取消等待行后，进行中的派发不得把该 prompt 发给驱动。
   - **旧 dedup 库升级**：用升级前创建的库（无 `content_hash` 列）启动 host → open 不报错、
     既有记录可查询、重复 open 无副作用、并发 open 不撞 duplicate column；旧行上的带 prompt
     重放被拒而不被静默收养。
   - **取消意图**：行 `dispatching` 时取消 → 设备得 `Accepted`；派发应答 `queued` 后同一 tick
     内取消被兑现（prompt 与 cancel 两个 operation 都推到）；派发应答 `dispatched` 时设备被
     告知"已运行"，无任何路径宣称 notSent。
   - **durable 取消绑定**：host 崩溃（行仍 `queued`）后重启 → 该 prompt **不被派发**，取消经
     `QueryRemoteCancel` 解析；旧库升级到 0006 后 `remote_prompts` 有两列、既有行不丢。
   - **同 C 重放幂等 / 异 C 拒绝**：同一取消操作重放不产生第二个绑定；另一设备对同一 prompt 的
     取消被拒。
3. **relay QueueFull 外进程黑盒**（relay 代理在途）。
4. **公网/真实 provider**：`workspace_reads` 30 pass 不证明授权、密文、daemon 适配或公网。
5. 双设备审批唯一 driver 决定仍需真实 provider turn（`multi_device_loop` 无 daemon/provider）。

安全约束（AGENTS 新增）：验收脚本只允许清理**本轮创建并持有句柄**的进程；禁止按名称、端口、
命令行子串或目录匹配杀进程；端口冲突另选随机端口或失败退出。daemon 以 `--parent-pid` 交给
测试进程监护，安装版 Fintwind / daemon / remote-host / OpenCode 服务不得作为清理目标。
