# OpenCode → opencode2 迁移调研

> 调研日期：2026-09-01。本文是迁移前的预研结论，所有标注「需实测」的条目必须在真实
> `opencode2` 上验证后再动手实现。

## 1. 背景

- opencode2 是 [anomalyco/opencode](https://github.com/anomalyco/opencode)（原 sst/opencode）
  的**全重写版本**（Effect 生态、Bun 运行时），CLI 二进制名为 **`opencode2`**
  （`opencode2` 起 TUI、`opencode2 run` 单次执行、`opencode2 mini` 迷你界面）。
- 官方 API 文档：<https://opencode.ai/v2/docs/api>（OpenAPI 3.1，138 个操作、229 个 schema，
  原始 JSON 在 <https://opencode.ai/v2/openapi.json>）。
- 服务端**同时提供两套 API 面**（源码 `packages/opencode/src/server/routes/instance/httpapi/`，
  默认分支 `dev`）：

| API 面 | 前缀 | 生成方式 | 状态 |
|---|---|---|---|
| v1 兼容面 | `/session/...`、`/event`、`/global/health`、`/question/...`、`/global/event` | 手写 HttpApi（`groups/*.ts`） | 保留，部分端点已标记 deprecated（如权限回复） |
| v2 协议面 | `/api/...` | `ServerApi`（`@opencode-ai/protocol/api` 自动生成） | 官方文档面，推荐迁移目标 |

> 关键源码文件：
> - API 组装：`packages/opencode/src/server/routes/instance/httpapi/api.ts`、`public.ts`
> - v1 兼容 session 组：`.../groups/session.ts`（含全部 legacy 路由清单）
> - question 组：`.../groups/question.ts`；workspace 路由：`.../middleware/workspace-routing.ts`、`instance-context.ts`
> - 事件端点：`.../groups/event.ts`、`.../handlers/event.ts`
> - CLI：`packages/opencode/src/cli/cmd/serve.ts`、`packages/opencode/src/cli/network.ts`

## 2. 服务端启动与鉴权（变化较小）

| 项目 | opencode v1（现状） | opencode2 |
|---|---|---|
| 启动命令 | `opencode serve --hostname 127.0.0.1 --port <ephemeral>` | **`opencode2 serve --hostname 127.0.0.1 --port <ephemeral>` 保留**（`cli/network.ts`：默认 `port 0`、`hostname 127.0.0.1`，flag 行为同 v1） |
| 健康检查 | `GET /global/health` | **保留**（`groups/global.ts`，返回 `{healthy, version}`） |
| 鉴权 | `OPENCODE_SERVER_PASSWORD` 空串 = 无鉴权 | 未设置 = 无鉴权（PR #46270 修复为非强制）；**空串是否视为未设置需实测**，保险做法是启动时不再注入该 env |
| workspace 定位 | server 进程与 workspace 绑定（以 cwd 为准） | 无 ambient instance：请求经 `x-opencode-directory` header 或 `?directory=` 路由，**缺省回退 `process.cwd()`**（`workspace-routing.ts` `defaultDirectory`）→ **Fintwind 现有"每 workspace 一个 serve 进程"的 pool 模型依然成立** |

## 3. Fintwind 现有驱动逐调用点映射

改动主体：`crates/fintwind-core/src/driver/opencode.rs`、`opencode_session.rs`、`opencode_pool.rs`。

| Fintwind 现有调用（v1） | v1 兼容面 | v2 协议面（`/api/...`） |
|---|---|---|
| `POST /session`（创建） | ✓ | ✓ `POST /api/session`（另有 `agent`/`model`/`location`/`metadata` 字段） |
| `POST /session/{id}/agent`（plan/build） | **✗ 兼容面无此路由** | ✓ `POST /api/session/{id}/agent` |
| `POST /session/{id}/prompt_async` | ✓（payload 兼容性需实测） | **`POST /api/session/{id}/prompt`**，body `{text, files, agents, skills, metadata, delivery, resume}`，立即返回 inbox item（`Session.Inbox.User`）；**无 `model` 字段**，模型切换走 `/api/session/{id}/model` |
| Steer（busy 会话再发） | v1 隐式 fold | **显式 `delivery: "steer"`**（折叠进当前回合）/<br>`"queue"`（排队）；另有 `GET /api/session/{id}/inbox`、`POST /api/session/{id}/inbox/{id}/steer\|queue` |
| `POST /session/{id}/abort` | ✓ | **`POST /api/session/{id}/interrupt`**（返回 `{interrupted}`，可带 `?continue=true`） |
| `POST /session/{id}/permission/{rid}/reply` `{reply}` | **✗ 变为 `/session/{id}/permissions/{pid}` `{response}`（deprecated）** | ✓ `POST /api/session/{id}/permission/{rid}/reply` `{reply, message}`（与 v1 同形） |
| `POST /question/{rid}/reply` `{answers}` | ✓ 保留同形 | 未列于 v2 OpenAPI 文档但实际提供（实例面） |
| `GET /event`（SSE） | ✓ 保留，**payload 仍为 `{id, type, properties}`**（`handlers/event.ts` 显式把 v2 的 `data` 转回 `properties`，与现有解析完全兼容） | `GET /api/event`（v2 原生 payload `{id, type, data}`，外层 SSE `event:`/`id:` 包装） |
| `GET /api/model`（模型目录/上下文窗口） | 未确认 | ✓ `Model.Info.limit.context` 结构不变 |
| `POST /session/{id}/fork` `{messageID}` | ✓（payload 需核实） | ✓ `{boundary: {type: "before", messageID}}`（显式 before/through；`before` 与 v1「保留到该消息之前」语义等价） |
| `GET /session/{id}/message?limit=20`（usage 回溯） | ✓ | ✓ 分页响应 `{data, cursor}` |
| `DELETE /session/{id}` | ✓ | ✓ `DELETE /api/session/{id}` |

### 新端点可选项（迁移时顺带评估）

- `POST /api/session/{id}/wait` —— 阻塞至 idle（可替代对 `session.idle` 事件的依赖，适合同步操作如 fork 前的等待）。
- `POST /api/session/{id}/compact` —— 压缩。
- `POST /api/session/{id}/generate` —— 一次性生成（可用于标题/摘要）。
- `POST /api/session/{id}/view` —— viewer 标记 idle 已查看（UI 归属元数据，不影响功能）。
- `GET /api/session/active` —— 当前活跃会话（进程内 foreground drains）。
- `GET /api/form/request`、`GET /api/permission/request` —— 轮询待处理表单/权限（备选事件流之外的通道）。

## 4. 事件流差异（关键）

- v1 兼容面 `/event`：**payload 形状与 v1 完全一致**（`{id, type, properties}`），现有
  `handle_event`（按 `type`/`properties` 解析）可直接复用。
- 新增事件：`server.connected`（连接首条）、`server.heartbeat`（**每 10 秒心跳**），
  以及结束时的 `server.instance.disposed`；未知事件名都会被现有 `_ => {}` 分支忽略，无害。
- **事件名映射**（源码 `packages/schema/src/v1/permission.ts`、`v1/question.ts`）：

| v1 事件名 | opencode2 事件名 | 备注 |
|---|---|---|
| `message.part.delta` | 保留 | 结构不变（sessionID/messageID/partID/field/delta） |
| `message.part.updated` | 保留 | `{sessionID, part, time}`，part 结构基本同 v1 |
| `message.updated` | 保留 | **info 结构需实测**（用量/模型追踪依赖 `info.providerID/modelID/tokens`） |
| `session.idle` / `session.status` / `session.error` / `session.updated` / `session.diff` | 保留 | `session.updated.info.title` 标题逻辑兼容 |
| `permission.request` | **`permission.asked`** | `{id: per_..., permission, patterns, always, tool?}` 结构兼容，仅事件名变化 |
| `question.asked` | **`question.v2.asked`** | `{id: que_..., questions: [{header, question, options, multiple}]}` 结构兼容，仅事件名变化；回执事件为 `question.v2.replied` / `question.v2.rejected` |

## 5. 数据与续聊兼容性（最大风险点）

- opencode1/2 **共用同一 SQLite**（`~/.local/share/opencode/opencode.db`，macOS/Linux），
  但 **V2 使用独立的 `session_v2`/`session_message` 表族**。
- 官方 issue [anomalyco/opencode#41217](https://github.com/anomalyco/opencode/issues/41217)：
  V2 不读取 V1 的 `session`/`message`/`part` 表；V1 会话导入（PR #40723）作为应用迁移存在，
  但**当前发布状态未确认**。
- 含义：**Fintwind 持久化的 `ProviderResumeCursor::OpenCode{session_id}`（v1 `ses_` id）迁移后
  大概率无法续聊**。需要沿用既有护栏思路（参照 `docs/remove-non-opencode-providers.md` 的
  「旧会话拒绝启动新回合」模式）：旧会话可查看、不启动新回合、提示用户新建。

## 6. 迁移工作量评估

改动集中在 3 个文件 + 探测：

1. **`crates/fintwind-core/src/driver/opencode.rs`**（约 1785 行）
   - 路由前缀 `/session` → `/api/session`；
   - `prompt_async` → `prompt`（body 重写：`text` + `delivery`；原 prompt 携带的 `model` 移到 `/model` 端点）；
   - `abort` → `interrupt`；
   - 事件名：`permission.request` → `permission.asked`、`question.asked` → `question.v2.asked`；
   - agent 切换改用 `POST /api/session/{id}/agent`（兼容面缺失）；
   - fork body 改 `{boundary: {type: "before", messageID}}`。
2. **`crates/fintwind-core/src/opencode_session.rs`**（约 612 行）
   - fork/消息列表分页响应结构（`{data, cursor}` vs 裸数组）适配。
3. **`crates/fintwind-core/src/opencode_pool.rs`**（约 365 行）
   - 可基本保留（每 workspace 一个 serve 仍成立）；
   - 更优方案是「单全局 server + `x-opencode-directory` header」，属后续优化，不在本次范围内。
4. **探测与启动**（`driver/support.rs` 等）
   - 二进制 `opencode` → `opencode2`；
   - `OPENCODE_SERVER_PASSWORD=""` 的注入处理（见 §2）。
5. **事件解析**：若过渡期继续读 legacy `/event`，`properties` 形状零改动；读 `/api/event` 则仅需把 `properties` 改为 `data`。

### 建议路线（两步落地）

- **第一步（过渡）**：沿用 legacy 兼容面（`/session/...` + `/event`），仅改二进制探测、
  事件名（`permission.asked`/`question.v2.asked`）、权限回复路由（若兼容面行为验证不通过则提前进入第二步）。风险：兼容面部分端点已 deprecated，长期维护价值低。
- **第二步（目标）**：会话操作全部切到 v2 协议面（`/api/*`），事件流切 `/api/event`（`data` 字段）；
  旧 v1 会话按 §5 护栏处理。

## 7. 待实测验证清单（动手前必须对真实 `opencode2` 验证）

1. v1 兼容面 `POST /session/{id}/prompt_async` 的 payload（v1 `parts` 结构）是否兼容 —— 决定过渡方案可行性；
2. `OPENCODE_SERVER_PASSWORD=""` 空串是否触发 basic auth；
3. `message.updated` 事件 `info` 字段形状（用量/模型追踪）；
4. v1 `ses_` 会话 id 能否在 v2 上 `GET /api/session/{id}`（v1→v2 import 是否已生效）；
5. 用户输入实际触发 `question.v2.asked` 还是 form（`frm_`），确认与 Fintwind `UserInputRequested` 的对接方式；
6. v2 `prompt` 不带 `model` 时是否使用会话已设模型（模型切换流程）；
7. 共享 daemon（`opencode2` 默认发现/启动后台服务）与独立 `serve` 并存时，Fintwind 独立 spawn 的 serve 是否会被 daemon 干扰。

## 8. 参考资料

- 官方 API 文档：<https://opencode.ai/v2/docs/api>；OpenAPI：<https://opencode.ai/v2/openapi.json>
- CLI 文档：<https://opencode.ai/v2/docs/cli>（`opencode2`/`run`/`mini`/`--standalone`/`--server`）
- 服务端源码（`dev` 分支）：`packages/opencode/src/server/routes/instance/httpapi/`
- 事件/协议 schema：`packages/schema/src/{event,event-manifest,session-event,session-v1,permission-v1,question,question-v1}.ts`
- 相关 issue：
  - [#41217](https://github.com/anomalyco/opencode/issues/41217) V1 会话历史未导入 session_v2
  - [#46270](https://github.com/anomalyco/opencode/pull/46270) serve 鉴权改为 opt-in

---

# 实测记录（2026-09-01，beta-18743）与第一步实施结果

> 本次迁移按 §6「建议路线」动手。**§7 实测后，第一步（沿用 v1 兼容面）被推翻，
> 直接实施了第二步（v2 协议面）**。以下全部结论来自对真实 `opencode2
> v0.0.0-beta-18743`（npm `@opencode-ai/cli`）的抓包验证，覆盖：无 env / 空
> password env / 固定 password env 三种启动方式、v1/v2 双面路由对照、完整
> prompt 周期事件流、权限轮询、fork 边界语义。

## A. 与 §2/§3/§4 调研结论的差异（均以实测为准）

1. **鉴权（推翻 §2）**：`OPENCODE_SERVER_PASSWORD` **未设置或空串都强制
   Basic auth，且 serve 自动生成随机密码并打印到 stdout**（`server password ...`）。
   无鉴权启动在当前发布版不存在。→ Fintwind 改为**注入随机密码 + 全部请求带
   `Authorization: Basic base64(opencode:<password>)`**（端口→密码注册表，
   SSE 流同样带认证头）。
2. **健康检查（推翻 §2）**：`GET /global/health` 已不存在（未鉴权 401；带认证
   返回 web UI 的 HTML 兜底）。**真实健康检查是 `GET /api/health`** → `{healthy,
   version, pid}`。
3. **v1 兼容面已死（推翻 §1/§3 表格）**：`POST /session` → **405**，`GET /model`
   → HTML 兜底。当前发布版只有 `/api/*` 面可用，§4 的「legacy `/event` 事件
   形状零改动」不成立。
4. **事件模型重构（推翻 §4）**：`/api/event` 的 SSE 行是 `data: {json}`（无
   `event:`/`id:` 包装，心跳是 `: heartbeat` 注释行），payload 字段为 **`data`**
   而非 `properties`。事件名与 v1 完全不同：
   - 文本流：`session.text.started / delta / ended`（`data.delta`）
   - 推理流：`session.reasoning.started / delta / ended`
   - 回合：`session.execution.started / succeeded / failed`（**无 `session.idle`**；
     `failed` 的 `data.error.message` 携带 provider 错误）
   - 用量：`session.step.started`（`data.model`）＋ `session.usage.updated` /
     `session.step.ended`（`data.tokens`，**无 `total` 字段**，`cost` 在旁）
   - 工具：`session.tool.input.started`（`name`）、`session.tool.called`
     （`input`）、`session.tool.progress`、`session.tool.success`（`content`）、
     `session.step.streamed`
   - 标题：`session.renamed`（`data.title`）
   - 其余：`session.inbox.enqueued/delivered`、`session.instructions.updated`、
     `server.connected`、`plugin.added` 等，均非转录内容
5. **权限（推翻 §4 事件名映射）**：权限请求**不进 SSE**，通过
   `GET /api/permission/request` 轮询获得（响应 `{data:[{id: per_..., sessionID,
   action, resources, save, source}]}`）；回复 `POST /api/session/{id}/permission/
   {rid}/reply` body `{reply: "once"|"always"|"reject", message?}`（`once` 实测
   有效；`always` 会写入服务端进程级缓存）。**默认配置（空 opencode.jsonc）下
   任何工具调用都不产生权限请求，全自动执行**；`permission: {"edit":"ask",
   "command":"ask"}` 等配置才会产生 `per_` 请求。→ Fintwind 新增 400ms 权限轮询
   线程，请求形状映射 `action→permission`、`resources→patterns`、`save→always`。
   Fintwind 的 question 回复仍走 `POST /api/question/{rid}/reply {answers}`。
6. **会话/消息（推翻 §3 部分假设）**：
   - `POST /api/session {}` → `{data:{id: ses_...}}`（继续 `ses_` 前缀）
   - `GET /api/session/{id}/message` → **`{data:[...], cursor:{previous,next}}`**
     分页，**最新在前（倒序）**；消息为扁平结构：user
     `{id,time,text,type:"user"}`；assistant `{id,time,type,agent,model:{id,
     providerID},content[],snapshot,finish,rawFinish,cost,tokens:{input,output,
     reasoning,cache}}`；另有 `synthetic`/`agent-switched`/`model-switched` 等
     类型。usage 在消息**顶层**（无 `info`）。
   - `POST /api/session/{id}/fork` 必须 `{boundary:{type:"before"|"through",
     messageID}}`（裸 `{messageID}`/空 body → 400）；`before` = 保留该消息之前
     （**含系统消息**），`through` = 保留到该消息；响应 `{data:{id}}`。
   - `POST /api/session/{id}/model` body `{model:{id, providerID}}` → 204
     （**不是** v1 的 `{providerID, modelID}` —— 首版误写成后者被 400 静默吞掉，
     实测回合落到默认模型才暴露）。
   - `POST /api/session/{id}/interrupt` → `{interrupted}`；`DELETE /api/session/
     {id}` → 204；`POST /api/session/{id}/agent {agent}` → 204；prompt
     `{text}` 默认 `delivery:"steer"`（busy 折叠同 v1）。
7. **环境差异**：Windows 下 npm shim 使 `find_executable` 优先命中
   `opencode2.cmd`（经 cmd.exe 中转），`kill()` 只杀包装进程导致 serve 变孤儿
   → 修复：Windows 上先按 PATHEXT 后缀探测（其后再退裸名），并且 serve 销毁
   用 `taskkill /T /F` 杀进程树（macOS 不受影响）。

## B. 第一步实施的代码改动（全部基于以上实测）

- `crates/fintwind-core/src/opencode_session.rs`
  - 随机密码注入 + Basic 头（`server_passwords()` 端口注册表，
    `basic_authorization(port)` 供 SSE 复用）；健康探测 `/api/health`
  - `native_messages()`：分页（`limit=200` + cursor）拉全量，**用户轮次按正序**
    （wire 倒序需 reverse），`last_id` 取最新一条；fork body 改
    `{boundary: before|through}`；`is_native_user_turn` 改 `type=="user"` 判定
  - Windows 进程树销毁（`taskkill /T` + PATHEXT 优先探测）
- `crates/fintwind-core/src/driver/opencode.rs`
  - 路由全部 `/api/*`：会话创建（`/data/id`）、agent、model 设置
    （`{model:{id,providerID}}`）、prompt `{text}`、interrupt、permission
    reply、question reply
  - `/api/event`（带认证）+ `data` 字段解析；事件名整套重写（§4 映射）：
    `session.text.delta`→TextDelta、`session.reasoning.delta`→ReasoningDelta、
    `session.usage.updated`→UsageUpdated（窗口查 `last_model`，模型来自
    `step.started`）、`session.execution.succeeded/failed`→TurnFinished（failed
    带错误消息）、`session.renamed`→AutoTitleUpdated、tool 事件族→RichActivity
    （`input.started` 记名 + `called` 开始 + `success`/`error` 完成）
  - 新增权限轮询线程（400ms，`GET /api/permission/request`，按 sessionID 过滤 +
    seen 去重，复用 `request_permission`）
  - usage 回溯适配扁平消息（`tokens`/`model` 顶层）
- `crates/fintwind-core/src/{model.rs, command_env.rs, opencode_pool.rs, server.rs}`
  - 二进制探测 `opencode` → **`opencode2`**；PATHEXT 优先（Windows）
- 测试：事件 wire 全部改写为 v2 形状；真实集成测试
  `forks_away_a_real_single_turn_session`（含 `taskkill` 验证）与
  `opencode_session_against_a_real_server`（prompt→事件→usage→fork 全流程）
  在真实 `opencode2` 上实测通过（后者指定 `glmcoding/glm-5.3-flash` 模型）。

## C. 遗留风险与后续

1. **§5 续聊护栏**：Fintwind 持久化的 v1 `ses_` 会话在 v2 表族中不可读 ——
   `ProviderResumeCursor::OpenCode` 落入旧 id 时需沿用「可查看、不启新回合、
   提示新建」护栏（本次未含 v1→v2 import 验证；`GET /api/session/{id}` 对
   v1 会话的返回需在目标环境确认）。
2. **Supervised 模式语义变化**：v2 默认配置不产生权限请求（全自动），
   Fintwind 的 Ask 模式只有用户侧配置 `permission: ask` 时才会出现审批 UI；
   轮询线程已就位，但「默认无审批」是产品行为差异，需产品侧确认。
3. **question 事件名**：~~`question.v2.asked` 未能自然触发验证~~
   **已实测（2026-09-02，beta-18866）**：`question` 工具的提问**不走**
   `question.asked` / `question.v2.asked` 事件，而是走 **form 通道**：
   - 工具侧：`session.tool.input.started`（name=`question`）→
     `session.tool.called`（`input` 为解析后的参数，`executed:false`）→
     **`form.created`**（`data.form = {id: "frm_…", sessionID, title:"Questions",
     metadata:{kind:"question", tool:{messageID, id: callID}},
     fields:[{key:"q0", title:header, description:问题文本, type:"string"|
     "multiselect", options:[{value,label,description}], custom:true}]}`）。
   - 等待答复期间回合保持 running；答复
     `POST /api/session/{sid}/form/{frm}/reply` body
     `{answer:{<key>: <label…>}}`（**multiselect 字段必须是数组**，裸字符串
     返回 400 `FormInvalidAnswerError: Expected string array`）→ 204 →
     `form.replied`（`{id, sessionID, answer}`）→ `session.tool.success` →
     回合正常 `session.execution.succeeded`。
   - `/api/question/*` 路由在该版本 404（连列表端点都没有）；
     `question.asked`/`question.v2.asked` 事件未出现。
   - Fintwind 对策：驱动同时监听 question 事件（旧版本）与 `form.created`
     （当前版本），回复按 `frm_` 前缀路由，form 字段形状记录在
     `OpenCodeFormState` 供组答复使用；另有 `/api/form/request` 轮询兜底
     （与权限轮询同一线程，`announced` 集合去重）。
4. **事件模型差异的产品影响**：无 `session.idle`，回合结算由
   `execution.succeeded/failed` 驱动（已实现）；`session.step.ended` 的
   `finish:"tool-calls"` 等中间态未建模，工具活动在 `tool.success` 完成。
5. **`delivery` 语义**：实测默认 `steer` 与 v1 fold 行为一致，未显式指定；
   若需「排队」语义可改 `{text, delivery:"queue"}`。
6. **共享 daemon 并存**：用户后台 daemon（`serve --service`）与 Fintwind 独立
   serve 实测共库共存无干扰（SQLite WAL）；Fintwind 会话不受 daemon 影响。