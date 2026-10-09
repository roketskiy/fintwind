# M0 冻结结论

本文落实 [remote-control-relay.md](remote-control-relay.md) 未决问题 1–4，作为
[remote-control-plan.md](remote-control-plan.md) M0 里程碑的产出。这些结论是
第一阶段的实现前提；改动须修订本文并说明原因，不得在实现中静默偏离。

数值类条目冻结的是**初值**：允许隔离试验调优，外部试用前按架构文档要求
复核并冻结终值。语义类条目（握手模式、恢复流程、失效语义）不随试验调整。

## 1. 密码协议与库（未决问题 1）

**库**：Rust 端 `snow` 0.10（Noise Framework 成熟实现，2025-07 发布 0.10.0，
维护稳定）。PWA 端复用同一实现：`fintwind-remote-crypto` 编译为 wasm 供
浏览器调用。**不引入任何 JS 侧密码实现**，两端加解密走同一份可审计代码。

**参数**（Noise 默认安全档）：

| 项 | 值 |
| --- | --- |
| DH | Curve25519 |
| Cipher | ChaChaPoly |
| Hash | SHA-256 |

**握手模式**：

- **配对握手：XX**。双方交换静态公钥并互相认证，配对消息（relay 地址、
  `host_id`、nonce、有效期、protocol version、双方角色）绑定进握手载荷。
  配对完成后，双方静态密钥互为已知。
- **日常连接：KK**。已知双方静态密钥，握手同时完成设备认证与前向保密。
  host 与 PWA 每次建立连接（含断线重连）都运行新的 KK 会话——**重连即新
  密钥周期**，满足架构文档"重连后使用新密钥周期；不得把旧密文当成新操作
  执行"。
- 传输阶段 Noise 自动维护逐消息 nonce；应用层副作用防护不依赖它，
  仍按 `operation_id` 去重。

**身份与密钥存放**：

- host 与 device 各持一对静态密钥。私钥存操作系统凭据保护
  （Windows Credential Manager / DPAPI）；PWA 使用 wasm 侧生成的
  NonExtractable 格式由浏览器管理，不落 localStorage（架构文档既定）。
- 公钥指纹 = SHA-256(公钥) 十六进制前 16 字节，即设备指纹与 grant 绑定值。

**配对校验码**：双方各显示

```
校验码 = HKDF-SHA256(salt = sort(host_pub, device_pub) 拼接,
                     ikm = 两者静态公钥拼接, info = "fintwind-pairing-v1")
         取 6 位十进制数字
```

两端显示同一数字，用户核对。同类机制先例：Bluetooth numeric comparison、
ZRTP SAS。6 位对"用户在场核对"场景足够；不用于无人值守校验。

**明确不做**：自制握手、明文回退、自动接受新指纹、版本降级兼容、
跨设备共享业务解密密钥。

**wasm 构建冒烟（2026-10-07）**：`fintwind-remote-crypto` 在
`wasm32-unknown-unknown` 上编译通过。树上同时存在两个 getrandom 大版本，
各自需要 wasm 专属配置，已落在代码里：0.2（snow/rand）用 `js` feature、
0.3（uuid）用 `wasm_js` feature 加 `.cargo/config.toml` 里 wasm 目标的
`--cfg getrandom_backend="wasm_js"`，uuid 另需 `js` feature。浏览器内的
实际握手与 1 MiB 帧加解密计时（规划文档风险表第一条）推迟到 PWA 接入
wasm 时在真实运行时测量，编译冒烟不构成性能结论。

## 2. 生命周期与资源数值（未决问题 2）

继承 [remote-control-plan.md](remote-control-plan.md) 资源初值表，以下为
补充冻结的寿命项：

