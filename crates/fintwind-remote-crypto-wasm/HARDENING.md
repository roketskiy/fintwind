# WASM 加密桥加固：失败方式清单与修复记录

范围：仅 `crates/fintwind-remote-crypto-wasm/`。上游边界：
[remote-control-relay.md](../../../docs/remote-control-relay.md)（端到端安全边界、
消息面协议语义）、[remote-control-m0-freeze.md](../../../docs/remote-control-m0-freeze.md)
（Noise 参数、配对校验码、身份存放、失败语义冻结）。

实现前先列失败方式（对应 relay.md「先列出失败方式」惯例）。每条给出触发条件、
当前代码行为、修复与残余风险。

## F1 协议版本错配 / 降级

- **触发**：对端或 relay 投递 `SealedMessage.protocol_version !=
  REMOTE_PROTOCOL_VERSION` 的密文；或扫码得到一个高/低版本 invite。
- **当前行为**：`open_frame` 解密后直接返回 body，内层版本无人核对；
  `Pairing::start` 原样接受 invite 的 `protocol_version`。
- **风险**：旧密钥周期或旧协议密文被当成当前消息处理，构成静默降级
  （relay.md「显式 protocol version，握手不匹配直接断」；m0-freeze「明确不做
  版本降级兼容」）。
- **修复**：`open_frame` 解密后强制内层版本相等；`Pairing::start` 早期拒绝
  版本不符的 invite。无任何回退路径。

## F2 内外寻址不匹配（relay 改写路由字段）

- **触发**：外层 `Frame::Sealed` 的 `host_id`/`device_id` 是 relay 可见、可
  改写的路由元数据；内层 `SealedMessage` 的同名字段在 AEAD 内、经认证。原代码
  只核对外层，且外层 `device_id` 可为 `None`（device→host 帧约定省略）。
- **风险**：恶意 relay 把别条会话的密文改写成发给本设备的帧（或改 host_id），
  密文能在本通道解密成功，但消息语义属于另一寻址（relay.md：「relay 可见的
  路由字段与端内验证结果不一致时拒绝」）。
- **修复**：`open_frame` 解密后核对内层 `host_id`/`device_id` 与通道绑定值
  （配对/连接时固定），不一致即失败并毒化通道。

## F3 invite 身份替换（QR 声称的 host 公钥 ≠ 握手真实公钥）

- **触发**：`PairingInvite.host_public_key` 只是 QR 展示文本；XX 的 prologue
  绑定 host_id/nonce/过期/relay/版本，**不绑定 host 静态公钥**，后者由握手
  message 2 传授。原代码 `Pairing::finish` 丢弃 `read_reply` 的 offer，从不
  比对二者。
- **风险**：攻击者用真实 `host_id` + 自己的静态密钥应答配对；设备以为配对
  成功，把攻击者公钥当 host 身份保存，日后 KK 重连全连到攻击者。校验码也救
  不了——真实 host 根本没参与这次握手，无人可对码。
- **修复**：`finish` 后强制 `channel.remote_static() == invite.host_public_key`，
  否则配对失败、不产出通道。校验码只从**核对通过**的握手公钥计算。

## F4 失败后通道继续可用（JS 忽略错误）

- **触发**：`open_frame` 抛错后，Noise 传输态已被失败解密/nonce 消耗污染；
  JS 若 catch 后继续调用，会把不可恢复错误当可恢复错误。
- **风险**：状态错乱的通道继续收发；relay.md「身份验证或解密失败即关闭业务
  通道」，m0-freeze「重连即新密钥周期」。
- **修复**：`WasmChannel` 增加 `poisoned` 标记。`open_frame`/`seal_frame` 在
  任何安全相关失败（帧解析、非 sealed、寻址/版本不符、解密失败、sealed 解码
  失败、encrypt 失败、超尺寸帧）后置位，之后所有调用 fail-closed，只能新建
  连接。纯调用方输入错误（body JSON 解析，发生在任何密码操作之前）不毒化。

## F5 身份 JSON 密钥不一致

- **触发**：`Identity::from_json` 加载被篡改/损坏的身份文件。
  - F5a：`verifying` 字段被静默忽略（`SigningKey::from_bytes` 从私钥派生后
    直接覆盖），声称的验签公钥与签名私钥不符时不报错。
  - F5b：`noisePublic`/`noiseSecret` 只查长度（32 字节），不验证对应关系。
- **风险**：F5a 让损坏文件带病运行；F5b 让设备对外显示错误的指纹与配对校验码
  （m0-freeze：指纹 = SHA-256(公钥) 前 16 字节；校验码双方各显示同一数字），
  用户在 host 端看到的指纹与设备端不一致，或配对校验码永远对不上。
