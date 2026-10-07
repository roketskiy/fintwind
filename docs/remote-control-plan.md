# 移动端远控实现规划（第一阶段）

本文是 [remote-control-relay.md](remote-control-relay.md) 的实现规划。架构、协议
语义、威胁模型与验收要求以那份文档为准；本文回答**按什么顺序做、每步产出什么、
代码落在哪里、参考实现借什么**。两份文档冲突时，以架构文档为准。

## 已定决策

| 项 | 决策 | 说明 |
| --- | --- | --- |
| 推进顺序 | 按架构文档推进路线全量推进 | relay → host → 配对 → PWA → 只读工作区 → 验收 |
| PWA 技术栈 | React + Vite + TypeScript | 与 ZCode 手机端同栈，参考代码可直接对照 |
| 端到端加密 | Noise Protocol Framework（Rust `snow`） | 响应架构文档未决问题 1；细节见下文，M0 冻结 |

## 参考实现：ZCode 的取与舍

ZCode Web Remote Control 是本次实现的主要工程参考。ZCode 开源
（`github.com/zai-org/ZCode`，TS/React monorepo），以下为调研快照
（源码经 DeepWiki 索引核对，产品行为对照官方文档 `zcode.z.ai/cn/docs/remote-control`），
不作为其安全结论的证明。

### 借

| ZCode 做法 | 落到 Fintwind |
| --- | --- |
| 桌面无监听，作为出站 ws 客户端连 relay；手机浏览器加载同一 React 应用连 relay | 拓扑与架构文档一致，照此实现；PWA 独立成应用但行为对照其移动端适配 |
| `PersistentProtocol`（`packages/rpc/src/persistent-protocol.ts`）：单调 `outgoingMsgId`、累积 ack、未 ack 消息按原序重放、`ACK_TIMEOUT` 20s 判定半开连接、`replayBufferGraceMs` 45s 弃置会话、keepAlive 定时 | host 侧 `BoundedOutboundBuffer` 与心跳超时的具体算法照此设计；Fintwind 已有 `ReplayCursor`/`SequencedEvent` 承担序号语义，缺的正是"已发未 ack"出站缓冲与 ack 超时 |
| `RECONNECT_DELAYS` 退避数组，超限判定永久断开 | host 与 PWA 的重连策略直接采用数组退避，不写指数公式的边界 bug |
| host capability 一次性凭据：先取一次性 token，ws 握手 header 消费，过期即拒 | host enroll 换独立连接凭据的实现形态参照；凭据一次性消费语义与配对 nonce 原子消费同构 |
| 一次性连接 ID 取出即删（`remoteConnections` map）实现单连接限制 | 借"取出即消费"的原子模式；Fintwind 不做单手机限制，用于 nonce 与配对请求消费 |
| clientMode 角色区分（`desktop-continuous` vs `web-remote-replayable`，后者不能触发本地副作用） | 连接角色固定：host 连接与设备控制连接认证后不可切换；控制端连接在适配层按 grant 白名单受限，不提供会触发本机副作用的通道 |
| 手机端能力显式排除清单（`createWebPlatform` 逐项禁用文件系统、多窗口、SSH/WSL/Docker、MCP 配置管理） | PWA 控制端用同样做法：能力不靠前端隐藏，靠 host 白名单拒绝；前端排除清单作为第二道一致 |

### 不借

| ZCode 做法 | 不借的原因 |
| --- | --- |
| 连接地址自带授权（URL 即凭据，刷新二维码使旧地址失效） | 架构文档已点名此类模式的缺陷；Fintwind 走配对 + 本机 grant，地址不含凭据 |
| 服务桥全量 RPC：`RemoteConnection` 持有 file/git/system/terminal services 直接桥给浏览器 | 架构文档铁律 2：只转白名单业务消息，不透明代理任意本地能力 |
| 无端到端加密，依赖 relay 侧 cookie/host capability 认证 | 架构文档要求端到端加密，relay 只见密文 |
| Bot Channel（微信/飞书） | 不在第一阶段路线 |

## 已核实的底座现状

实现前先盘点可复用资产，避免另造一套：