| 项 | 初值 | 语义 |
| --- | --- | --- |
| device 访问 token | 15 min | 短寿命；仅 relay 访问与发现，不授权业务 |
| device 刷新 token | 30 天，一次性轮换 | 重用检测即撤销整条凭据链 |
| host 连接凭据 | 长期，可撤销 | enroll 换得，日常重连复用；泄露即撤销重enroll |
| 配对 nonce TTL | 5 min，原子消费 | host 与 relay 双侧消费 |
| 审批 TTL | 5 min | 过期重建审批，不补执行 |
| 断线输入 pending | 10 min，仅内存 | 重载即丢，界面持续明示 |
| 操作去重记录 | 覆盖请求 TTL 全程，持久化 | 不因队列满淘汰仍可重试的记录 |
| 事件保留窗口 | 4096 条/会话 | 与底座 journal 上限一致 |

其余连接数、帧大小、队列深度、限速数值沿用规划文档表格，不再重复。

## 3. 账号恢复与跨 host 撤销（未决问题 3）

**relay 账号密码重置**：只恢复 relay 账号本身。不新增、不恢复、不替换任何
host grant。重置后设备持有未过期的 device token 仍可用（token 与密码独立），
刷新轮换照常；若怀疑账号失陷，提供"撤销该账号全部 device token"的管理操作，
关闭所有连接并要求各设备重新登录——这仍不触碰任何 host 本机 grant。

**忘记设备**：撤销永远是双通道，语义分离（架构文档"撤销语义"节）：

1. host 本机 grant 撤销——host 设备管理页执行，立即失效该 host 上的
   连接与排队动作，是可靠执行边界。
2. relay token 撤销——relay 账号页或 host 管理页发起，立即拒绝新认证、
   关闭关联连接、清除未投递队列，无宽限窗口。

**多 host 场景的撤销确认**：每台 host 只管理**自己**的 grant；relay 账号页
显示全部设备的 token 状态，以及每个 host 的本地撤销确认进度。host 不可达时
显示"撤销待确认"，不显示"已全部撤销"。

**跨 host 的本地 grant 撤销通知**：第一阶段**不实现** host A 自动撤销
host B 的 grant——架构文档指出该通知须由目标 host 已信任的身份认证，
不能由 relay 签发，其身份与投递方案是独立设计。第一阶段以双通道撤销 +
逐 host 确认状态如实显示收口；自动跨 host 通知留待后续独立文档。

**host 重装 / 换密钥**：host 静态私钥丢失即身份终结。旧 grant 全部失效，
重新 enroll 产生**新的 `host_id`**（不复用旧标识），各设备重新配对。
relay 侧旧 host 记录标记退役并保留审计元数据；不提供"恢复旧身份"路径。

## 4. 本地适配与结果恢复（未决问题 4）——底座核实结论

以下为 2026-10-07 对 `fintwind-core` / `fintwind-protocol` 源码核实的快照
（文件与行为以行内注释与单测为准）。

### 已具备，直接复用

| 能力 | 位置 | 结论 |
| --- | --- | --- |
| 序号 + 游标补发 | `fintwind-protocol/protocol.rs`：`SequencedEvent{session_id, runtime_id, epoch, sequence}`、`ReplayCursor`、`ClientMessage::Hello.resume_from` | 与远控"事件流单调 `seq_id` + 重连带游标"一一对应；`epoch` 即"host 运行实例"，daemon 重启换代，旧游标不遮蔽新事件（有单测） |
| 有界重放 journal | `fintwind-core/server.rs`：`MAX_REPLAY_EVENTS_PER_SESSION = 4096`，环形淘汰 | 超窗走快照重拉，语义已定 |
| 事件可重放性分级 | `EventSink::send`（replayable=true）/ `send_ephemeral`（false，仅 PTY 原始输出） | 终端原始输出天然 live-only，不进远控通道 |
| 响应缓存与去重 | `server.rs`：`MAX_CACHED_RESPONSES = 2048`，重放 `request_id` 从缓存回答且不重复执行；browser 命令按连接 seen-id 集合失败关闭 | "同一操作 ID 重试不重复执行副作用"的现成机制 |
| 提交持久化与未知结果 | `submission.rs`：`AcceptSubmission` 原子持久化先于执行；`SubmissionState::{Dispatching, Unknown}` + `is_unconfirmed()`；`DispatchSubmission` claim-once；`receipt.supersedes()` 秩比较 | 正是架构文档"接受记录先持久化再执行副作用；结果未知先查询不盲目重试"的实现，远控输入通道直接建立在它上面 |
| 审批回复一次性 | `driver/opencode.rs`：审批"always"只更新驱动会话本地规则，Fintwind 只发 provider 一次性回复，不写全局 always | 远控"此次允许/拒绝"不创建免审批规则的底座保证 |
| 任务状态快照 | `Command::LoadTaskState` → `TaskState{projects, sessions, ...}` | PWA 会话列表/项目筛选的数据源 |
| 会话正文按需加载 | `Command::FetchNativeTranscript`（按 session 拉取） | 满足"正文按需加载，不全量同步" |

