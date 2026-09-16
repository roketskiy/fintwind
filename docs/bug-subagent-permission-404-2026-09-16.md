# Bug 调查报告：子代理区外路径权限回复 404，工具静默挂死

日期：2026-09-16　|　现场：OpenCode 2.0.3 + Fintwind 驱动　|　状态：已定位根因，未修复

## 现象

父会话派 `code-reviewer` 子代理。子代理对工作区外路径发 `grep` / `glob`（例如 `C:\Users\潘雷\.cargo\git\checkouts`）后，两个工具调用一直停在 `running`，直到用户手动停止。一次现场挂了 **19 分 54 秒**。

同一父会话随后再派一个全程待在工作区内的审查子代理，454 秒正常结束。问题只在「子代理碰到权限闸门」时出现。

Full Access 也救不了：自动 `once` 照样打错 URL，用户连授权弹窗都看不到。

## 结论（TL;DR）

OpenCode v2 的权限/表单按**会话**归属。Fintwind 驱动仍按 v1 假设「request id 全局，父会话管道能直接答」，把子会话的回复打到父会话路径上，服务端 404，授权永远落不下去，工具一直等。

三条路同时断了：

| 环节 | 现在怎么做 | 实际结果 |
| --- | --- | --- |
| 回复 URL | 永远用 `worker_session`（父会话） | 子请求 404 |
| 轮询兜底 | `sessionID == 自己` 才处理 | 子请求被跳过 |
| 失败恢复 | 404 后只从 `permission_seen` 删掉，不换 session 重试 | 静默挂死 |

这不是 OpenCode 把工具卡死。服务端按会话隔离是对的；坏的是 Fintwind 的回复路由。`question` 表单走 `/form/{id}/reply`，是同一类坑。

## 现场时间线

会话：`ses_f5668644…` 是 `ses_f56707e6…`（会话卡片改 Logo）派出的 `code-reviewer` 子代理。

| 时间 | 事件 |
| --- | --- |
| 09:47:05.26 | 子代理最后一步发出 `grep` + `glob`，路径在工作区外，要 `external_directory` 授权 |
| 09:47:05.279 / .281 | `opencode.log` 两条 `POST /api/session/{父会话}/permission/per_xxx/reply` → **404**（工具启动后 16ms，对上这一对调用） |
| 09:47:05.26 – 10:06:58.83 | 两个调用一直 `running`，**19 分 54 秒** |
| 10:06:58.996 | 用户按停止，父+子同时 `idle_outcome: interrupted`，工具以 `Tool execution interrupted` 结束 |
| 10:07:32 | 父会话再派审查子代理，全程待在工作区内，454 秒成功 |

16ms 内 404 不可能是人点的。这是事件流上的自动批准（Full Access / 已记住的规则）打到了错误 URL。

## 触发条件

OpenCode v2 默认策略（后匹配获胜）：

```text
{ action: "*",                  resource: "*", effect: "allow" }
{ action: "external_directory", resource: "*", effect: "ask"  }
```

工作区外路径会先要 `external_directory` 批准，再谈 `read` / `grep` / `glob`。子代理只要碰到 Location 之外的目录（cargo git checkout、家目录、别的仓库），就会在子会话上挂起一条权限请求。

父会话自己碰区外路径时，回复 URL 碰巧是对的，所以主会话看起来正常——缺陷被「只有子代理会踩」遮住了。

## 代码链路

文件：`crates/fintwind-core/src/driver/opencode.rs`

| 位置 | 作用 | 问题 |
| --- | --- | --- |
| `:54–62` `CommandMessage::Respond` | 回复只带 `request_id` + `option_id` | 归属 `session_id` 在进 worker 前已经丢了 |
| `:364–367` 权限轮询 | `GET /api/permission/request` 后只留 `sessionID == 父会话` | 子请求被跳过；共享 server 上别人的会话也不能误收，但自己的孩子被误杀 |
| `:407–409` 表单轮询 | 同样按父会话过滤 | 子代理 `question` 同样进不来 |
| `:682–690` 权限回复 | `POST /api/session/{worker_session}/permission/{id}/reply` | `worker_session` 是父会话 |
| `:716–720` 表单回复 | `POST /api/session/{worker_session}/form/{id}/reply` | 同一错误 |
| `:691–697` 404 处理 | 从 `permission_seen` 删掉，发一条 Error | 不按真实 session 重试；轮询又捞不到孩子 → 死锁 |
| `:2084–2089` 子事件透传 | 把 `permission.*` 交给 `request_permission` | 弹窗/自动批准能发生，但回复仍走父会话 |
| `:458–475` 事件分流 | 未知 `sessionID` 且不在 `state.children` 里 → 当别人的会话丢掉 | `permission.requested` 若早于 `session.created`，连弹窗都没有 |
| `:2994–3043` `request_permission` | 抽出 `per_` id，Full Access 立即 `Respond { once }` | 不记录 `sessionID` |
| `:4156–4214` 测试 | `child_permission_requests_surface_like_foreground_ones` | 只断言弹窗和自动批准的 `request_id`，**不断言回复 URL** |

OpenCode 侧契约（v2 HTTP API，行为符合文档）：

| 路由 | 语义 |
| --- | --- |
| `GET /api/permission/request` | 全局待处理列表 |
| `GET /api/session/{sessionID}/permission` | 某会话的待处理请求 |
| `POST /api/session/{sessionID}/permission/{requestID}/reply` | **只响应该会话拥有的请求** |
| `GET /api/form` | 全局待处理表单 |
| `POST /api/session/{sessionID}/form/{formID}/reply` | 同样按会话归属 |