- **`fintwind-protocol/src/protocol.rs` 的 wire 协议已是"序号 + 游标补发"形态**：
  `SequencedEvent { session_id, runtime_id, epoch, sequence }`、
  `ReplayCursor`（`epoch` 标识 daemon 进程，重启即换代，对应架构文档的
  "host 运行实例"）、`ClientMessage::Hello { resume_from }` 重连带游标、
  `Request.request_id` 即操作 ID。远控消息面**沿用这套语义**，不发明第二套序号。
- **`fintwind-core/src/server.rs` 已有有界缓冲先例**：
  `MAX_REPLAY_EVENTS_PER_SESSION = 4096`、`MAX_CACHED_RESPONSES = 2048`、
  `MAX_WIRE_MESSAGE_BYTES` 帧上限、常数时间 token 比较、`ConnectionGuard`
  全路径清理。远控侧资源上限的量级与风格对齐这里。
- **`fintwind-core/src/opencode_events.rs`**：单端口单 SSE hub、连接 generation
  通知消费方对账。架构文档未决问题 4（底座事件重放能力核实）以此为入口。
- **`fintwind-daemon`**：薄壳进程，loopback bind + token + stdout JSON 就绪记录
  + 父进程监控。host 守护进程的进程形态（就绪记录、父进程监控、优雅退出）照此复刻。
- **既有本机路径**：持久 submission（`submission.rs`）、permission/question 回复、
  分支/回退、checkpoint、文件/diff 读取——远控适配层只做映射，不复制业务逻辑。

## 组件与代码落点

```
crates/
  fintwind-remote-protocol/   新  远控消息面类型 + 加密帧封套 + schemars JSON Schema 导出
  fintwind-remote-crypto/     新  Noise(snow) 会话封装、身份密钥、校验码；编译 wasm 供 PWA 用
  fintwind-relay/             新  relay 二进制：身份库、路由、配对协调、有界队列
  fintwind-remote-host/       新  host 守护进程二进制：出站 WSS、适配层、去重、审批绑定
  fintwind-core/              少改 仅为适配层补充查询接口，白名单内
  fintwind-daemon/            不动
  fintwind-protocol/          不动
fintwind-remote-pwa/          独立仓库  React + Vite + TS 控制端
src/                          后续 GUI：配对 QR 弹窗、设备管理页
```

- **relay 与 host 的 WS 服务端沿用 `tungstenite` 手写风格**，与
  `fintwind-core/src/server.rs` 一致；TLS 由 Caddy 终结，relay 后端仅 loopback
  （部署清单既定），因此不引入 axum/tokio 之外的新 Web 框架依赖。
- **PWA 独立仓库**：架构文档要求 PWA 发布方与 relay 运维隔离、发布流水线独立。
  托管沿用 fintwind-website 的 Cloudflare 模式。协议类型一致性靠
  `fintwind-remote-protocol` 导出 JSON Schema 生成 TS 类型，CI 校验漂移。
- **加密层 Rust 单实现 + wasm 出口**：PWA 侧不引入 JS Noise 实现——把
  `fintwind-remote-crypto` 编译为 wasm 供 PWA 调用，两端同一份可审计实现，
  呼应架构文档"审查 Rust 与浏览器两端的可用实现；不自行拼协议"。

## Noise 方案要点（M0 冻结）

- **身份密钥**：host 与 device 各自生成静态密钥对，私钥存系统凭据保护机制
  （Windows Credential Manager / DPAPI）；公钥指纹即设备指纹。
- **握手模式**：配对时 XX（双方交换并验证静态密钥，绑定 relay 地址、nonce、
  有效期、协议版本）；日常重连 KK（双方静态密钥已知，认证 + 前向保密），
  每次连接新会话密钥，重连即新密钥周期——满足"重连后使用新密钥周期"。
- **配对校验码**：双方静态公钥哈希截断为短数字串，host 与手机各自显示、
  用户核对（架构文档配对步骤 6）。
- **防重放**：Noise 传输层的 nonce 保序 + 应用层 `operation_id` 去重并存，
  不依赖 relay 的投递序号。