### 缺口，由 host 适配层补足

1. **"列出当前有效待批请求"查询**。审批请求（`DriverEvent::Permission` /
   `UserInputRequested`）可重放，重连客户端能从 journal 恢复待批视图；但
   journal 是环形缓冲，被淘汰或跨多次重连后没有权威查询。驱动内有
   `OpenCodePermissionState.pending` 表但无导出接口。**适配层新增一个查询：
   从驱动状态导出按会话的有效待批审批/问题清单**，恢复时以它为准，
   事件重放只作展示补充。
2. **审批与远控设备绑定**。底座审批 ID 是 OpenCode 的 `per` 请求 ID，不绑定
   设备与授权版本。适配层为远端审批决定包一层远控审批记录（审批 ID、
   来源设备、目标动作摘要、grant 版本、TTL、终态），执行前原子消费；
   底层 `Respond`/`RespondUserInput` 语义不变。
3. **运行中发送排队**。底座 `Command::Prompt` 在会话忙时的行为是拒绝或
   steer 语义，没有有界排队。适配层实现按会话的有界队列（排队、可查看、
   可取消），"停止后发送"显式走 `Cancel` + 提交路径——架构文档"复用行为
   与明确例外"的落地。
4. **批量与会话管理命令**。批量归档/停止/删除、归档/恢复归档在本机是 GUI
   编排的多个原语操作。适配层把它们编排为远控批处理命令：先停再删、
   逐项报告、失败不扩大，对象清单在确认时冻结。
5. **文件读取路径校验**。底座文件/diff 读取按工作区操作实现，未内置
   "仅已登记项目 + 拒绝穿越/越界链接 + 解码大小上限"的远控约束。适配层
   在白名单入口统一校验，不修改本机路径。

### 结论

底座不需要为远控改动传输与持久化语义；缺口全部收敛为适配层新增代码，
`fintwind-core` 预期仅暴露第 1 条所需的查询接口。

## 冻结规则

- 本文语义条目变更须修订本文并更新规划文档对应行。
- 数值条目在隔离 E2E 后可调，终值在外部试用前复核一次并在此登记。

## 决策修订：放弃账号层（2026-10-07）

产品决策：**不引入任何手动注册与密码填写**。relay 从"形态 3（多账号共用）"
收敛为**形态 1（单人自用）变体**，架构文档
[remote-control-relay.md](remote-control-relay.md) 的身份与归属模型按此修订：

- **公钥即身份**：host 与 device 各持 Ed25519 签名密钥（注册/认证用）+
  X25519 静态密钥（Noise 用）。注册 = 向 relay 提交公钥 + 挑战签名。
- **签名即登录**：relay 不发不存任何长期连接凭据。每次 WSS 连接：
  relay 发随机挑战 → 客户端静态私钥签名 → relay 用注册公钥验证。
  `Frame::Auth` 的 `credential` 字段改为挑战签名；token/refresh 体系整体删除。
- **配对即归属**：删除 `accounts` 表与全部账号逻辑。路由隔离改为查
  `pairing_links`（host ↔ device 信任关系），该关系只能由 host 在本机
  确认配对后、经已认证的加密通道声明；device 只能寻址配对过的 host。
  一个 device 可配对多台 host（关系表多对多），每台独立确认。
