# Relay 加固：失败方式清单与修复记录

范围：仅 `crates/fintwind-relay/`。上游边界：
[remote-control-relay.md](../../../docs/remote-control-relay.md)（分层职责、消息面语义、
威胁模型）、[remote-control-m0-freeze.md](../../../docs/remote-control-m0-freeze.md)
（资源表初值、配对 nonce 语义、决策修订）。编号对应主代理盘点：S1/S2/S3/S14/S17，
另列盘点外发现项。

实现前先列失败方式。每条：触发、当前行为、风险、修复、残余风险。

## S1 帧与请求头尺寸未在端点强制（tungstenite 默认 64 MiB）

- **触发**：`WebSocket::from_raw_socket(stream, Role::Server, None)` 不传配置，
  tungstenite 默认 `max_message_size` 64 MiB / `max_frame_size` 16 MiB；
  `MAX_FRAME_BYTES`（1 MiB）只在 `config.rs` 定义，没有任何执行点。
- **风险**：一个已认证（甚至未认证）连接即可让 relay 分配数十 MiB 缓冲，
  64 条连接就是 GiB 级；frame.rs 明确「endpoints enforce it again on anything
  they accept」。HTTP 侧 `read_head` 用 `read_line` 无限读，客户端不发终止符
  就能让 `String`/`Vec` 无限增长；头部数量无上限。
- **修复**：
  1. WS 握手显式 `WebSocketConfig { max_frame_size, max_message_size } = MAX_FRAME_BYTES`，
     超限帧由 tungstenite 在协议层拒绝（`Error::Capacity` → 关闭会话），不进入解析。
  2. 文本帧解析前再查 `text.len() > MAX_FRAME_BYTES`（纵深，防配置回退）。
  3. `read_head` 改为有界读取：`MAX_HEAD_BYTES` 64 KiB、`MAX_HEADER_COUNT` 64、
     请求行 8 KiB；超限即拒绝并关闭。`Content-Length` 解析失败即拒绝；
     出现 `Transfer-Encoding` 即拒绝（不支持分块，避免歧义）。
  4. REST body 上限维持 64 KiB（`MAX_BODY_BYTES`），在分配前检查。
- **残余**：单帧 1 MiB × 64 连接的理论峰值由 S2/慢消费者条目继续收敛。

## S2 已认证连接上限是死配置

- **触发**：`MAX_AUTHENTICATED_CONNECTIONS = 64` 无任何读取点；
  `Registry::register` 不检查容量；且 `pending_connections` 从 accept 一直持有到
  会话结束—— authenticated 连接终身占用「未认证」名额，两个帽子互相穿透。
- **风险**：已认证连接数无上限；未认证帽子被认证连接耗尽后，新连接在
  `has_admission_slot` 处被 503，正常 host 重连被已有会话挤掉。
- **修复**：
  1. `Registry` 维护已认证计数，`register` 内**原子准入**：先窥视旧 id、计算
     投影数（同实体重连会顶掉旧连接，净增为 0），满则拒绝且**不发生任何状态
     变更**；成功才插入。配对槽与 session 槽都计入同一全局计数，不绕过。
  2. `register` 返回 `Result`，满额时 ws 层发 `RejectCode::RateLimited` 并关闭。
  3. `unregister`/`disconnect` 计数随之释放；被顶掉的连接注销时不误删替代者
     （原有逻辑保留）。
  4. `pending_connections` 改为「未认证名额」：引入 `AdmissionGuard`，
     认证成功（registry 接管）即释放；REST 与一切失败路径随 guard Drop 释放。
- **残余**：上限 64 是 M0 冻结初值，外部试用前需隔离负载复核。

## S3 配对 offer 限速是全局桶

- **触发**：`auth_limiter.admit("pairing-offers")` 单键；`RateLimiter` 的 map
  无上界（sweep 每 60s 一次，窗口 1 小时的键可存 1 小时）。
- **风险**：任一 flooder 耗尽全局桶，正常 host 全部 429；反过来，攻击者用大量
  一次性键可把 map 撑大（内存），或在「淘汰最旧」策略下重置他人计数器（限速旁路）。
- **修复**：
  1. offer 限速改按**已认证 host 身份**：签名验证通过后才 `admit(host_id)`，
     顺序不变（先验签再信 host_id）。
  2. `register` 的全局桶同样改按来源（X-Forwarded-For，与 challenge 端点一致），
     否则注册也被单点耗尽。
  3. `RateLimiter` map 有界（8192 键）：满且是新键时，先剔除**窗口已过期**的
     死键（不重置任何活键计数器，杜绝旁路），仍满则拒绝——可用性损失有界且
     自愈（sweep 收紧为「窗口内」保留）。
- **残余**：X-Forwarded-For 由 Caddy 覆盖写入，loopback 直连可伪造；部署必须
  保持「Caddy 前置 + relay 仅监听 loopback」形态。

## S14 REST 通配 CORS + 跨站 WS

- **触发**：`write_response` 硬编码 `Access-Control-Allow-Origin: *`；
  OPTIONS 对任意路径 204 并带回通配头；错误响应同样通配。
- **风险**：任意站点可读取 relay 的 REST 响应（挑战、注册回包）；通配头与
  「精确 Origin 白名单」的既定边界相反。
- **修复**：
  1. 响应头按请求 Origin **精确反射**：Origin 在白名单 → 回写该 origin +
     `Vary: Origin`；不在白名单 → 403 且**不带**任何 CORS 头；无 Origin
     （原生客户端）→ 不带 CORS 头。OPTIONS 预检与错误响应走同一套逻辑。
  2. WS 升级沿用原有 Origin 检查（浏览器 WS 必带 Origin，跨站被 403）；
     原生客户端无 Origin 仍走独立签名认证，不因 CORS 收紧受影响。
   3. `main.rs` 默认来源是独立 Pages 页面来源；生产部署也通过 unit 的
      `FINTWIND_RELAY_ORIGINS` 显式配置。无 Origin 的原生客户端不使用该列表。
