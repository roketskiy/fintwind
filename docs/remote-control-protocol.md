# 远控协议说明（v2）

本文定义 Fintwind 远控直连协议 2。手机和桌面通过 WebRTC 可靠、有序数据通道交换
业务消息。信令服务只交换建立连接所需的加密信息，STUN 协助发现网络地址。协议不
配置 TURN，也不通过旧中继转发业务数据。

## 配对与加密

每次开启生成两个独立 32 字节随机值：路由 token 用于匹配信令双方，配对密钥认证并
加密手机连接。两者都不是 daemon token。

配对链接形如 `https://fintwind-mobile.roketskiy.top:18761/#v=2&route=TOKEN&key=SECRET`，
值为无填充 base64url。手机加载后清除 fragment，把密钥导入不可导出的 HKDF
CryptoKey；IndexedDB 保留密钥与路由以便同浏览器重开，同源脚本仍可用该密钥，
不可导出不防 XSS。关闭桌面、停止远控或重新配对使旧激活失效；「忘记此电脑」移除
浏览器授权，不影响其他浏览器。

双方通过 WSS `/signal/v2` 按路由 token 的 SHA-256 值匹配。手机为每次连接生成随机
attempt，并用配对密钥加密 offer。桌面解密后返回加密 answer。双方只配置 STUN，
完整的 SDP 和候选信息在加密信令内交换。信令断开不会关闭已经建立的数据通道。
桌面在线时保持信令连接；信令服务按时间限制连接尝试频率，不以固定次数终止配对。
桌面在本次远控开启期间保留已处理的连接标识，拒绝重放。
手机信令连接关闭或被替换时，信令服务丢弃该连接的迟到应答，不影响桌面继续等待新连接。
手机网页短暂切到后台后，回前台先用只读请求验证现有数据通道；通道不可用时才重建。

数据通道建立后，手机发送 `{"type":"hello","version":2,"nonce":"BASE64URL"}`，
桌面返回相同结构与自己的随机 nonce。双方使用新的 nonce 派生数据通道密钥。

用 HKDF-SHA256 派生两个方向各 32 字节密钥：输入为配对密钥，盐为手机 nonce +
桌面 nonce 拼接，info 为 `fintwind.remote.v1.client` / `fintwind.remote.v1.server`。

加密封套为 `{"type":"sealed","seq":0,"data":"BASE64URL"}`，AES-256-GCM，128 位
tag 追加在密文后；12 字节 nonce 为 4 个零字节 + 无符号 64 位大端序号；AAD 为
UTF-8 字符串 `fintwind.remote.v1`。每个方向从 0 开始严格递增；拒绝序号溢出与
超出 JS 安全整数的值。

桌面和手机先在加密消息中确认角色、attempt 和双方 SDP 的 SHA-256 值，然后桌面发送
加密 `ready`。双方在身份确认完成前不执行业务命令。SDP 中的证书指纹由受保护的
信令绑定到本次连接。认证或消息分块非法时关闭数据通道。本配对密钥方案不为信令
内容提供前向保密。

数据通道消息按 UTF-8 字节分块传输。接收端按顺序重组，有大小和时间限制。断线时，
手机保留结果未知的操作并在恢复后读取状态，不自动重发变更。

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
后才发条件请求，其有界内存缓存按配对路由与资源隔离，不缓存变更结果。旧版中继
协议的桌面和网页需要更新后才能使用直连。

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

`sessionDeltas` 在已认证的数据通道上订阅更新。`watchSession`
返回 `{accepted:true}`；新订阅替换旧订阅，禁用只影响匹配的 `watchId`，不接受
`ifRevision`。首次推送为带 `revision` 的完整快照，后续为完整快照或
`{baseRevision,revision,edits}` 增量（`set`/`append`/`truncate` 按序执行）。客户端
校验 base 修订与重建后的完整会话；失败重读并重建订阅，再失败回退条件读取。桌面
200ms 合并实体通知；投影有界，哈希/差分/序列化/加密跑在传输线程；无变化不推送。
关闭数据通道或替换、禁用订阅即停。

## 桌面侧边界

传输与密码状态由 `fintwind-remote` crate 内部持有，不对外暴露。对外接口覆盖启停
远控、取配对链接、上报连接状态、分发桌面请求与会话订阅。信令与 STUN 部署见仓库
`deploy/remote-control/` 目录。

桌面回调绑定激活代次，停止后旧激活排队的请求不再执行；每个请求执行前重查 live 授权。