- **配对协调**：owner 校验删除，由 QR 一次性 nonce（host 原子消费）+
  双方校验码 + 本机确认承担——这三者本来就是防串扰的实际防线。
- **恢复语义简化**：设备丢失 = host 本机撤销 grant；host 重装 = 新密钥
  重配对。原未决问题 3 中账号密码重置、跨账号撤销的部分不再存在；
  跨 host 撤销通知维持"第一阶段不做，UI 如实显示"的结论。
- **威胁模型更新**：注册开放后防滥用依赖注册/配对限速（数值沿用 M0 表）；
  relay 数据库泄漏的暴露面缩小为公钥与信任关系（密码哈希不存在了）。
  "A 账号不得寻址 B 的 host"改述为"未配对 device 不得寻址 host"，
  验收条目含义不变。

架构文档其余部分（配对流程、端到端加密、审批绑定、数据最小化、验收组）
不受影响，继续有效。

## 部署锚点（已定）

- **relay 公网入口**：`https://relay.fintwind.xyz`，Cloudflare **Proxied（橙云）**，
  DNS A 记录指向 UCloud 服务器 `165.154.64.159`（Debian 12.7，2C/1.9G/59G，已核实空闲）。
- **源站证书用 Cloudflare Origin CA**（SSL/TLS → Origin Server 签发，15 年有效，
  手动更换），Cloudflare SSL/TLS 模式必须设 **Full (strict)**；Caddy 加载
  Origin CA 手动 `tls` 配置，不用 ACME（HTTP-01 穿不透代理）。
- **信任披露**：橙云代理使 Cloudflare 成为第二个 TLS 终结点与元数据观察者。
  它能看到外层 wire 帧（密文 + 路由元数据）与连接元数据，看不到端到端明文，
  也不持有任何 host/device 私钥——与"不防 relay 运营方看到元数据；自托管
  只改变信任对象，不消除云商观察"一致，属必须向用户披露的范围。
- 免费版代理对空闲连接有 100 秒超时；relay↔端点 keepalive 为 30 秒，已覆盖。
- Caddy 终结回源 TLS，relay 后端仅 loopback；云防火墙放行 443（80 可关，
  不再需要 ACME），22 维持现状；3389 模板默认项已删。
- 2G 内存无 swap：已配置 2G swap（部署时落地）。
- 配对 QR 与 host enroll 配置中的 relay 地址一律使用该域名，不用裸 IP。

## 实施修订：手机身份存储与发布隔离（2026-10-08）

Rust `snow` 的 Noise 私钥必须在端点 WASM 内存中可用，不能直接用浏览器的
不可导出 X25519 `CryptoKey` 代替。实际持久化采用独立身份库：不可导出的
Web Crypto AES-GCM 包装密钥和密封的 Rust 身份保存到 IndexedDB；连接时仅在
内存中解封，退出清理身份使用会话。不再把原始身份 JSON 写入 localStorage。
旧身份只在加密存储事务成功、能够解封核对后删除，存储失败不能生成替代身份。

这保护的是浏览器静态存储中的原始私钥，不是硬件密钥隔离。同源恶意脚本仍能
调用包装密钥或读取连接时的身份，不能声称 Noise 私钥在所有执行路径都不可导出。
业务内容、队列和操作结果仍禁止持久化到 IndexedDB 或其他浏览器业务缓存。

手机页面拟迁移到 `remote.fintwind.xyz` 的 Cloudflare Pages；中继仍位于
`relay.fintwind.xyz` 的 UCloud。Pages 发布秘密不放在 UCloud，旧中继来源在
迁移验收后停止提供手机代码。仅不同子域名不够，必须落实独立发布权限和流程。
两者由同一 Cloudflare 账号管理的风险仍披露，不声称独立账户级隔离。
迁移操作与验收见 `deploy/pwa/README.md`，完成状态见 `remote-control-progress.md`。