- **部署事实（2026-10-08 迁移决策，纠正本轮早期表述）**：relay 的
  API/WSS 入口仍是 `https://relay.fintwind.xyz`；手机页面迁往
  Cloudflare Pages 项目 `fintwind-remote`，页面来源为
  `https://remote.fintwind.xyz` 与 `https://fintwind-remote.pages.dev`。
  unit 的 `FINTWIND_RELAY_ORIGINS=https://remote.fintwind.xyz,
  https://fintwind-remote.pages.dev` **正是页面来源白名单，配置正确**；
  旧同源托管路径按 `deploy/relay/README.md` 头部说明仅作历史，页面与公网
  链路验收通过后才切 `Caddyfile-isolated`，旧入口不提前关闭。早期报告中
  “unit origin 缺域名、属阻塞项”的说法有误，以此处与部署文档为准。
- **残余**：Origin 是浏览器承诺而非认证；非浏览器客户端本就可伪造，防线是
  逐消息签名（未改动）。

## S17 配对单槽被新设备覆盖（串扰）

- **触发**：`Registry::note_pairing` 直接 `insert(host_id -> device_id)`；
  第二个设备发起的配对静默覆盖第一个。`pairings` 仅在 HostPairing 连接
  `unregister` 时清理，而被顶掉的旧连接早已不在 `connections` 里，其
  unregister 是空操作——旧映射可残留到新配对通道结束。
- **风险**：设备 A 的握手消息被路由给设备 B（串扰）；A 永久等待；一份 nonce
  的配对不再「一次路由」。另发现：host→device 的 **Handshake** 帧在
  `device_id` 存在时**不做任何成员检查**就路由/排队（Sealed 分支有检查），
  未配对设备可被塞入手机握手帧。
- **修复**：
  1. `note_pairing` 返回 `Started`/`Retried`（同设备重发，幂等）/`Conflict`；
     Conflict 时拒绝该帧（`RejectCode::AuthFailed`，明确告知「该 host 已有
     配对进行中」），第二个设备重扫即可——安全优先于可用性。
  2. host 配对通道**注册时清空该 host 的陈旧配对映射**：新配对通道 = 新配对
     尝试，旧通道被顶掉不留下残余路由。
  3. host→device 的 Handshake 帧补上与 Sealed 一致的授权：已配对，或正是
     当前配对对端。Connect 应答（device_id: Some）只可能发向已配对设备，
     不受影响。
  4. 配对对端豁免（决定先于 link 到达）保持不变，且仅限**当前**配对对端；
     映射随任一端连接消失而清除（原有语义）。
- **残余**：配对对端在握手完成后、link 建立前的时间窗内可收 host 的 sealed
  帧（配对决定），窗口由连接生命周期限定，队列有界。

## 盘点外发现（同类缺陷，顺手修复）

- **X1 慢消费者可撑爆内存**：`route_to_*` 向 crossbeam **无界** channel 发送，
  对端不读则内存无限增长；离线队列有界、在线投递无界。
  修复：每连接在线出口缓冲按 `OUTBOUND_MAX_BYTES`(4 MiB)/
  `OUTBOUND_MAX_MESSAGES`(256) 记账，超限返回 `QueueFull`（明确拒绝，不静默）；
  会话循环实际写成功后释放记账；另加写超时（120s）防止永久卡死的对端
  无限占槽。
- **X2 帧限速在解析之后**：不可解析帧不计入限速。修复：先限速后解析。
- **X3 `drain_queue` 格式缺陷**（`{` 后缺换行）与 `EncodedLenHint` 私有 trait
  跨模块复用问题：改为 `pub(crate) fn encoded_len_hint(frame)`；
  `route_to_device` 的 `frame_bytes` 参数改为内部按 hint 计算（口径统一，
  且把 base64 膨胀计入，原实现按原始密文长度少算 1/3）。

## 第二批失败清单（2026-10-08 晚，实施前）

### 2026-10-09 接手补充失败方式（修改前）

- 未认证准入先读计数再自增，多个连接线程可以同时超过全局帽；必须原子预留，
  在创建线程之前预留。超额连接不能再产生无限拒绝线程，也不能阻塞 accept。
- REST 正文滴漏和 HTTP→WebSocket 阶段重置期限可越过从 accept 开始的总截止。
  持续滴漏必须在总截止关闭，健康检查与合法客户端能继续进入。
- 注销先 `outbound.remove` 再第二次 remove，导致全局字节只增不减；被替换的
  连接也必须释放。以在线慢消费者断链后的总计数和正常投递验证，不查源码当测试。
- TTL 测试应在不向过期设备发送的条件下证明后台回收；新发送触发的清理不能
  替代周期回收证明。跨 host 解配对只能清理该 link 的待发帧，不丢另一 host 帧。
- Connect 拒绝需断言目标 host 未收到冒名帧，再断言合法设备准确帧可达。
  仅断言收到任意帧会误把冒名投递当成功。
- 测试目录使用 GUID、隔离 HOME/配置与白名单环境；失败也清理自有子进程，
  产物包含源文件、运行器和二进制哈希，不覆盖之前的记录。

对应主代理第二批盘点。编号 B1–B6。

### 主代理接管后的失败补充（2026-10-09，先列失败）

- HTTP 头、REST 正文及升级后的 WS 认证共用 accept 时刻的总截止，不允许每个
  阶段重置十秒。拒绝连接的排空也使用总截止，且不得无界生成拒绝线程。
- 所有未认证线程在 accept 前原子取得名额；满额立即关闭，不能用无界拒绝线程
  保存 503。拒绝可关闭连接，不静默接受业务帧。
- 慢连接退出、同身份替换、写失败和主动断链都准确回收在线字节；不能先删除
  记账项再试图从它扣总数，不能让已删连接占着全局容量。
- TTL 验证先观察无新流量时离线队列消失，而不是靠重新 enqueue 顺便回收；
  reconnect 不能发送已经过期的帧。