- **修复**：F5a 直接比较，不符即拒。F5b 用一次**自对抗 KK 握手**验证：同一
  身份同时充当 KK 双方，message 1 的 `es` 令牌把发起方临时钥与应答方静态钥
  混合，公钥被替换则 MAC 失败；完整交换后应答方学到的 remote static 必须等于
  存储的 `noisePublic`。验证器就是冻结的 Noise 实现本身——不自制密码、不新增
  依赖、不动 Cargo.lock。

## F6 超大帧（端点侧尺寸执行缺失）

- **触发**：`frame.rs` 约定「The relay enforces this before parsing;
  endpoints enforce it again on anything they accept」（MAX_FRAME_BYTES）。
  原桥两个方向都不检查。
- **风险**：恶意 relay 投递巨帧，PWA 侧 JSON 解析内存放大；本端发出超大帧
  也会被 relay 拒投却无人知晓。
- **修复**：`open_frame` 入口与 `seal_frame` 出口按 `MAX_FRAME_BYTES` 拒绝。

## F7 重连时 host 身份漂移未被设备察觉

- **触发**：`WasmChannel::connect` 完成后不暴露握手绑定的对端静态钥/指纹。
- **风险**：host 重装换密钥（m0-freeze：「host 静态私钥丢失即身份终结…
  不自动接受新指纹」）后，设备若无从核对，等于静默接受新 host 身份。
- **修复**：新增只读 API `remote_static_b64()` / `remote_fingerprint()` /
  `Identity::noise_fingerprint()` / `PairingAccepted::pairing_check_code()` /
  `PairingAccepted::host_fingerprint()`；`Connect::finish` 断言握手绑定的
  host 公钥就是入参提供的那把。PWA 负责把配对时拿到的指纹与每次重连的指纹
  比对并在变化时要求重新配对。

## 第二批失败清单（2026-10-09，实施前）

| 编号 | 必须拒绝或恢复的失败 | 结果 |
| --- | --- | --- |
| B3a | 重放：同一 Noise 密文二次投递不得被当成新消息 | 拒绝（`decrypt failed`）+ 通道毒化 |
| B3b | 跨密钥周期：同一 host 第二次配对（新 XX 交换、新传输密钥）的密文不得在旧通道解开 | 拒绝（解密失败） |
| B3c | 跨设备：host 给另一设备的密文，即使 relay 改写外层寻址到本设备也不得解开 | 拒绝（外层校验通过、解密失败） |
| B3d | 内层协议版本降级（v2 构建收到 v1） | 拒绝（`protocol version mismatch`）+ 毒化 |
| B3e | malformed 输入后通道不得恢复继续 | 拒绝后同通道好帧亦 `channel poisoned` |
| B3f | fragment 的物理序号（信封）与逻辑序号（片段自身）不得混为一谈 | 必须接受，且逻辑序号原样返回 |

## 第二批实施结果

`tests/e2e_replay_foreign.rs`（新增，与 `e2e_remote_link.rs` 共享
`tests/common/` harness；每个用例独立配对，因为首次失败即毒化通道本身
也是被断言的一部分）。fragment 用例加在 `e2e_remote_link.rs` 的会话
（KK）通道上：信封 `transport_seq` 必须等于帧序号，而
`MessageBody::Fragment.transport_seq` 是逻辑序号，取值 4242 与帧序号 7
不同——设备必须接受并原样返回该值。混为一谈的实现会在此静默丢弃或
误收大消息。
## 按设计已覆盖、无需改动

- **重放**：Noise 传输 nonce 逐消息递增，snow 对重复/过期 nonce 直接失败 →
  解密错误 → 通道毒化（F4）。应用层仍按 `operation_id` 去重（relay.md）。
- **握手乱序/截断**：`snow` 状态机失败即 Err，不产出通道（fintwind-remote-crypto
  模块头失败方式 5，已有单测覆盖）。
- **未知消息无控制效果**：`Frame`/`MessageBody` serde 层 `deny_unknown_fields`
  + tag 判别，未知变体解析即失败 → 进入 F4 毒化路径。

## 报告项（不在本模块修复）

