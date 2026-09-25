# opencode2 v2.0.11 → v2.0.16 跟进调研：适配清单与功能机会

> 调研日期：2026-09-25。前置文档：[opencode2-migration.md](opencode2-migration.md)
>（迁移预研与实测记录）。本文回答两个问题：opencode2 更新到 v2.0.16 后，
> fintwind **必须/建议适配什么**；以及**可以依此发展什么功能**。
>
> 所有标注「需实测」的条目，动手前必须在真实 `opencode2` 上按迁移文档 §7 的
> 抓包套路验证；改动完成后须在重新编译的应用上验证真实 provider 交互。

## 0. 证据来源

| 来源 | 内容 |
|---|---|
| GitHub tag 对比（`anomalyco/opencode`，compare API） | v2.0.11→v2.0.16 五段区间共 176 个 commit（含机器人提交） |
| v2.0.16 官方 OpenAPI（`packages/protocol/openapi.json`） | 113 条 HTTP 路径的最终契约，含请求体字段级校验（`additionalProperties: false`） |
| 仓库内 `V2_HTTP_API_AUDIT.md`（v2.0.16 tag，2026-09-13 再生成） | 全部端点的 Keep/Change/Remove 处置与理由 |
| fintwind 现码 | `crates/fintwind-core/src/driver/opencode.rs`、`opencode_session.rs`、`src/app/*`、本文前置文档的实测记录 |

版本时间线（发布节奏约一天一个）：v2.0.12 (09-21) → v2.0.13 (09-22) →
v2.0.14 (09-22，热修) → v2.0.15 (09-23) → v2.0.16 (09-24，最新 tag)。

---

## 1. 必须适配（P0：契约已变，现网功能静默失效）

> 2026-09-25 已落地，并保留旧 CLI 回退。实测结论见 §5。下面的「现状」
> 是改动前的代码，用来说明为什么必须改。

OpenAPI 的请求体均为 `additionalProperties: false`，旧字段会被服务端直接拒绝，
不是告警。

### 1.1 权限回复字段 `reply` → `decision`（审计 #100）

- v2.0.16 契约：`POST /api/session/{id}/permission/{rid}/reply` body 为
  `{decision, message?}`，`decision` 必填。
- fintwind 现状：`driver/opencode.rs:1027-1028` 发 `{"reply": option_id}` 与
  `{"reply": "reject"}`。
- 影响：**Ask 模式的 once/always 审批整体失效**。迁移文档实测时
  （2026-09-01，beta-18743）`reply` 尚有效，此后契约改名。
- 对策：改发 `decision`。如需兼容旧 CLI，可按 400 回退重试 `reply`。
  需实测：确认 `reply` 形状从哪个版本开始被拒。

### 1.2 fork body 简化：`{before}`，`through` 语义移除（审计 #053）

- v2.0.16 契约：`POST /api/session/{id}/fork` body 为 `{before: "msg_…"}`；
  **省略 `before` = 复制全部历史**。旧的
  `{boundary: {type: "before"|"through", messageID}}` 不在契约内。
- fintwind 现状：`opencode_session.rs:325-329` 按两种 boundary 构造 body；
  keep-all 分支用 `through` 最新消息。
- 对策：「保留全部轮次」改为**省略 body**（新语义即复制全史）；截断点仍用
  `{before}`。`revert/stage`、`revert/commit` 不受影响（均 Keep）；
  新增 `DELETE /api/session/{id}/revert`（清除已暂存边界，审计 #073）可顺带接入。
- 需实测：服务端对旧 `boundary` 形状是 400 还是静默兼容；`before` 指向
  最新一条消息与省略 body 的结果差异。

### 1.3 表单轮询路径 `/api/form/request` → `GET /api/form`（审计 #087）