- 冒名 Connect 拒绝要绑定原 host/device/seq，合法另一个设备的实际帧继续通行；
  不能用“收到任意帧”或仅解析日志文字作为正向证明。
- 验收子进程使用隔离 HOME 与环境白名单；唯一目录保存最终报告，输入哈希在
  构建和执行之后核对。新 deadline 测试持续滴漏到总截止，不让旧逐次 timeout
  也能在断言前恰好关闭。

### B1 在线慢消费者未被证明有界（X1 的实证缺口）

- **触发**：设备认证后停止读取，host 持续发送。
- **当时行为**：X1 的记账代码已在，但黑盒只覆盖了**离线队列**（设备不在线，
  帧进 `queues`），从未构造“在线但不读”的对端，4 MiB/256 条在线出口上限
  没有可执行证据。
- **风险**：若记账在某些路径被绕过（如 `drain_queue` 直写、控制帧直发），
  上限即失效，慢消费者重新变成无界内存。
- **修复**：新增在线 slow-consumer 黑盒：设备认证后停止读取，host 发
  ~700 KB 帧，第 5 帧必须 `QueueFull`（字节上限 4 MiB 的精确命中），
  期间另一正常设备仍可被控制（host↔device 双向通行），恢复读取后慢设备
  恰好收到已 ack 的 4 帧（无静默丢失）。

### B2 64 认证 / 64 未认证全局配额竞争无产物

- **触发**：未认证连接占满 64 名额、已认证连接占满 64 名额，同时发生。
- **当时行为**：仅有认证上限的行为测试，无“两个全局配额同时打满 + 断链
  释放”的可重复产物；也没有代码 hash 与日志脱敏扫描。
- **风险**：配额互相穿透（认证连接占用未认证名额——本轮已修）可能回归而
  无人发现；日志泄漏私密材料无人检查。
- **修复**：新增压力测试产物：64 认证 + 64 未认证（slowloris 式半截头，
  每次 read 都有数据故不受 10s 认证超时释放）同时打满；断言既有认证会话
  在全满期间仍可路由、新连接 503、释放后准入恢复；产出 JSON（阶段/计数/
  拒绝码/耗时/代码 hash），并扫描 relay 日志断言不含 nonce、challenge、
  signature、公钥、密文。

### B3 通道完整性攻击面覆盖不全

- **触发**：重放同一密文帧、跨 host 密文、跨 device 密文、内层版本混配
  （旧版本 1 vs 构建版本 2）、fragment 物理/逻辑序号混淆、malformed 输入。
- **当时行为**：已有内层 host/device/version 与毒化，缺重放、跨密钥会话
  密文、版本 1 降级、fragment 序号鉴别、malformed 后不可恢复的显式断言。
- **风险**：relay 或对端把旧密钥周期/别的主机密文投递到本通道；fragment
  的逻辑序号被误当作信封序号校验（或反之）导致大消息被静默丢弃或误收。
- **修复**：E2E 补齐六类攻击，全部要求“拒绝 + 通道毒化”，fragment 用例
  要求**接受**（信封序号匹配帧序号）且逻辑序号原样返回。

### B4 QueueFull 无法关联丢帧

- **触发**：host 收到 `RejectCode::QueueFull`。
- **当时行为**：`ControlFrame::Rejected { code, message }` 无 seq 字段；
  host 侧（`relay_session.rs`）对任何 `Rejected` 只记 warning 后继续，
  既不重传也不标记未知——被拒帧在其有界缓冲里永远等不到端到端 ack。
- **风险**：host 无法知道“哪一帧没送达”，若假定重传可靠就会静默丢消息；
  若直接重放旧 Noise 密文到新密钥周期则必然解密失败（密钥已轮换）。
 - **当前契约**：拒绝帧使用结构化 `RejectedFrame {hostId, deviceId, seq, reason}`。
   日志文案中的序号不作为恢复坐标；该文案不能可靠区分多个设备的同序号。
- **建议（需主代理/后端决策，不在本 crate）**：见 HARDENING.md「QueueFull
  恢复建议」——端到端 ack 未到即视为未送达，恢复路径是**新密钥周期重新
  密封 + 快照校准**，不是重放旧密文。

### B5 XFF 信任边界与邀请来源固定

- **触发**：relay 未限制只有受信代理可写 X-Forwarded-For；配对邀请的
  `relay_origin` 可被二维码替换为任意来源。
- **当时行为**：限速按 XFF 第一个值取键，任何能连上 relay 的客户端都可
  自写；设备侧把邀请 origin 绑进握手 prologue，但“设备期望的 origin”
  不在桥的认知内。
- **风险**：直连（非 Caddy）客户端可伪造 XFF 耗尽他人来源桶；恶意二维码
  可把设备引到攻击者的“relay”完成配对（社会工程 + 假 host）。
- **修复（本 crate 内）**：XFF 仅在**直连对端是 loopback**（即本机受信
  代理）时采信，否则用对端 IP 作键；relay 默认只绑 loopback，部署要求
  非 root 服务账号（systemd unit 属部署文件，此处只文档化）。
- **建议（端点/PWA，不在本 crate）**：设备拨号前必须校验
  `invite.relay_origin` 等于自身配置的 relay 来源（桥已把该字段绑进
  握手，不匹配即失败，但“期望值”只能由端点持有）；PWA 需 CSP
  `connect-src` 固定 relay 来源，CORS 不是认证。

### B6 运营/隐私审计统计缺失

- **触发**：需要判断 relay 是否被限流、队列是否积压、超时是否在涨。
- **当时行为**：只有逐条 audit 日志，无当前计数；也没有“日志不含私密
  材料”的可执行检查。
- **风险**：要么无观测，要么为了观测记录 payload（隐私倒退）。
- **修复**：新增最小聚合计数（认证/拒绝按码/队列深度与字节/超时/路由帧），
  只以**聚合**形式出现在公开 `/api/health`；不新增端点、不新增监听端口、
  无任何按主体（subject）维度数据；payload 永不进日志，并由 B2 的日志
  扫描断言保证。