- **明确不做**：不自制握手、不做明文回退、不自动接受新指纹、不做版本降级兼容。

## 资源与寿命初值（M0 给出，隔离验证后冻结）

| 项 | 初值 | 依据 |
| --- | --- | --- |
| 密文帧上限 | 1 MiB | 对齐 `MAX_WIRE_MESSAGE_BYTES` 量级；截图另行限解码尺寸 |
| 每设备投递队列 | 4096 条 / 32 MiB | 对齐 `MAX_REPLAY_EVENTS_PER_SESSION` 数量级 |
| 事件保留窗口 | 与 host 会话缓冲一致（4096 条/会话） | 快照基准游标与增量衔接 |
| 审批 TTL | 5 min | 手机能看到并核对的合理窗口；过期重建审批 |
| 断线输入 pending TTL | 10 min，仅内存 | 架构文档"断线输入与审批"节 |
| 心跳 | ping 10s / pong 超时 60s / idle 10 min | 架构文档候选值，目标网络实测后冻结 |
| ack 超时 / 缓冲宽限 | 20s / 45s | ZCode PersistentProtocol 实测值，作为起点 |
| 未认证连接 | 超时 10s，全局上限 64 | 架构文档"连接认证"节 |
| 登录 / 配对限速 | 5 次/小时/账号 | 防爆破初值 |
| 重连退避 | 数组 [1,2,4,8,15,30,60]s，超限判永久断开 | ZCode `RECONNECT_DELAYS` 模式 |

## 里程碑

### M0 冻结项与骨架

**范围**：架构文档未决问题 1–4 落成文档与代码骨架。
**产出**：`fintwind-remote-protocol` 消息面 v1（信封、序号、游标、操作、审批、
加密帧封套，含 `PROTOCOL_VERSION` 显式协商）；`fintwind-remote-crypto` Noise
封装 API 定型；本表资源初值复核；`opencode_events` 重放能力核实结论
（哪些事件可重放、快照如何生成，写入适配层设计）。
**完成判据**：协议与加密 API 通过两端（Rust 单测 + TS schema 生成）编译消费；
未决问题 1–4 各有书面结论，不再留"实现时再定"。

### M1 relay

**范围**：账号（Argon2id）、device/host 身份库（SQLite）、连接注册表与角色固定、
按 `host_id` 的逐消息路由校验、有界密文内存队列与投递序号、配对协调
（owner 校验、nonce 消费、限速、token 签发）、版本协商、心跳、资源上限与限速、
最小化日志。
**产出**：`fintwind-relay` 二进制 + 部署清单落地（Caddy、systemd、非 root）。
**完成判据**：架构文档 A 组与 B 组中 relay 职责项在隔离环境通过；观察点验证
relay 进程/日志/库中只有密文与身份元数据。

### M2 host 守护进程

**范围**：出站 WSS 长连与自动重连（退避数组）；Noise 会话建立与密钥周期；
`BoundedOutboundBuffer`（已发未 ack 有界缓冲 + ack 超时 + 弃置）；到 daemon 的
loopback 适配层——远控操作白名单映射到既有 `ClientMessage::Request/Command`，
补 grant 校验、操作 TTL、跨重启去重记录、结果查询；审批绑定（审批 ID、
参数摘要、原子消费、竞争唯一终态）；心跳与"连接不可达/状态未知"如实上报。
**产出**：`fintwind-remote-host` 二进制，进程形态复刻 `fintwind-daemon`
（就绪记录、父进程监控、优雅退出）。
**完成判据**：A 组 1–8 与 C 组 23–29 的 host 侧行为在隔离 E2E 通过；
relay 断开时本机 agent 照常运行（优雅降级验证）。

### M3 配对与设备信任

**范围**：八步配对流程全链路（host UI 的 QR 弹窗、手机扫码、XX 握手、校验码
核对、本机确认弹窗含授权范围全文、grant 持久化、确认回执）；撤销双通道
（host 本机 grant 撤销 + relay token 撤销，语义分离）；GUI 设备管理页的
本机查看与撤销；配对 nonce 原子消费 + TTL。
**产出**：GUI 配对弹窗与设备管理入口；`fintwind-remote-host` 的 grant 校验
与撤销执行路径。
**完成判据**：B 组 9–22 逐条验证（QR 拍摄、nonce 重放、密钥替换、跨账号寻址、
撤销即时性、多 host 不隐式授权）；配对超时与轰炸限速生效。