- **R1**（2026-10-09 更新为当前事实）：设备身份私钥的持久化已按
  m0-freeze 方案落地——PWA 侧 vault 为 AES-GCM 加密的 non-extractable
  IndexedDB 条目，`localStorage` 明文路径已废。**残余**：
  `Identity.to_json`/`from_json` 仍在桥 API 上可产出明文 JSON（保持
  API 兼容不删除），且 Noise 静态/会话密钥活在 wasm 线性内存里，同一
  JS 上下文的任意代码仍可读取——「NonExtractable」只覆盖浏览器管理的
  vault 密钥，**不**覆盖 Noise secret。结论不变：不宣称 NonExtractable
  全覆盖；内存暴露靠同源策略与无第三方脚本约束。
- **R2**（2026-10-09 更新为当前事实）：host 侧 `pair.rs` 已计算校验码
  并随本地配对事件发出（"host 未调用 pairing_code"为过时描述）。
  **仍未验收**：host 与设备两端对**同一个数字**的端到端真对（双端
  blackbox）尚未跑通——配对人肉防线不能标记为闭环，以真实双端对码
  证据为准。
- **R3**：`Identity::sign` 对任意 challenge 签名。PWA 必须只签从 relay 官方
  端点取得的 challenge（跨连接重放签名无意义，但不应签来历不明的字节）。
- **R4**：F5b 的自对抗 KK 探针若主代理更希望集中在密码层，可改为在
  `fintwind-remote-crypto` 增加 `IdentityKey::verify()`（x25519-dalek 一行），
  本桥改调用它；当前实现不新增依赖、不动 Cargo.lock。

## API 兼容性

只增不破。既有导出（`Identity.generate/to_json/from_json/signing_public_b64/
noise_public_b64/sign`、`Pairing.start/step0_b64/host_id/finish`、
`PairingAccepted.message3_b64/take_channel`、`WasmChannel.connect/seal_frame/
open_frame`、`Connect.step0_b64/finish`）签名与语义不变；PWA 现有调用零改动。
新增 5 个只读方法见 F7。

## 验证（2026-10-09 批次记录；当前状态见下）

```sh
cargo check -p fintwind-remote-crypto-wasm --target wasm32-unknown-unknown
pwsh -File crates/fintwind-remote-crypto-wasm/tests/e2e/run-e2e.ps1
```

- `e2e_remote_link.rs`：**5/5**。
- `e2e_replay_foreign.rs`：**1/1**（重放、跨密钥周期、跨设备、版本降级、
  malformed 后不可恢复，五个攻击在一个用例内依次断言）。
- 合计 **6/6**，全部跑在 wasm-pack 产出的真实 wasm 模块上。

**当前状态（2026-10-09 晚，勿当全部通过引用）**：以上 6/6 是当日历史
批次的事实记录。此后 `run-e2e.ps1` 经两轮改造（round 目录 + 总
deadline + 进程句柄跟踪 + finally 恢复 env + source/end 双哈希）；
main 审查又实证其缺陷（source/end 哈希都在运行后采集、手写小清单、
stage 日志互相覆盖、raw stderr 未脱敏、env 继承未白名单、summary 缺
fullPlanVerified/publicChainVerified/产物哈希）——修正轮已按
relay HARDENING「D3 修正轮」逐条落码，但**入口加无旁路 HOLD**
（blocked summary + exit 2）：进程容纳仍待 ownedprocesslauncher（job
对象）方案，main 审查解除前不得运行。配对校验码的双端真对（R2）同样
仍未通过。最新通过的验证仅到 wasm32 `cargo check`（编译级）。历史
6/6 只证当日源码行为，不证明本批 runner 正确。
## 认证投递序号（协议 v2 新增，已落地）

`SealedMessage.transport_seq` 是认证投递序号：端点把它和密文一起封进
AEAD，relay 无法在不破坏认证的前提下改写外层帧序号。

- 配对通道（`ChannelKind::Pairing`）：业务载荷序号一律为 0；Hello 与
  Decision 本身不带序号。
- 会话通道（`ChannelKind::Session`）：必须等于外层 `Frame::Sealed.seq`。
  不符即拒绝（`transport_seq does not match the frame`）并毒化通道。
- 片段（`MessageBody::Fragment`）另有自己的逻辑序号，与信封序号无关：
  二者不得混为一谈（见第二批 B3f）。
- 该字段**不可** serde 默认跳过：缺失即视为 0 之外的错配，按错配拒绝。

## PWA 接入注意（给另一会话）

- 配对前必须校验 `invite.relay_origin` 等于 PWA 自身配置的 relay 来源。
  桥把该字段绑进握手 prologue，不匹配的握手会失败，但“期望值”只能由
  端点持有，桥无从判断哪个 origin 才是对的。
- CSP 需以 `connect-src` 固定 relay 来源；CORS 是浏览器承诺，不是认证。
- 原生（无 Origin）客户端走逐消息签名，与 CORS 收紧无关。