## QueueFull 恢复建议（给主代理/后端，不在本 crate 实现）

现状（已核实）：`ControlFrame::Rejected { code, message }` 没有 seq 字段；
host 侧 `relay_session.rs` 对任何 `Rejected` 只记 warning 后 `Continue`。
于是 `QueueFull` 今天对 host 意味着“某一帧没了，但不知道是哪一帧”，
而其有界 outbound buffer 仍在等一个永不到来的端到端 `Acknowledged`。

本 crate 已做（零成本、可今天关联）：拒绝消息带上被拒帧的序号，
形如 `device queue is full; seq=4211`。序号本就是发送方写在外层帧上的
relay 可见元数据，回显不新增泄漏；消息文本不是稳定 API，仅作过渡。

**不要做的事**：不要把旧 Noise 密文在新密钥周期上重放。KK 重连会派生全新
会话密钥，旧密文对新周期必然解密失败（`SecureChannel::decrypt` 直接
拒绝），重放只会把可恢复的丢帧变成永久乱序——这正是架构文档
“重连后使用新密钥周期；不得把旧密文当成新操作执行”的含义。

**建议的恢复路径**（需主代理/后端决策）：
1. host 的 outbound buffer 以**明文或可再密封的业务载荷**为持有单位，
   而不是只持有已密封密文；端到端 `Acknowledged` 未到达即视为未送达。
2. 收到 `QueueFull`（或任何未知路由/超时）后，该帧标记为未确认，**不**
   从缓冲释放；恢复动作是“在新密钥周期用当前会话重新密封后重发”，
   并复用同一 `operation_id`（host 侧去重记录按 ID + 摘要绑定，重发不会
   造成重复副作用）。
3. 若重发前发生重连（新密钥周期），先做快照校准（`RequestSnapshot`），
   再按游标补发，避免把旧序号当时序真相。
4. 中期建议在协议层给 `Rejected` 增加可选 `seq` 字段（本 crate 只转发，
   不定义 wire；属 `fintwind-remote-protocol` 决策）。

## 第三批失败清单（2026-10-09，code-review 复核后，实施前）

### C1 离线队列只有 per-device 帽，无全局帽，回收只在同设备新帧/重连时发生

- **触发**：N 个设备各自占满 32 MiB/4096 条；或设备解配对/吊销后其队列无人再碰。
- **当时行为**：`QUEUE_MAX_BYTES`/`QUEUE_MAX_MESSAGES` 只约束单个设备；
  `QUEUE_TTL` 的清理写在 `route_to_device` 的 offline 分支里，只有**同一
  设备再来一帧**或该设备重连 `drain_queue` 才触发。unlink / revoke 不清队列，
  没有对应设备的孤儿队列常驻到进程结束。
- **风险**：512 个设备即可占 16 GiB；目标机 2C/1.9G，靠 MemoryMax 兜底只是
  被 OOM killer 杀掉，不是拒绝。在线侧同样：4 MiB/256 是每连接的，64 条连接
  的理论在线总量 256 MiB，同样没有总量帽。
- **修复**：registry 增加进程内聚合记账（离线字节/条数/队列数、在线字节），
  超限即 `QueueFull`（原子拒绝，零变更）；新增周期性 `sweep_queues`（TTL 到
  期即弃，空队列即删）；unlink 与吊销路径显式丢弃该设备队列；
  `drain_queue`/`unregister`/`release_outbound` 全部同步释放聚合记账。

### C2 准入只有每-read 10s 超时，没有从 accept 起的总截止

- **触发**：客户端每 9 秒发 1 字节，`read_head` 的每次 `read` 都在
  `AUTH_TIMEOUT` 内返回数据。
- **当时行为**：每次 read 成功就重置等待，慢滴漏连接可长期占用 64 个未认证
  名额之一，合法重连被无限推迟。
- **风险**：未认证名额被慢速占用型拒绝服务；`refuse_at_capacity` 的排空若
  写成无界 `read_to_end`，还会随滴漏无限增长。
- **修复**：accept 时记 `accepted_at`，`read_head` 收总 deadline，每次 read
  的超时收紧为剩余时间；超时即关连并归还名额。容量拒绝的排空改为有界读取
  （上限字节数），且该路径本就在 accept 循环派生的线程里，不阻塞 accept。

### C3 设备 Connect 握手的 frame.device_id 未绑到已认证 self_device

- **触发**：设备 D1 已认证，发送 `Handshake{mode:Connect, device_id:D2}`。
- **当时行为**：`device_belongs(host, D2)` 只校验 D2 与该 host 同账户，
  通过后按 D2 转发到 host 会话槽——relay 把**别人**的握手帧送进了 host 的
  通道。Noise 会兜底失败，但 Noise 不是 relay 的授权依据。
- **风险**：已认证设备可向 host 通道注入指向任意同账户设备的握手帧
  （攻击别人的 channel）。
- **修复**：与 Sealed 路径同样绑定 self——设备发出的 Connect 握手，其
  `device_id` 必须等于本连接已认证的 device_id，否则拒绝。

### C4 压力工件固定目录 + 固定文件名，重复运行互相覆盖

- **触发**：两次 `run-stress.ps1` / `run-e2e.ps1`。
- **当时行为**：都写同一个 `relay-quota-stress.json` 和同一个 wasm `pkg`
  目录；没有输入哈希、没有起止时间、没有执行过哪些检查的记录。
- **风险**：主代理读到的是上一轮的旧产物而无从分辨；无法证明产物对应当前
  代码。
- **修复**：每次运行一个唯一 round 目录（GUID），记录测试文件与
  Cargo 输入的 SHA-256、start/finish、执行的检查清单、清理与脱敏摘要；
  `cargo test` 加 `--locked`；不删除旧目录。

### C5 默认 allowed origins 指向 relay 自身，且文档说法不准