- 旧路径在审计中处置为 Remove（"冗余 request 命名空间"）。
- fintwind 现状：`driver/opencode.rs:809` 与 `:1074` 的兜底轮询打旧路径。
- 影响：question 表单在事件流之外的兜底通道失效。
- 对策：轮询路径切换；可参照健康检查的三代探测链
  （`opencode_session.rs:35` 的 `/api/info`→`/api/status`→`/api/health`）
  做新旧双探测。需实测：旧路径在 v2.0.16 上是 404 还是重定向。

### 1.4 已跟上、无需动作（备忘）

- 健康检查三代变化 `/api/health`(≤2.0.4) → `/api/status`(2.0.5) →
  `/api/info`(2.0.6+)：探测链已就位（`opencode_session.rs:33-35`）。

---

## 2. 建议适配（P1：行为与数据形状变化）

| # | 变化 | 证据 | fintwind 影响 |
|---|---|---|---|
| 4 | **prompt body 大幅扩展**：`{text, files, agents, skills, metadata, delivery, resume}` | openapi `session.prompt` | fintwind 只发 `{text}`，附件走自有 daemon 通道。至少把 `metadata`（fintwind 任务 ID、来源）写入 prompt / session create，打通「任务 ↔ opencode 会话」双向追溯；`delivery` 可显式化 steer/queue（迁移遗留 C5） |
| 5 | **interrupt 的 `continue` → `resume`** | 审计 #067 | fintwind 的停止调用不带该参数，保持原样即可 |
| 6 | **Console-managed policies**（v2.0.13，PR #49729） | 上游 PR | 禁用的 provider 会从模型目录消失；旧目录缓存不得让被禁用模型重新可选 |
| 7 | **MCP Code Mode 默认值调整**（#51029）+ **MCP 资源成为 codemode 工具**（#50680） | 上游 PR | #51029 仅针对 PostHog / Executor 做默认值适配；#50195 是内部变量重命名，不新增工具事件字段 |
| 8 | **MCP OAuth 不再强制 consent**（#50519） | 上游 PR | 服务端的 OAuth URL 生成修复，客户端继续按授权 URL 登录 |
| 9 | **read 工具长度/截断处理**（#51011） | 上游 PR | 越界错误附文件行数，超额 limit 截为 2000 行，长行截为 2000 字符；没有新增结构化上报字段 |
| 10 | **`session.list` 增强**：`search` / `parentID` / `project` / `subpath` / `cursor` / `order` | openapi `session.list` | 主会话对账早已游标分页；可在服务端过滤根会话，并修复子会话刷新只取首 200 条的问题 |
| 11 | **WebSocket idle timeout 放宽并遵循 chunkTimeout**（#50914） | 上游 PR | 仅影响服务端到模型提供方的 Responses WebSocket，**不影响**客户端 `/api/event` SSE |

### 2.1 修正一项上轮调研的推测

媒体生成（image / speech / transcription / video，v2.0.14–16 的主线工作）**不在
HTTP API 面**：v2.0.16 的 113 条 openapi 路径中没有任何 media 路由。它是 ai 包
内部能力（模型与工具内部消费），fintwind 无法作为客户端直接调用，只能在工具
活动与转录内容中渲染其产物。

---

## 3. 可依此发展的功能

按「离现有架构的距离」分三档。端点均在 v2.0.16 openapi 中确认存在。

### 3.1 近期（1–2 个迭代）

- **已有本地排队 UI；可选对接服务端 inbox** —— busy 时 Enter 会将消息
  加入本地 `queued_messages`，面板支持查看、编辑、steer、取消，当前轮次
  结束后自动发送下一条；`SteerRejected` 也会把消息退回队列。
  `GET /api/session/{id}/inbox`、`PATCH inbox/{inboxID}`（转换
  `delivery: steer|queue`）、`DELETE inbox/{inboxID}`（取消）只在需要
  跨客户端共享/恢复待发送队列时才有增量价值。对接前须先验证
  `session.inbox.enqueued/delivered` 的实际载荷与避免双重投递的方案。