### M4 PWA 控制端

**范围**：登录与 host 发现；会话快照拉取 + 增量订阅（游标恢复）；第一阶段
能力清单的会话面——列表/搜索/筛选、创建（限已有项目）、整理、删除与批量
（分级确认、先停再删、逐项结果）、任务控制（排队/显式中断/停止后继续）、
用户问题回复、工具审批展示（完整参数 + 目标 + 风险，截断不可批准）、
历史分支/回退、会话选项；断线 pending 的内存队列与明示；严格 CSP、
安全渲染（Markdown/终端序列/附件按不可信数据）、不缓存业务响应、
退出清理；移动端虚拟化列表。
**产出**：`fintwind-remote-pwa` 仓库可用站点；wasm 加密层接入。
**完成判据**：C 组与 E 组中控制端职责项通过；架构文档"隐私与数据最小化"表中
手机存储、浏览器缓存两行逐项核对；能力排除清单与 host 白名单双向一致
（前端隐藏项在 host 同样拒绝）。

### M5 只读工作区与隐私边界

**范围**：文件/diff/任务输出只读通道（路径校验、链接目标校验、解码大小上限、
按需分页）；按需截图（遵守既有捕获边界）；推送通知最小化（第一阶段无推送，
则本项仅预留通道设计）；脱敏与发送前裁剪策略。
**完成判据**：E 组 39–45 逐条验证；只读通道伪造消息（编辑、Git 写、终端输入、
配置）在 host 全部拒绝。

### M6 E2E 验收与外部试用准备

**范围**：架构文档"E2E 验收产物"全部条目；隔离 E2E 含两个账号、两台 host、
两枚设备；脱敏时间线产物；部署清单全项；隐私说明与发布恢复流程成文
（未决问题 5）。
**完成判据**：验收产物可重复执行并留档；密码协议与实现审查完成前，
不开放真实工作区外部试用（架构文档红线）。

## 依赖顺序与并行点

```
M0 ──► M1 relay ──► M2 host ──► M3 配对 ──► M4 PWA ──► M5 只读 ──► M6 验收
 │                                │
 └── wasm 加密层 ────────────────►│（M4 需要，可在 M2 后并行开工）
```

- M4 的 UI 骨架（列表、会话视图、审批卡片）可在 M3 结束后用模拟 relay 提前开工，
  但加密与配对贯通前不接真实 host。
- PWA 类型生成、虚拟列表、移动端适配不依赖 M2/M3，随时可启动。

## 风险

| 风险 | 应对 |
| --- | --- |
| snow 的 wasm 产物在移动浏览器表现未知（体积、性能、WebCrypto 交互） | M0 末做 wasm 冒烟验证（配对握手 + 1 MiB 帧加解密计时），不达标则在 M0 内改方案，不带病进入 M1 |
| 底座事件不可全量重放，快照生成成本高 | M0 核实结论直接决定 M2 适配层是有界事件窗口还是按需快照；架构文档已允许后者 |
| relay 单机 2C2G 容量不足 | 资源上限按初值硬编码拒绝新增负载 + 告警；扩容是独立决策，不自动高可用 |
| 手机网络下心跳/退避参数失真 | M4 起在真实 4G/弱网实测，冻结前调整；心跳超时不当作 token 失效 |
| 两套协议（桌面 wire、远控 wire）语义漂移 | 远控消息面复用同一批类型与 serde 表示；schema 漂移 CI 卡点 |

## 不做（与架构文档一致，重申）

组织与团队、能力分层、配额计费、高可用多 relay、原生手机 App、任意端口转发、
Bot Channel、远端设置/项目新增/文件编辑/Git 写/交互式终端/设备授权管理入口。
"给同事开只读权限"类需求在第三阶段之前一律顶住。