- **触发**：未设 `FINTWIND_RELAY_ORIGINS` 启动 relay。
- **当时行为**：默认 `https://relay.fintwind.xyz`；HARDENING 称其为
  "native fallback"——不准确，无 Origin 的原生客户端本就不看白名单。
- **风险**：默认值对一个不存在的页面来源生效，读者误以为它服务原生客户端。
- **修复**：默认改为两个精确的生产 PWA 来源（与正式 unit 一致），无通配；
  测试在 loopback 上用 env 显式覆写。

### C6 限速来源键取 XFF 最左值；ChallengeBook 无全局预算

- **触发**：客户端自写 `X-Forwarded-For`；或本机/代理配置失误放大请求。
- **当时行为**：`source_key` 取 XFF 第一个值（任意字符串），可作为限速键
  进入 `auth_limiter` 的有界 map；ChallengeBook 只按 TTL 回收。
- **风险**：任意字符串键可能挤满限速 map（挤掉真实来源的预算）；challenge
  簿可被无界增长。
- **修复**：只采信 loopback 受信代理给出的**单个规范 IP**，多值/非法即退回
  对端 IP；ChallengeBook 加全局固定在途预算。**注**：review 初稿“公网左值
  spoof”已撤回——relay 只绑 loopback，公网路径由 Caddy 前置，此处是加固而非
  修复已发生事件。

## 第四批失败清单（2026-10-10，冻结轮 13/15 后，两失败定向复现完成）

冻结证据：`relay-blackbox-37a49ea60a0e4a2aa004e9449d025fca/summary.json` 与
`relay-blackbox-8fba346304ea4e149f7574ec0427920b/summary.json`（同源码两次
并发冻结轮，结果一致 13 passed / 2 failed；source/binary/runner 哈希在
report 内）。以下修复前均已在独立合成 fixture 上 `--exact --nocapture`
复现，未输出任何私钥/nonce/密文。

### D1 WS 阶段总截止失效：tungstenite read() 内部循环绕过外层检查

- **复现**：`delayed_websocket_upgrade_does_not_reset_the_auth_deadline`，
  `stage=admission continuous drip exceeded the total deadline`（14.76s 未关）。
- **根因**：`read_frame` 在 `websocket.read()` **外**检查 deadline 并设
  `remaining` 读超时；但 tungstenite 的 `read()` 内部会发起多次阻塞底层
  read（每个到达的帧片段一次），每次成功都重启 SO_RCVTIMEO。对端以
  400ms/字节滴漏一个 4096 字节帧时，每次底层 read 都在窗口内返回新字节，
  `websocket.read()` 永不返回，外层检查永不到执行——每-read 10s 永远
  变不成总 10s。HTTP 头阶段逐字节 read 不受此影响（每次 read 前重设）。
- **修复**：从 socket Read 边界强制绝对截止——`DeadlineStream` 包装
  TcpStream，每次 `read` 进入重算剩余时间并在到点时返回 `TimedOut`，
  tungstenite 内部循环因此终止；认证成功即解除（admission=None），会话期
  恢复既有 250ms 节奏与 IDLE_TIMEOUT，统一 accept 起算、不另加 WS 窗口。
- **资源测量**：每连接一个 `Arc<Mutex<Option<Instant>>>`（每次 read 一次
  lock，纳秒级）；零新增线程、零新增依赖；无 per-admission watcher。

### D2 过期帧无流量回收：sweeper 首轮 60s 晚于观察窗

- **复现**：`offline_frames_expire_and_release_their_budget`，
  `stage=expiry no traffic-independent reclaim`（35s 轮询无回收）。
- **根因**：`route_to_device` 的 TTL 清理只在同设备再来帧时执行（测试无
  send）；后台 `sweep_queues` 挂在 `sleep(60s)` 的 sweeper 首轮之后，35s
  观察窗内从未运行。判据本身正确——问题在回收节奏。
- **修复**：sweeper 改 5s tick；store/限速清扫仍每 60s；队列清扫按
  `QUEUE_SWEEP_INTERVAL`（默认 30s，`FINTWIND_RELAY_SWEEP_INTERVAL_SECS`
  可覆写）判定，TTL 到期即弃并同步释放聚合记账。测试覆写 sweep=2s，
  判据保持：无 send、health 三项归零、新帧只留 1。
- **核验（main 提示项）**：`drain_queue` 先扣 `queued_total_*` 再移除
  队列、`drop_expired` 用局部计数写回，饱和扣减均发生在持有锁的同一临界
  区，无"本地计数 0 饱和而漏扣实际字节"路径；`queue_totals` 为健康面板
  唯一来源，与逐队列求和一致。

### D3（随后补齐，非冻结轮失败项）

- 配额压力脚本冻结化：`run-stress.ps1` 仍写固定文件名、缺依赖哈希与失败
  产物；改为唯一 round 目录 + `--locked` + 输入哈希 + start/finish/checks/
  cleanup/privacy 摘要，满额判据接受"503 或立即断链"（满帽不得繁殖拒绝
  线程），不删除历史轮。
- `run-e2e.ps1` 补依赖哈希、失败报告、finally 恢复 env、子进程清理，
  不写 public/wasm。
- WASM HARDENING 的"全部通过/R2 host 未调用 code"为过时源码状态：pair.rs
  已计算并发出本地码事件，两端真对码尚未验收；R1 为 AES-GCM
  non-extractable IndexedDB，Noise 会话密钥仍在内存可导出——按现状披露，
  不宣称 NonExtractable。

### 第四批实施结果（2026-10-09，同日）

- D1：`DeadlineStream`/`DeadlineHandle` 落在 `ws.rs`（Read 边界每次 read
  重算剩余并设 SO_RCVTIMEO，保底 1ms 防平台毫秒粒度把超时归零成永久阻塞；
  到点返回 `TimedOut`）；http.rs 升级点包装；`run_session` 认证成功即
  `deadline.clear()`。零新依赖、零新线程。