- **已有会话级权限规则；可选补齐管理和反馈** —— Fintwind 已按
  Ask / Auto-accept / Full access 模式通过
  `PATCH /api/session/{id}` 写入服务端 `permissions`，不是仅由 UI
  自行约定。当前写入错误被忽略，也未消费 `session.permissions` 事件；
  后续可明确处理写入失败/跨客户端变更，并用
  `GET /api/permission/saved` + `DELETE /api/permission/saved/{id}`
  做「已放行规则管理」页。Console/配置策略的硬性拒绝仍优先于这些规则。
- **usage 统计换官方源** —— `GET /api/experimental/session/stats`
  （`from/to/project/timezone/tools=none|summary|detail`，#049）。
  `usage_page` 的热力图 / 日柱 / 模型排行不再本地全量解析历史，
  省掉子进程与文件遍历（符合 AGENTS.md 性能原则）。
- **后台化工具** —— `POST /api/session/{id}/background`（#058）：
  前台长工具转后台观察，配合多会话视图。
- **revert 清除** —— `DELETE /api/session/{id}/revert`（#073）补齐
  revert 流程的「取消暂存」操作。

### 3.2 中期（需要新页面或新流程）

- **turn 级 diff 与上下文投影** —— `GET /api/session/{id}/diff`
  （turn 范围结构化 diff，#076）+ `GET /api/session/{id}/context`
  （活动模型上下文投影，#075）。right_panel 的 Review 视图直接消费服务端
  结构化 diff；`/context` 做「上下文占用」可视化。
- **会话导入/导出** —— `POST /api/experimental/session/import` /
  `GET /api/experimental/session/{id}/export`（#070/#071，export 带
  `sanitize` 参数）。「本地优先」叙事下的会话备份、跨机迁移、脱敏分享。
- **无状态生成** —— `POST /api/experimental/generate`（#138）与
  `POST /api/session/{id}/generate`（#066）：自动标题/摘要不再由 fintwind
  拼 provider。
- **slash 命令走服务端** —— `POST /api/session/{id}/command`（#060，
  请求字段已改名 `name`）+ `GET /api/command`（#018）。composer 的本地
  命令索引升级为服务端命令，远程场景行为一致。
- **集成/凭据中心** —— `GET /api/integration`（#025）+
  `connect/key|oauth|command` 三种连接流（#028–#035）+
  `credential PATCH/DELETE/activate`（#042–#044）。
  `custom_providers.rs`（约 34KB）可渐进迁移到服务端集成目录；
  配合 v2.0.13–16 的浏览器登录（#50267）与 device flow（#48501）接
  Console/Go。
- **MCP 运行时控制** —— `PUT/DELETE /api/experimental/mcp/{server}`、
  `connect/disconnect`（#037–#040）+ `GET /api/mcp/resource`（#041）。
  mcp_page / market 从「改配置 + 重启」升级为运行时启停与资源浏览。
- **配置热重载** —— `POST /api/location/reload`：重载全部 location，
  发 `location.shutdown` 事件供客户端恢复。providers / MCP 保存后不再
  重启 serve 进程。
- **V1 迁移护栏闭环** —— `GET /api/experimental/migration/v1`（#143）+
  import（#070）+ v2.0.16 的「迁移时提示 legacy 工具重命名」（#50188）。
  迁移遗留 C1 的「旧会话只读」可升级为「一键导入 V1 会话」；工具卡片需做
  同名映射显示。

### 3.3 远期（结构性机会，建议单独立项）

- **远程工作区桥** —— opencode2 原生已有：
  - 文件：`GET /api/fs/read/*`、`/api/fs/list`、`/api/fs/find`（#102–#104，
    另有 experimental `fs/write`）；
  - worktree：`GET/POST/DELETE /api/worktree` + `POST /api/worktree/refresh`
    （#105–#108）；
  - VCS：`GET /api/vcs|vcs/base|vcs/status|vcs/branch|vcs/diff`（#109–#113）；
  - 终端：`/api/pty` 全套 CRUD + `connect-token`/`connect`（#114–#120）、
    persistent PTY 全套（experimental，#121–#131）、`/api/shell` 全套
    （#132–#137）。

  README 明说 fintwind 的 folder picker 与 PTY「等协议有 daemon-host
  端点再做」——协议现在有了。fintwind-daemon 可把 daemon 机上的 opencode2
  作为远程文件/终端/VCS 提供方，解锁「桌面在 A 机、工作区在 B 机」拓扑。

