# 远控协议说明（v1）

本文定义 Fintwind 远控协议 1。它不是 Alma 的加密协议；中继基于开源项目 Alma
Relay（yetone，MIT），只承载外层隧道帧。桌面接受 `/__fintwind_remote__`
WebSocket 建连，拒绝一切 HTTP 请求与其他 WebSocket 路径。

## 配对与加密

每次开启生成两个独立 32 字节随机值：路由 token 标识隧道，配对密钥认证并加密
手机连接。两者都不是 daemon token 或中继注册密钥。

配对链接形如 `https://fintwind-mobile.roketskiy.top/#v=1&route=TOKEN&key=SECRET`，
值为无填充 base64url。手机加载后清除 fragment，把密钥导入不可导出的 HKDF
CryptoKey；IndexedDB 保留密钥与路由以便同浏览器重开，同源脚本仍可用该密钥，
不可导出不防 XSS。关闭桌面、停止远控或重新配对使旧激活失效；「忘记此电脑」移除
浏览器授权，不影响其他浏览器。

手机经 WSS 打开 `/c/TOKEN/__fintwind_remote__`，发送
`{"type":"hello","version":1,"nonce":"BASE64URL"}`（32 字节 fresh nonce）；桌面回
同样结构与自己的 fresh nonce，每条 WebSocket 都是新 nonce。

用 HKDF-SHA256 派生两个方向各 32 字节密钥：输入为配对密钥，盐为手机 nonce +
桌面 nonce 拼接，info 为 `fintwind.remote.v1.client` / `fintwind.remote.v1.server`。

加密封套为 `{"type":"sealed","seq":0,"data":"BASE64URL"}`，AES-256-GCM，128 位
tag 追加在密文后；12 字节 nonce 为 4 个零字节 + 无符号 64 位大端序号；AAD 为
UTF-8 字符串 `fintwind.remote.v1`。每个方向从 0 开始严格递增；拒绝序号溢出与
超出 JS 安全整数的值。

桌面先发加密 `{"type":"ready"}`，手机解密验证后才发命令，桌面只执行解密成功的
命令。认证或 framing 非法直接关闭虚拟 socket，不泄露细节。本 PSK 协议不提供
前向保密。

## 命令与响应

请求为 `{"id":"UUID","command":{...}}`，命令用 `type` 区分，字段 camelCase。

| 类型 | 字段 |
| --- | --- |
| `listWorkspaces` | 无 |
| `listSessions` | `projectId` |
| `getSession` | `sessionId` |
| `watchSession` | `sessionId`、`watchId`（UUID）、`enabled`（只读，需 `sessionDeltas`） |
| `getComposer` | `sessionId`（只读，需 `composerOptions`） |
| `configureSession` | `sessionId`，可选 `model`、`interactionMode`、`reasoningEffort` |
| `uploadChunk` | `sessionId`、`uploadId`、`name`、`offset`、`dataBase64`、`complete` |
| `createSession` | `projectId` |
| `sendPrompt` | `sessionId`、`text`，可选 `attachmentIds` |
| `stop` | `sessionId` |
| `respondPermission` | `sessionId`、`requestId`、`optionId` |
| `answerQuestions` | `sessionId`、`requestId`、`answers` |

只读请求可带顶层 `ifRevision`（此前响应的 64 位小写 SHA256 修订）。修订检查跑在
授权与时效检查之后；无变化返回 `{notModified:true,revision}`。手机只在收到过修订
后才发条件请求，与旧桌面兼容；其有界内存缓存按配对路由与资源隔离，不缓存变更结果。

响应为 `{"id":"UUID","ok":true,"data":...}` 或
`{"id":"UUID","ok":false,"error":"MESSAGE"}`。变更成功指桌面受理，不等于 Agent
已完成。手机不自动重试变更；变更 ID 在本次激活内保留，去重表拒绝新变更而不驱逐
旧身份。

手机把未完成操作的身份保存在路由隔离的本地存储（无消息体）。超时、断线、刷新或
重开页面后，同会话/工作区的另一变更保持阻塞，直到后续成功读取刷新当前状态。
同浏览器同配对只允许一个活动页面（Web Locks）；不支持锁/存储能力时直接失败。
文本相同的不同用户动作不合并。

## 响应数据

`listWorkspaces` 返回 `{workspaces:[{id,name}],selectedProjectId}`，只含本次激活
范围内仍存在的项目。`listSessions` 返回 72 小时内的会话与 `cutoff`；每次读写重查
项目归属与会话时效。

`getSession` 返回会话投影：有序 `entries` 为准（消息与动态交错，工具只带名称与
状态，不带参数、输出与目标细节；只有推理条目带有限思考文本；上限 200 条与
128 KiB 序列化预算）。超限审批/问卷置 `desktopRequired` 而不展示不完整表单；
无水合历史的 live 会话返回可用消息且 `canSend:false`。通告 `todoList` 的桌面返回
只读计划胶囊（至多 64 项，每项至多 512 UTF-8 字节）；通告 `workspaceChanges`
的返回增删计数（后台 IPC+Git，至多 8 路）。`getComposer` 返回模型、命令与可配置性，
校验走目标工作区目录。通告 `goals` 的会话返回目标（目标/结果各 8 KiB 上限），手机
仅在通告时提供 Goal 操作，走桌面既有 `/goal` 命令路径。

`uploadChunk` 每块至多 65536 个 base64 字符（标准 base64，非尾块无填充）；完整文件
至多 2 MiB 解码；每激活至多 8 个上传、约 8 MiB 解码，共保留 10 分钟。
`sendPrompt.attachmentIds` 只接受本次激活同会话已完成的上传；附件可单独发送。
已受理的发送消费上传 ID；发送与分块都不自动重试。

`permission` / `questions` 为 null 或对应表单；`createSession` 返回 `{sessionId}`，
其余变更返回 `{accepted:true}`。手机按需与任务进行中刷新有界视图；渲染不可信文本
只用 DOM 文本内容，不用 HTML。

### 可选会话更新

`sessionDeltas` 在既有加密连接上订阅更新，握手与 `ready` 不变。`watchSession`
返回 `{accepted:true}`；新订阅替换旧订阅，禁用只影响匹配的 `watchId`，不接受
`ifRevision`。首次推送为带 `revision` 的完整快照，后续为完整快照或
`{baseRevision,revision,edits}` 增量（`set`/`append`/`truncate` 按序执行）。客户端
校验 base 修订与重建后的完整会话；失败重读并重建订阅，再失败回退条件读取。桌面
200ms 合并实体通知；投影有界，哈希/差分/序列化/加密跑在传输线程；无变化不推送。
关闭 socket 或替换/禁用订阅即停。

## 桌面侧边界

传输与密码状态由 `fintwind-remote` crate 内部持有，不对外暴露。对外接口覆盖：
加载默认中继配置、启停远控、取配对链接、上报连接状态、分发桌面请求与会话订阅。
注册使用 `~/.fintwind/remote-control-identity.bin` 中的安装身份：私钥签名
challenge 换短期 JWT，只驻内存；Windows 用当前用户 DPAPI 保护密钥。损坏或吊销
的身份永不自动替换。安装密钥与 JWT 不进配对链接、远控事件或日志。中继部署见
仓库 `deploy/remote-control/` 目录。

桌面回调绑定激活代次，停止后旧激活排队的请求不再执行；每个请求执行前重查 live 授权。