- D2：`main.rs` sweeper 改 5s tick（`SWEEP_TICK`），store/限速清扫独立
  60s 门控（`STORE_SWEEP_INTERVAL`），队列清扫按
  `RelayConfig.queue_sweep_interval`（默认 `QUEUE_SWEEP_INTERVAL`=30s，
  `FINTWIND_RELAY_SWEEP_INTERVAL_SECS` 覆写）。TTL 测试覆写 sweep=2s，
  无 send、health 三项归零、新帧只留 1 的判据逐字保留。
- 定向复现→修复→复跑：D1 由 14.76s 不关 → 10.6s 内关闭通过；D2 由
  35.8s 失败 → 5.2s 通过（首扫即回收）。
- 冻结全量验收：`scripts/check-relay-blackbox.ps1`（main own，本会话仅
  运行）两轮 15/15 通过、`allOwnedProcessesStopped=true`：
  `relay-blackbox-8ef8b63c724a4e3689851e449d4793ee`（D1/D2 修复后的
  第一轮）与 `relay-blackbox-529e004ef9df41af9288a2889a5eac76`
  （18:26:41 完成，binary `FE2B5D62…`、runner `370A012F…`）。
  **时态更正（2026-10-09 深夜）**：ready 协议落地后 `src/main.rs` 已
  再次变化，这两轮 artifact 只对应各自当时冻结的源码，**不代表当前
  源码，不能当当前 pass 引用**；产物按唯一目录保留，不重跑。当前
  源码的最新验证仅到 `cargo check --locked -p fintwind-relay`
  （编译级，ready 协议含内）。
- 遗留死代码警告（非本批引入，main 重构后无人调用）：
  `context.has_admission_slot`、`registry.clear_pairing/drop_queue`、
  `store.verify_device_signature`——待 main 决策删除或接线。

### D3 脚本改造（2026-10-09 晚，本批代码完成，行为验收待下批）

- `tests/stress/run-stress.ps1`：唯一 round 目录 + `--locked` + 输入
  source/end 双哈希（中途漂移记 `inputsChangedDuringRun`，不把漂移轮
  当干净通过）+ 15 分钟总 deadline + 失败也写 `run-summary.json` 后
  exit 1 + `FINTWIND_STRESS_OUT` finally 恢复。进程规则按新 AGENTS：
  只持有本轮 `Start-Process` 返回的 cargo 句柄，`HasExited` 复查后才
  `Kill($true)`；无名称/端口/命令行/目录匹配，无全机扫描。
- `tests/e2e/run-e2e.ps1`：同样改造，另含 wasm-pack/cargo 三个受控
  子进程句柄跟踪、`CARGO_TARGET_DIR`/`FINTWIND_E2E_WASM_DIR` finally
  恢复、20 分钟 deadline、stdout/stderr 落 round 目录。仍不写
  public/wasm。
- 语法解析两脚本通过（Parser::ParseFile 静态检查）；**改造后行为
  验收未跑**（本批禁止启动 fixture/网络测试），当前通过的最新行为
  证据仍是下方冻结验收两轮。
- 门禁健康记录：`relay-blackbox-ac759b2d6a7f4b0989861e6daaed23a3`
  一轮报 `build-source-drift` 失败——是我方在 wrapper 运行窗口内
  编辑 `run-stress.ps1` 所致（对比 summary 的 sourceSha256 与磁盘，
  恰该文件变化）。门禁正确拦截，非 false fail；该轮产物按唯一目录
  保留。

### D3 修正轮（main 审查发现的 runner 实证缺陷，2026-10-09 深夜，代码 ONLY）

先列失败（对上一小节脚本的逐条实证，全部成立，修复随本批提交）：

- **R-a 哈希时序失效**：`run-stress.ps1` 原第 83/84 行、`run-e2e.ps1`
  原第 136/137 行的 source/end 两次哈希**都在** try/finally 运行结束
  **之后**采集——两次都在运行后，间隔为零，根本证明不了运行期间无
  漂移。修复：`$sourceHashes` 在**任何子进程启动前**采集，`$endHashes`
  在 finally 之后采集，二者对比得出 `inputsChangedDuringRun`。
- **R-b 输入清单是手写小清单**：只列了 6–9 个文件，不含实际依赖
  （`fintwind-remote-crypto`、`fintwind-remote-protocol`、
  `fintwind-protocol` 全部源码）。修复：改 `git ls-files --cached
  --others --exclude-standard` 全输入枚举 + 扩展名过滤，与 main
  wrapper 的 Get-Inputs 同法（stress 枚举 relay+三协议/密码 crate；
  e2e 枚举 wasm 桥+三依赖 crate，各含 Cargo.lock 与脚本自身）。
- **R-c `Kill($true)` 不是容纳证明**：依赖遍历进程树，无 job 容纳；
  kill 后未 `WaitForExit` 核验；失败被 catch 吞掉，无法证明
  allstopped。修复（过渡）：kill 后 `WaitForExit(10s)` 逐句柄核验，
  结果入 `processContainment`（killed/stoppedConfirmed/containmentOk），
  失败不再吞——containment 不完整则整轮判失败。**job 容纳等
  ownedprocesslauncher（另 GLM）方案，main 审查后接入**；届时替换
  本过渡路径。两脚本入口加**无旁路 HOLD 门禁**（无参数/env 可绕）：
  写 blocked summary 后 exit 2，在 main 解除前不得启动任何子进程、
  不得改环境、不得宣称任何验证。
- **R-d 日志覆盖**：`run-e2e.ps1` 各 stage 共享同一 stdout/stderr
  文件，后者覆盖前者。修复：每 stage 唯一 `NN-<stage>.out/.err`。
- **R-e raw log 未脱敏**：stderr 全文直接落盘且被当安全内容。修复：
  stage 结束后读入内存，按高熵 base64 串（≥43 字符，即 32 字节密钥
  的编码长度）模式替换为 `[REDACTED]` 并 bounded 截断（128 KiB）后
  才回写磁盘；summary 只记 findings 计数与截断事实，不保存 raw。