---

## 4. 建议落地顺序

1. **P0 三项**（§1.1–1.3）：改动集中在 `driver/opencode.rs` 与
   `opencode_session.rs`；对 2.0.11–2.0.16 做一次兼容矩阵实测。
2. **权限写入失败反馈 + permission.saved 管理**（§3.1）：已有会话级
   `PATCH permissions`，补齐可靠性与已放行规则管理。
3. **评估本地队列与服务端 inbox 的同步价值；session.list 服务端过滤/
   分页**（§3.1 / §2#10）：本地队列 UI 已具备，无须重复开发。
4. **stats 端点替换本地聚合**（§3.1）：纯性能收益。
5. **integration/credential + MCP 运行时控制**（§3.2）。
6. **远程桥（fs/pty/vcs/shell）**（§3.3）：立项评估。

## 5. 实测验证清单

> P0 三项已于 2026-09-25 在本机隔离环境实测（`opencode.exe` v2.0.11 与
> v2.0.16，`XDG_*` / `OPENCODE_DB` 指向临时目录，不碰用户数据库）。
> 结论已落进驱动；下列 1–3 不再是动手前的未知项。

1. `permission reply`：2.0.11 与 2.0.16 **都已拒绝** `{"reply":"once"}`
   （400 `Missing key` at `decision`）。`{"decision":"once"}` 通过校验
   （无待处理请求时 404 `PermissionNotFoundError`）。取值仍是
   `once` / `always` / `reject`。因此当前契约优先发 `decision`，仅在 400
   时回退 `reply`，以覆盖更早的 CLI。
2. `fork`：两版都接受 `{before:"msg_…"}` 与 `{}`（省略 `before` = 复制全史）。
   **省略 HTTP body** 是 400 `Expected object`，不能真的不发 body。
   旧 `{boundary}` 在 2.0.16 **不是 400，而是被静默忽略并复制全部历史**
   ——截断必须发 `{before}`，不能再发 `boundary`。`before` 指向最新一条
   消息会丢掉该消息；全量复制必须用 `{}`，不能用 `before: 最新消息`。
   `boundary` 只作为「新字段被拒」时的回退，业务性 400（如 `empty_session`）
   不回退，否则会在新服务上把截断变成全量复制。
3. `GET /api/form`：两版都是 200，形状 `{location, data:[]}`，与旧解析
   （读 `data` 数组）兼容。`GET /api/form/request` 两版都是 404。轮询先打
   `/api/form`，404 再打旧路径，并按端口记住成功的那条，避免每拍 404。
   MCP sentinel 仍只在会话级 `GET /api/session/{id}/form`（审计 #088），
   全局列表的 `data` 形状未变，现有按 `sessionID` 过滤的解析不用改。
   `revert/stage` 与 `revert/commit` 契约未变；`DELETE /api/session/{id}/revert`
   清除暂存已在现码中。

以下为另行规划的功能/实测项（与本次 #5–#11 的兼容复核不同）：

4. `session.prompt`：`metadata` / `delivery:"queue"` / `resume` 的实际行为；
   `session.inbox.*` 事件与 inbox 端点的联动。
5. `PATCH /api/session/{id}` 的 `permissions`：写入后对运行中会话的生效时机，
   以及 `session.permissions` 事件的 payload。
6. `experimental/session/stats`：聚合口径与 fintwind 本地统计的差异。
7. SSE 长连接：服务端真正断流后的恢复策略需单独验证；#50914
   针对的是提供方 WebSocket，与 SSE 无关。