事件流注释（`:2084–2087`）写着「request id 全局，父会话回复管道能直接答」。这和当前服务端行为已经不符。

## 死锁怎么形成

```
子会话工具碰到区外路径
  └─ OpenCode 在 ses_child 上挂起 permission.requested
       ├─ 事件流（孩子已登记）
       │    └─ request_permission → 自动 once / 或弹出授权
       │         └─ CommandMessage::Respond { request_id }   ← 没有 session_id
       │              └─ POST /api/session/{父}/permission/{id}/reply → 404
       │                   └─ permission_seen.remove(id)
       │                        └─ 工具仍在 ses_child 等待授权
       ├─ 事件流（孩子尚未 session.created）
       │    └─ 当成别人的会话 continue                    ← 用户什么都看不到
       └─ 轮询 GET /api/permission/request
            └─ sessionID != 父 → continue                 ← 兜底也被关掉
```

404 之后没有第二条活路。工具一直 `running`，直到用户停掉父/子会话。

## 修复方案：按会话归属的权限路由器

原则：

> 权限/表单的归属 session 以服务端为准；回复必须打到拥有该 request 的 session，而不是永远打到父会话。

不要改 OpenCode API（按会话隔离是对的），也不要让父会话“代答”。客户端对齐契约。

### 1. 数据面：`request_id → session_id`

- `OpenCodePermissionRequest` 和 form 状态记下 `session_id`
- `CommandMessage::Respond` / `RespondUserInput` 带上这个 id；缺省才回退到父会话
- 事件流和轮询都从 payload 的 `sessionID` 取值

### 2. 发现面：子孙会话的请求也要进来

两路都只认「本会话族」，不能把共享 server 上别人的 pending 弹到当前 UI。

**本会话族** = 父会话 ∪ 已在 `state.children` 的孩子 ∪ 探测后 `parentID` 指向本会话（或本会话的孩子）的 session。未知 id 时 `GET /api/session/{id}` 一次，结果缓存。

事件流：`permission.requested` / `form.created` 若早于 `session.created`，探测归属后再接纳，不要当别人的会话丢掉。

轮询：`GET /api/permission/request` 和 `GET /api/form` 用同一套归属判断。事件线程和轮询线程共享 `Arc<Mutex<HashSet<String>>>` 子会话集合。

### 3. 回复面：正确 URL + 404 二次解析

快路径：

```http
POST /api/session/{归属session}/permission/{requestID}/reply
POST /api/session/{归属session}/form/{formID}/reply
```

404 时不要只打日志：

1. 再 `GET /api/permission/request`（或 `/api/form`），用 id 反查真正的 `sessionID`
2. 换对的 URL 重试一次
3. 仍没有 → 立刻 `DriverEvent::Error`，不要让工具继续 `running`

有缓存用缓存，404 再查一次。`GET /api/permission/request` 是全局源，适合做权威查找。

### 4. 安全网：禁止再静默挂 20 分钟

OpenCode 侧权限可以一直等。客户端必须自己收口：

- 权限/表单超过 N 秒未决 → 自动 `reject`，UI 写明「子代理在等区外目录授权，回复失败/超时」
- 标题带上子代理名和路径，例如：`code-reviewer 请求读取 ~/.cargo/git/checkouts`
- Full Access 自动批准失败时，降级成可见错误，而不是当成功

这是兜底。路由修对之后应该很少走到。

## 建议改动点

全部在 `crates/fintwind-core/src/driver/opencode.rs`：

1. `CommandMessage::Respond` / `RespondUserInput` 增加 `session_id`
2. `request_permission` 从 payload 抽出 `sessionID` 并写入 pending
3. worker 回复改用该 `session_id`，不再写死 `worker_session`
4. 轮询过滤改为「本会话族」；未知 session 探测 `parentID`
5. 404 → 全局列表反查 → 重试一次 → 失败则 `DriverEvent::Error`
6. 表单回复同一套
7. 事件流对未知子 session 的权限/表单做归属探测

## 测试（现有覆盖不够）

`child_permission_requests_surface_like_foreground_ones` 只证明弹窗能出来，必须补：

- 子权限回复路径含 `ses_child`，不含父 id
- 轮询能捞到子请求
- 权限早于 `session.created` 仍能露出
- 404 后用正确 session 重试
- 子会话 form 回复打到子 session
- 共享 server 上别人的 pending 不会弹到当前会话

## 不要做的

- 不要让 OpenCode 父会话代答子权限。API 按会话隔离是对的，客户端跟契约走。
- 不要把轮询改成「全局所有 pending 都弹」。共享 server 会串到别人的会话。
- 不要只改 URL、不改轮询。事件若丢了，子请求还是没人接。
- 不要只改权限、不改 form。`question` 工具是同一条死路。
- 不要指望 Full Access。自动批准走同一条错误 URL。

## 今天先止血（不改代码）

已经挂住的会话：停掉子代理。没有别的恢复按钮。

能立刻避开的，是别让闸门抬起来——工作区外路径预授权：

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "permissions": [
    {
      "action": "external_directory",
      "resource": "C:/Users/潘雷/.cargo/git/checkouts/*",
      "effect": "allow"
    }
  ]
}
```

这只挡住「cargo checkout」这一类。子代理提问、别的区外路径照样会挂。只能当临时措施。

## 可选：OpenCode 侧硬化（非必须）

Fintwind 修对即可闭环。若要给 OpenCode 提改进，优先级低于客户端修复：

- 权限长时间未回复时，服务端超时失败，而不是工具永远 `running`
- 对「session 下找不到该 request」的 404 给出更明确的错误体（例如指出请求实际归属的 session）

不要因此加「父会话代答」API。那会把会话隔离重新打穿。