- **R-f `privacy.assertNoLeak=true` 硬编码**：未经任何扫描就宣称。
  修复：对日志脱敏结果与 quota stress 产物实际扫描后计算，
  findings>0 或未扫描即 false。
- **R-g 环境未白名单**：子进程继承完整 `process.env`。修复：记录
  原 env → 清成白名单（PATH/SYSTEMROOT/COMSPEC/TEMP 类/CARGO_HOME/
  RUSTUP_HOME/处理器变量）→ 注入本轮独立 `HOME`/`USERPROFILE`/
  `APPDATA`/`LOCALAPPDATA`/`TEMP`/`TMP`（round 目录内）与独立
  `CARGO_TARGET_DIR`（stress 原先用默认 target/，未隔离编译产物，
  一并修）→ finally 整表还原。
- **R-h 报告字段缺失**：summary 补 `fullPlanVerified=false`、
  `publicChainVerified=false`、实际 binary hash（stress：
  `fintwind-relay.exe` 与 quota_stress 测试 exe；e2e：wasm/JS 产物）
  与 cleanup/containment 状态。
- **R-i ConvertTo-Json Depth 6**：按全局规则改 Depth 10。

**边界声明**：历史 15/15（`relay-blackbox-529e004e…` 等）只证明当时
冻结源码的网络行为，**不证明本批 runner 正确**；两个 runner 的运行
由 main 独立审查解除 HOLD 后统一执行，本会话本轮零测试、零 fixture、
零进程操作。

### 启动 ready 协议（2026-10-09 深夜，PWA isolated-real-chain 接口，代码 ONLY）

先列失败（修改前实证）：

- **R-j missing ready**：launcher（CREATE_SUSPENDED + 句柄 job + pipes
  方向，桌面侧未定稿）与 PWA isolated-real-chain 需要从确切本轮 child
  stdout 读一行结构化就绪信号
  `{"type":"relayReady","boundAddress":"127.0.0.1:<port>","pid":<pid>}`；
  此前 relay 没有任何 stdout 契约，调用方只能靠猜端口或解析日志确认
  bind 完成——在 `--bind 127.0.0.1:0`（随机端口）下无法成立。
- **R-k ready 地址来源**：boundAddress 必须取 `listener.local_addr()`
  （内核分配后的真实值）；config 端口在 :0 时是占位符，log 字符串不
  是契约——两者都不得作为来源。
- **R-l stdout 卫生**：stdout 必须只有 ready 一行（JSONL + flush），
  其余日志全部走 stderr；ready 行不得携带 nonce/key/token/publicURL/
  raw error/业务内容。既有启动 log 把 public_url 打进 stderr——不属于
  ready 行，但输出卫生一并收敛：该字段从启动 log 移除。grep 全 crate
  确认无 `println!/print!/stdout()` 直写。
- **R-m 写失败语义**：ready 写入或 flush 失败 = 启动拒绝（`?` 传播，
  main 返回 Err 非零退出），绝不"没宣 ready 还继续 serve"。

实现（`main.rs`）：Store 初始化、Context 构造、TcpListener bind、
sweeper 线程 spawn 全部成功后、`http::serve` 之前写入。协议 2 不变；
auth deadline 与 sweep 语义未触碰。

## 第二批实施结果（2026-10-09）

### B1 在线慢消费者：发现并修复两个真实缺陷，上限现在可证

黑盒实现时暴露的问题不是测试写错，而是 relay 自身的两个 bug：

1. **接受的套接字继承非阻塞属性**（`http.rs`）。accept 循环为能检查
   shutdown 把监听器设为非阻塞，Windows 上 `accept` 返回的套接字**继承**
   该属性。后果：缓冲区一满，写入立刻返回 WSAEWOULDBLOCK
   （tungstenite 0.30 的 `write_out_buffer` 对 WouldBlock 直接 `?` 传播，
   内部队列只用于 `max_write_buffer_size` 溢出的 `WriteBufferFull`），
   relay 把慢消费者当死链关闭，4 MiB/256 条在线记账永远累积不起来。
   修复：接受的套接字设回阻塞（`set_nonblocking(false)`），读写超时
   照旧生效——慢但存活的对端会被 `send` 阻塞等待而非被杀。
2. **写失败路径无日志**。原代码 `send().is_err()` 即 break，运维无从
   判断死因。修复：记录 `link ended (write failed); id=… error=…`，
   并计入 `timeouts` 计数。

修复后黑盒精确命中设计值：设备认证后停止读取，host 发 933 KB 帧，
**第 5 帧被 `QueueFull` 拒绝且消息带 `seq=5`**（4×933 KB=3.7 MiB ≤ 4 MiB，
第 5 帧超限），`queue.messages/bytes` 保持 0（走在线通道而非离线队列），
`timeouts=0`（会话未死），另一正常设备全程可收发；恢复读取后慢设备
恰好收到已 ack 的 4 帧（无静默丢失）。

### B2 全局配额竞争：可重复产物

`tests/quota_stress.rs` + `tests/stress/run-stress.ps1`，产物写入
`FINTWIND_STRESS_OUT/relay-quota-stress.json`，含阶段、计数、拒绝码、
耗时、relay 源码 SHA-256，以及日志脱敏扫描结果（0 泄漏）。

实测阶段：63 设备 + 1 host 打满认证帽（第 64 个 `RateLimited`）→
同实体重连在满额下仍成功（顶替自身，不占新名额）→ 64 个 slowloris 式
半截头连接打满未认证帽（每次 read 都有数据，故不受 10s 认证超时释放），
新连接 **503**（解析前拒绝）→ **两帽同时打满时既有认证会话仍可配对与
投递**（两配额互不消耗）→ 释放 12 个未认证名额后新请求 200、重填后
再 503（记账精确）→ 释放 6 个认证名额后新认证成功 → relay 健康。

实现中另修复一个真实缺陷：**容量拒绝路径不读请求就关闭**，Windows 上
带未读数据关闭会发 RST，可能摧毁刚写好的 503 响应（客户端
`read_to_end` 因此失败）。修复：先响应，再以 200ms 读超时排空已到达
的字节后关闭。