### 5.1 P1 #5–#11 复核（2026-09-26）

- **#5 不改**：`opencode.rs` 前台 Cancel 和子代理 Stop 均 POST `/interrupt`
  且不带查询参数；新版 `resume` 是可选项。不用替换不存在的 `continue`。
- **#6 已修**：PR #49729 表明受策略禁止的提供方不再出现在模型列表。
  `model_catalog.rs` 已过滤 `enabled:false`，但此前把**成功的空目录**当成
  查询失败，回退到旧缓存；现在区分成功空目录与失败。空目录写入缓存文件，
  启动时仍按原有规则不读取空缓存，以免未连接服务被当作可用目录。
  `providers_page.rs` 对已安装但无可用模型显示连接/Console 策略提示。
  老版本 API 请求失败时仍可退回 CLI 目录与最后成功缓存。隔离的
  v2.0.16 `serve` 实测对照：不带策略的目录有 9 个模型，
  `provider.use deny *` 的目录 `/api/model` 返回 0 个模型。
- **#7 暂不改**：#51029 并非全局首次启用 Code Mode；它对已开启的
  Code Mode 调整 PostHog/Executor 的默认配置。#50195 仅将插件内部参数
  `tool` 改名 `context`，不改变事件 payload。现有 `execute` 卡片已能从
  `toolCalls` metadata 提取嵌套工具名，未知工具保留其完整名称；没有可靠的
  新标题字段可接入。MCP 资源调用在真实载荷出现后再验证标题细化。
- **#8 不改**：#50519 在 OpenCode 的 MCP OAuth 客户端去掉 URL 上自动加的
  `prompt=consent`，并保留 `offline_access`。fintwind 由 daemon 执行
  `opencode mcp auth`、打开 CLI 给出的 URL，未自行添加 consent 参数。
- **#9 不改**：#51011 只改 read 的结果文字、越界报错和长行截断，不提供
  机器可读的读取行数/截断元数据。卡片已有输出展示与本地长度限制，不能
  从这次变更推断出一个稳定的“读了多少”字段。
- **#10 已修**：主会话列表在 `driver/native.rs` 原本就带 cursor 翻页并
  本地剔除子会话；现优先请求 `parentID=null`，旧版拒绝查询时退回原路径，
  旧版忽略参数时仍本地过滤。子代理后台刷新之前只取前 200 条，现按
  cursor 翻页并优先服务端按 `parentID` 过滤；失败、满页无游标或超过分页上限时不把
  不完整结果作为“子代理已消失”的证据。隔离 v2.0.16 实测
  `limit=1&parentID=null` 的三页可找全三个根会话、按 parentID
  查询不返回根会话；复现脚本与结果位于 gitignored 的
  `temp/p1-session-list-smoke.ps1` / `.json`。
- **#11 不改**：#50914 修改的是服务端模型 WebSocket 默认 5→30 分钟和
  provider `chunkTimeout`，与 Fintwind 的 SSE 读循环不是一条连接；不应
  因此修改 SSE 重连策略。服务端断流的独立恢复问题须另立项验证。

上游证据：`https://github.com/anomalyco/opencode/pull/49729`、
`/pull/51029`、`/pull/50680`、`/pull/50195`、`/pull/50519`、
`/pull/51011`、`/pull/50914`，以及 v2.0.16 OpenAPI `session.list`、
`session.interrupt`、`SessionsResponse`。

## 6. 参考

- tag 对比：`https://api.github.com/repos/anomalyco/opencode/compare/v2.0.11...v2.0.16`（逐段）
- v2.0.16 OpenAPI：`https://raw.githubusercontent.com/anomalyco/opencode/v2.0.16/packages/protocol/openapi.json`
- API 审计：`https://github.com/anomalyco/opencode/blob/v2.0.16/V2_HTTP_API_AUDIT.md`
- 前置迁移文档：[opencode2-migration.md](opencode2-migration.md)