### B3 通道完整性：攻击面补齐

见 `crates/fintwind-remote-crypto-wasm` 的 E2E（6/6）。新增：重放
（同密文二次投递 → `decrypt failed` + 毒化）、跨密钥周期（同 host 第二次
配对的密文 → 解密失败）、跨设备（host 给另一设备的密文 + relay 改写外层
寻址 → 外层校验通过但解密失败）、内层版本降级为 1（`protocol version
mismatch`）、malformed JSON（拒绝后同通道好帧也 `channel poisoned`）。
fragment 物理/逻辑序号在会话通道上验证为**必须接受**且逻辑序号原样返回。

### B4 QueueFull 可关联

`Rejected.message` 现为 `device queue is full; seq=N`（host→device 业务
路径与 `device not connected; seq=N`）。序号是发送方写在外层帧上的
relay 可见元数据，回显不新增泄漏；文本不是稳定 API，仅作过渡。
恢复路径建议见上文「QueueFull 恢复建议」。

### B5 XFF 信任边界

`RequestHead` 新增 `peer_ip`/`trusted_proxy`：仅当**直连对端是
loopback**（即本机受信代理 Caddy）时采信 `X-Forwarded-For`，否则用对端
IP 作限速键——客户端无法通过写头消耗他人预算。relay 默认只绑 loopback；
非 root 服务账号属部署文件（`deploy/`）范畴，此处只文档化。

### B6 最小聚合审计计数

新增 `src/metrics.rs`：认证接受/拒绝、按拒绝码分类的拒绝数、路由帧数、
超时数、未认证拒绝数，全部进程内 AtomicU64，**无任何按主体维度**。
队列深度与会话数以聚合形式从 registry 读取。只在公开 `/api/health`
以聚合字段出现（`sessions`/`queue`/`totals`/`rejectionsByCode`），
不新增端点、不新增监听端口、无敏感 metadata；payload 永不进日志，
由 B2 的日志扫描断言保证。

## 按设计已覆盖、无需改动

- **nonce 一次消费**：store 的条件 UPDATE 是唯一串行点（已有单测）。
- **身份/角色固定**：`admit` 后 role 不可变；重复 `Frame::Auth` 被拒。
- **逐消息路由授权**：Sealed 双向都查 `device_paired_with_host`，store 错误
  fail-closed。
- **relay 不见业务明文**：本批改动不触碰密文内容，只动尺寸/路由/元数据。

## 黑盒 E2E

真实二进制 + 真实 REST/WS 流（手写 HTTP/WS 客户端，零新依赖）。

`tests/relay_blackbox.rs`（7 个用例，共享 harness 在 `tests/common/`）：
巨帧、超大 HTTP 头、非法/合法 Origin（REST+OPTIONS+WS）、重复 Auth 与
身份切换、64 已认证连接上限、配对路由冲突与串扰、未配对设备不可寻址、
离线队列有界（4096 条上限 + 顺序投递）、**在线慢消费者有界且不影响
其他设备**（B1）。

`tests/quota_stress.rs` + `tests/stress/run-stress.ps1`（B2）：
两个全局准入帽竞争与释放，产出带源码 hash 的 JSON 工件，
并扫描 relay 日志断言不含 nonce、challenge、签名、公钥、密文。

断言只输出阶段名与拒绝码，不输出私钥、nonce、密文。

## 验证（2026-10-09 第二批记录；当前状态见第四批实施结果）

> 时态说明：以下数字是**第二批当日**的事实（7/7 黑盒、1/1 压力、6/6
> WASM E2E）。此后 relay 增至 15 case（第三/四批），第四批 D1/D2 修复
> 后的冻结验收为 15/15 两轮（见「第四批实施结果」）；压力与 E2E 脚本
> 在 D3 改造后**尚未行为验收**。引用覆盖率请以第四批实施结果为准，
> 不要引用本节当当前状态。

```sh
cargo check -p fintwind-relay
cargo check -p fintwind-relay --tests
cargo test -p fintwind-relay --test relay_blackbox -- --test-threads=2
pwsh -File crates/fintwind-relay/tests/stress/run-stress.ps1
cargo check -p fintwind-remote-crypto-wasm --target wasm32-unknown-unknown
pwsh -File crates/fintwind-remote-crypto-wasm/tests/e2e/run-e2e.ps1
```

- relay 黑盒：**7/7**（30.6s）。
- relay 配额压力：**1/1**（9.1s），工件
  `relay-quota-stress.json`（阶段/计数/拒绝码/源码 hash/脱敏扫描 0 泄漏）。
- WASM 桥 E2E：**6/6**（真实 wasm 产物 + Node + Rust host 对端）。
- `cargo check` 仅存量死代码 warning（黑盒以独立二进制覆盖那部分 API）。
- Cargo.lock 未因本批改动新增依赖。

## 部署与运维注意（给主代理/运维，不在本 crate 实现）

1. `FINTWIND_RELAY_ORIGINS` 是**页面来源**白名单（`remote.fintwind.xyz`、
   `fintwind-remote.pages.dev`），与 relay 自身的 `relay.fintwind.xyz`
   入口不是一回事；配置正确，无需回退（2026-10-08 迁移决策）。
2. relay 以**非 root** 服务账号运行，且只绑 loopback：这是 XFF 只采信
   loopback 对端的前提，二者必须同时成立。
3. `/api/health` 现含聚合计数字段；若部署文档断言其输出恰为
   `{"ok":true}`，需按实际输出调整。
4. `QueueFull` 的恢复路径见上文建议；host 侧当前只记 warning 后继续，
   被拒帧会在其有界缓冲里等一个永不到来的端到端 ack。
5. PWA 侧需自行校验 `invite.relay_origin` 等于其配置的 relay 来源
   （桥已把该字段绑进握手，但“期望值”只能由端点持有），并以 CSP
   `connect-src` 固定 relay 来源；CORS 不是认证。
