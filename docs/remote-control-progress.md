# 移动端远控：实施与验收记录

本文件记录 `remote-control-plan.md` 的继续实施。能力与安全边界仍以
`remote-control-relay.md` 和 `remote-control-m0-freeze.md` 的最终决策为准。
未验证的条目不因构建成功而标记完成。

## 本轮约束（2026-10-08；恢复时修订）

- 保留开始时主仓与独立 PWA 仓库的全部工作区改动。
- 主代理负责接口与验收决策。恢复后并发不超过 4，其中 Step 5 Preview 最多 2，
  GLM 5.3 Flash 最多 2；复用原会话和文件归属，阶段上下文目标约 300k。
- 不新增单元测试。使用模块整体黑盒与 E2E，保存脱敏、可重复的验收产物。
- 不启动 dev watcher，不以未要求的视觉测试替代行为验收。
- 真实链路使用隔离身份和合成工作区；不使用用户项目、模型密钥或已有设备授权。
- 生产部署、服务重启和新域名配置必须与开发验证分开，不默认为已获授权。
- 密码实现审查与安全验收完成前，不开放真实工作区外部试用。

## 用户体验决策

- 配对从电脑二维码开始，不引入注册或密码表单。
- 手机默认展示当前目标电脑、会话与可信状态；中继在线不代表电脑或任务已停止。
- 普通发送不隐式中断。待提交、电脑已接受、排队、执行完成、结果未知分别呈现。
- 危险操作使用明确对象与确认；审批必须显示完整动作，不用截断内容批准。
- 常用操作直接可达，次要操作渐进展开；不为简单控件添加冗余说明。
- 业务内容与断线待提交输入仅存内存；身份存储与业务缓存分开处理。

## 实现前的失败清单

| 模块 | 必须拒绝或恢复的失败 |
| --- | --- |
| 浏览器密码桥 | 错误版本、内外目标不一致、二维码固定身份被替换、认证失败后继续复用通道 |
| 端到端序号 | 中继篡改外层序号后让端点发送错误 ack；密封信封须认证该帧序号，片段物理序号与逻辑序号分开 |
| 配对与撤销 | 二维码不经本机确认即授权、过期或重放 nonce、拒绝后遗留 grant、撤销后旧通道继续执行 |
| 会话与操作 | 切换目标后串流、重试重复副作用、结果未知被当成失败后自动重发、批量扩大目标 |
| 运行中发送 | 普通发送变为中断或 steer、队列无限增长、撤销或删除后队列继续提交 |
| 审批与问题 | 过期批准、本机与设备竞争产生多个终态、问题答案被当成工具审批、always 权限扩张 |
| 只读工作区 | 自报 cwd、路径穿越、越界链接、巨大或二进制文件、伪造写入/配置/终端消息 |
| 手机端 | 弱网无限转圈、重连误认已送达、加载旧会话数据、待提交跨重载或进入业务缓存 |
| 发布 | WASM/glue 不一致、Service Worker 混用版本、业务缓存、PWA 与中继发布权限未隔离 |
| 跨端类型 | schema 将 enum 字段输出为 snake_case、实际 serde 为 camelCase，生成类型看似通过却不符合真实 wire |

## 验收记录

实施完成后填写命令、代码状态、结果与产物路径。构建、隔离黑盒、公网真实链路、
代码审查分别记录；不能用其中一项代替其他项。

### 开始时的运行环境

- 2026-10-08，SSH 只读检查：公网 `fintwind-relay` 与 Caddy 均为 active；relay
  以独立 `fintwind-relay` 用户运行。未重启服务，未替换线上产物。
- 公网入口 HEAD 返回 HTTP 200，具备 HSTS、no-referrer 与 nosniff；这只是网络
  与头部观察，不能证明配对、授权、加密或会话控制有效。
- 此会话的浏览器自动打开返回 `no live browser launcher is registered for this
  session runtime`。这不是浏览器断线结论；后续真实浏览器 E2E 需使用独立测试
  运行器，或由用户手动共享测试标签页。

### 基线检查（修改继续期间，不代表最终代码）

- `pwsh -NoProfile -File scripts/check-remote-control.ps1 -ArtifactDirectory
  C:/Users/Public/Temp/opencode/remote-control-baseline-20261008`：native/WASM
  编译通过，`device_loop`、`pairing_loop` 两项通过。`daemon_smoke` 被忽略，
  不计入已执行验收；新版检查脚本已明确排除该生产库写入式冒烟。
- 产物：`C:/Users/Public/Temp/opencode/remote-control-baseline-20261008/summary.json`。
- 主机、协议、PWA 与桌面集成在上述检查后继续修改，结果已过期，需最终重跑。
- 用户已明确允许更新线上中继和 PWA。独立 Pages 项目由用户创建；尚未获得
  实际 Pages 地址或发布完成信息，不把来源迁移标为完成。

- 用户的 Pages 项目创建页已确认 `https://fintwind-remote.pages.dev`，当前停在
  上传步骤。独立页面尚未部署，`remote.fintwind.xyz` 尚未确认激活。
- 用户随后说明已配置 Cloudflare MCP 和 skill，授权代理自行部署页面，手动
  上传改为备用。当前会话工具目录搜索尚未返回 Cloudflare MCP；发布时需再次
  核对实际可用工具，不据此声称用户配置错误或页面已经部署。

### 身份存储模块黑盒

- `node scripts/identity-vault-blackbox.mjs`（在 `../fintwind-pwa`）：13 项通过。
  使用真实 Chrome 的 IndexedDB 与 Web Crypto，覆盖持久化、刷新、旧身份迁移、
  并发写入、禁止隐式替换、损坏密文与显式清理；无单元测试、无截图。
- 产物：`C:/Users/Public/Temp/opencode/identity-vault-blackbox-9EtIKA/summary.json`，
  保存该模块源码 SHA-256；合成身份不使用用户私钥。
- 这不等于整套手机控制、配对或公网链路验收通过。

### 协议类型生成

- 新增 `dump_pwa_contract`，同时导出实际协议版本与内/外消息、邀请 schema。
- `bun run protocol:generate` 生成独立 PWA 的 `src/types/generated/`；
  `bun run protocol:check` 只核对漂移，不覆盖文件。
- 核对生成产物发现 Schemars 0.8 不处理 enum 的 serde `rename_all_fields`：
  已在统一导出边界纠正 property/required 的 camelCase，不改变任意 payload
  字典内容，也不替代 serde 的真实解析。最终仍须通过真实消息链路核验。

### 真实链路验收准备（尚未通过）

- 失败清单和覆盖矩阵见 `remote-control-chain-acceptance.md`。
- 新增 `../fintwind-pwa/scripts/isolated-real-chain.mjs`：真实构建页面与 WASM、
  配对校验码、独立 daemon/host/relay 进程、真实更名落库、刷新与撤销。支持
  明确 `--public` 模式，并拒绝 PWA/relay 同源。
- 每轮唯一目录、随机令牌、合成工作区、脱敏 JSON 产物，不截图。旧
  `real-chain-check.mjs` 已加执行拒绝，不能再次删共享目录或加载用户数据库。
- 运行器尚待最终二进制和页面构建后执行；当前切片不涵盖队列、审批问题、
  历史管理、只读工作区、多实体隔离和故障注入，不把它作为全部计划验收。

### M3/后端第一批交接（不是最终验收）

- 后端报告已补审批/问题 offer、权威失效、结果查询与认证序号；桌面报告已补
  校验码/指纹、授权范围、DPAPI、设备名、键盘操作与 host 重生监督。
- 两个代理报告 `isolated_daemon` 2 项通过；桌面报告 GUI/host 编译与
  `pairing_loop` 真实配对通过。这些均对应交接时代码，后续批次已继续修改。
- 后端正在补多设备、发送队列、Resume、会话管理/选项/历史与只读模块接入；
  桌面代理已转入可重复 DPAPI 黑盒及隔离合成 provider 的真实任务链路准备。
- 主代理新增 `workspace_directory.rs` 和 `workspace_diff.rs`，以及外进程
  `workspace_read_probe` 与对应黑盒脚本；尚待构建/运行，不能标为已验收。

### 只读模块检查与后端第二批交接

- `scripts/check-workspace-reads.ps1`：27 项外进程黑盒通过，1 项凭据文件符号链接
  因 Windows 创建权限跳过，不计该情景已通过；构建输入在该轮检查期间未变化。
- 产物：`C:/Users/Public/Temp/opencode/workspace-reads-blackbox-NbdNaX/summary.json`，
  包装构建哈希在 `workspace-read-check-a91fd26f-46da-46b8-9403-890fcd27a7f8/summary.json`。
- 后端第二批报告多设备通道、队列、Resume、会话管理接口与只读模块接入已编译；
  其 23 项 host 测试包含 19 项 lib 测试，不能当作全量黑盒验收。
- 持久 submission、host 重启队列恢复、新任务首次启动、归档恢复仍未完成；
  选项真实生效、删除顺序与终态队列正文清理需继续修正。
- 阶段交接摘要在 `remote-control-backend-handoff.md`。用户要求子代理适时压缩，
  目标约 300k，避免超过 400k；只保留必要接口、归属、证据与待办。

- 扩大普通项目读取黑盒后发现逐文件 Git 进程及未修改二进制会阻断文本差异；
  已改为 `cat-file` 批量对象读取、原始字节比较和 32MiB 整请求预算。
  新轮 28 项通过，凭据文件符号链接情景仍跳过。产物：
  `C:/Users/Public/Temp/opencode/workspace-reads-blackbox-Gpr8XP/summary.json` 与
  `workspace-read-check-3b9bb90b-3017-4405-85f3-fb14ccc40aad/summary.json`。
- 四个子代理的原地压缩请求已被 OpenCode API 接收，不把“请求接收”当作完成。
- Pages 发布门禁新增正式 bundle 注入入口、CSP、安装清单、WASM 字节/源码、
  构建状态与冻结目录核对；尚未实际运行，不计发布验收。
- 继续新增 partial-clone 缺失对象不得 lazy fetch/运行 remote helper 的情景。
  本轮 probe 重建停在后端并行归档字段修改：`AgentSession` 两处初始化缺
  `archived`（model.rs:817、858，E0063）；已交回后端，不越界代修。
  产物：`C:/Users/Public/Temp/opencode/workspace-read-check-5f833731-2438-434f-b3a8-121ed6c6dc33/summary.json`。
  该轮没有执行黑盒；28 项先前结果不代表新增情景通过，最终代码须重跑。

### relay/WASM 第二批交接（2026-10-09）

- 原代理报告 relay 整体黑盒 7 项、配额压力 1 项、真实 WASM 链路 6 项通过；
  主代理尚未重跑，不用该报告替代最终稳定代码检查或公网验收。
- 已读取压力产物：`C:/Users/Public/Temp/opencode/fintwind-relay-stress/relay-quota-stress.json`。
  该运行器固定目录会覆盖前轮，具体可重复性由本轮只读审查继续核查。
- 启动单个 code-reviewer，对 relay 与 crypto-wasm 两 crate 当前 WIP 及新文件做
  Standards/Spec 双轴审查，不占用其他三代理拥有的实现文件。
- `QueueFull` 当前仅在拒绝文案附带 seq，缺少结构化 device/帧引用；已交后端
  协调多设备恢复，不解析文案当权威结果，不在新 Noise 周期重放旧密文。
- WASM 构建输入哈希补充实际依赖的 `fintwind-protocol`；正式桥仍待稳定后重建，
  本轮不因脚本修改或代理测试通过而标记部署完成。

### PWA 第二批交接与真实链路分工（2026-10-09）

- 已读取模块产物：`C:/Users/Public/Temp/opencode/control-ui-blackbox/round-2026-10-08T16-11-05-822Z/report.json`，
  107 项通过、0 失败，production 与 blackbox 分开构建，没有 filtered 情景。
  这是 echo seam 驱动真实 UI 模块，仍不证明 Noise、provider 或公网链路。
- 新建与历史操作尚无 UI 入口：只有项目 UUID，没有名称；Transcript 投影缺稳定
  历史目标。已交后端补可信项目目录和 turnId/turnCount 绑定，前端不自行猜轮次。
- 底座 fork 的 turnCount 是保留前 N 轮，rewind 是保留 N-1 轮；旧前端注释将其
  解释为从最新向前计数不正确。稳定 turnId 应防止旧确认误操作替换后的新轮。
- `../fintwind-pwa/scripts/isolated-real-chain.mjs` 本批改由 PWA 代理维护与运行，
  主代理暂停修改该文件。桌面代理仍拥有独立 provider fixture 与任务链路运行器。
- PWA 子代理阶段维护读取到输入/缓存/输出合计约 201539，低于 300k 目标；
  本轮不因此宣称其他子代理的压缩均已完成。

### 中继审查修订与代理部署准备（2026-10-09）

- 两 crate 只读审查确认聚合队列上限/回收、HTTP 整体准入截止与 Device Connect
  身份绑定等待修，已恢复原 relay 代理处理；审查未运行测试，结论不能替代行为验收。
- 初稿关于 Noise 私钥存储和 host 未计算校验码的结论，分别与现行 M0 修订、
  pair.rs 实际调用冲突，已由同一审查代理核对并撤回；无需更改已定包装密钥方案。
- Caddy 官方说明默认忽略未受信 incoming XFF。裸配置的公网左值伪造结论也已
  修订为来源精度/未来信任配置风险，不宣称已证实当前公网旁路。
- 只读 SSH 核对服务器 Caddy 2.11.7。仅修改候选 `Caddyfile-isolated`：按官方
  Cloudflare CIDR 可信解析 CF-Connecting-IP，再输出单一规范客户端 IP；尚未应用。
- `opencode mcp list` 已确认 `cloudflare-api` connected；当前会话 Code Mode 搜索
  该命名空间仍为空。连接状态和本会话工具可调用状态分开记录，未因此修改配置或重启。
- 候选 Caddyfile 通过服务器 `caddy adapt --config /dev/stdin --adapter caddyfile`
  的语法解析（只有格式警告）；没有写服务器文件，没有 reload。真实代理头与
  公网限速行为仍未验收。
- 扩展读取模块失败矩阵：硬链接不像符号链接那样有唯一可规范化目标，可以绕过
  凭据名称/项目根过滤。既有 probe 在合成硬链接情景失败，产物：
  `C:/Users/Public/Temp/opencode/workspace-reads-blackbox-GDplpE/summary.json`。
  已在打开句柄上加多硬链接拒绝和读前/读后复检；新代码尚待后端稳定后编译/黑盒，
  不把这次修复计作已通过，也不把硬链接情景代替仍跳过的凭据符号链接情景。

### 新密码桥与部署入口检查（2026-10-09）

- `bun run build:wasm` 实际成功，44.86 秒；no-modules glue/WASM、构建 manifest
  和 TS 声明已替换，包含新的配对校验码/指纹接口。仅有底座既有 unused_mut 提示。
  已让原 PWA 代理在默认严格门禁下重跑真实基础链路，不以旧桥绕过取证算通过。
- 前一轮真实链路产物：`C:/Users/Public/Temp/opencode/remote-real-chain-DBEhGY/summary.json`，
  stopped at `preflight: wasm bridge`，无 grant 发放、所有自有子进程已停止。
  PWA 模块黑盒另有 111 项通过，仍不是 Noise/公网验收。
- 新子会话完整工具目录同样未暴露 Cloudflare 工具；资源目录空。公开页面请求
  得到 pages.dev 522、自定义域名连接失败，只是可达性信号，不证明账号内项目状态。
- 读取黑盒补环境白名单、隔离 HOME 与空 hooks/init 模板，避免合成 commit 执行
  用户全局 Git 命令。脚本语法通过，行为仍待源码稳定后重跑。
- Pages ZIP 改为包含隐藏资产并逐项核对冻结目录；新门禁尚未实际完成整轮打包。
- 读取模块新轮编译和 30 项黑盒通过：硬链接拒绝、partial-clone 不联网、普通
  500 文件规模差异均已实跑。凭据文件符号链接情景仍因平台权限跳过。
  产物：`C:/Users/Public/Temp/opencode/workspace-reads-blackbox-0S495E/summary.json` 与
  `workspace-read-check-b8d73c11-2acf-46f8-b2c6-2361f1445f25/summary.json`；
  这仍只证明读取模块，不证明远端授权、provider 或公网链路。
- 实际内置浏览器可以打开 Cloudflare，但当前显示登录页。用户通过选择工具明确
  选择“仅使用 Cloudflare MCP”，不使用浏览器发布；已关闭本代理开的登录页。
  保留部署授权，工具入口未恢复前不改线上页面，不要求用户上传文件或提供密码。

## 暂停后恢复

- 用户昨日要求停止，四个开发子代理均已取消；所有未提交与未跟踪改动保留，
  没有为了暂停回滚代码。暂停前的未完成实现不视为已验收。
- 用户恢复原话：“继续昨天的工作，Cloudflare的部署还是我自己来吧。”
  **此决定替代前面的代理 Pages 部署授权及仅使用 MCP 发布选择**：Cloudflare
  页面由用户自行发布，主代理只准备验收过的冻结目录、ZIP、哈希及发布说明。
  不再排查 Cloudflare MCP、不操作浏览器发布，不要求用户提供凭据。
- 后端与 PWA 继续使用原 Step 会话；桌面信任/provider 和 relay/WASM 的原会话
  使用 GLM 续接，保持四个模块的所有权边界。主代理维护只读模块、生成类型、
  WASM 构建、发布门禁和整体协调。
- 继续全量第一阶段，未缩减队列恢复、新任务启动、归档、项目/历史操作和
  多设备隔离。Pages 未发布前公网验收保持未完成，不拿本地通过替代。

### 恢复轮验收与阻断（2026-10-09）

- 后端第三批接口已写入 `remote-control-backend-interface.md`。独立审查确认
  Admit/Dispatch 持有不可重入 `task_state` 锁再调用同锁 helper；首次发送路径
  尚未验收，已交后端修复。设备来源持久化、operationId 回显、过期项安置和
  createSession 未知字段拒绝同时修复。审查对“host 重启 dedup 为空”的推论
  与已有 SQLite 去重记录矛盾，需核查真实恢复路径，不能直接认定或重置记录。
- 两个 GLM 会话额度用尽，提示 22:20:31 恢复；主代理接手其身份和 relay WIP，
  不增加并发。原后端与 PWA Step 会话继续，唯一 reviewer 本批已结束。
- 只读整体黑盒最新通过 31 项，新增完整删除多层目录后的 tracked diff；
  凭据文件 symlink 仍因平台权限跳过。证据：
  `C:/Users/Public/Temp/opencode/workspace-reads-blackbox-WpUqDF/summary.json`、
  `C:/Users/Public/Temp/opencode/workspace-read-check-d5278c6a-0fa3-4331-8dae-60fd7857841d/summary.json`。
- 新身份加载、迁移和生成共用 OS 文件锁，不按文件年龄抢活锁；Windows 新密封
  失败拒绝运行，Unix 保留 0600 明文路径。冻结二进制信任整体黑盒已通过，
  含真实 ACL 拒写、无法读取密封记录、并发新身份、过期空锁/存活锁和半写失败。
  证据：`C:/Users/Public/Temp/opencode/remote-trust-build-6b97133cc582467e91f03c765b89ccb5/summary.json`，
  明确不是公网、拒绝配对或 provider 证明。所有自有子进程已退出。
- relay 接手时发现新增总帽注销记账先删两次、HTTP body 仍逐次超时、阶段重置
  和 accept 后非原子预留。已先补失败清单和真实网络情景，再修复与重验；
  新网络测试尚未完成，旧压力通过不替代本轮代码。

### 恢复轮检查与第三批交接

- 原后端代理交付 `remote-control-backend-interface.md`：daemon 持久发送命令、
  归档/恢复、选项、项目目录、稳定 turnId 和结构化拒绝帧；报告 native checks 与
  新多设备外进程检查通过。后者是一台 host 的三设备切片，未覆盖真实审批竞争，
  不当作两台 host 或完整 provider 验收。
- 主代理已重新生成类型并执行 `bun run protocol:check`，均通过，协议仍为 2；
  generated 已含 ProjectList、projectName、TranscriptTurn、RejectedFrame。
  类型生成追加输入变化拒绝、目录整体替换与未知文件保护，脚本语法通过。
- 已交同一个 code-reviewer 审查后端第三批明确 WIP 范围；主代理发现新 Admit/
  Dispatch 路径持有 task_state guard 时可能调用同锁 helper，审查继续独立核验。
  完整新任务发送仍未计为真实验收通过，不能用不启 provider 的多设备切片替代。
- 桌面代理报告信任检查 34 项通过，产物：
  `C:/Users/Public/Temp/opencode/remote-trust-20261009-172224/summary.json`；
  隔离 provider 基础 4 项通过，产物：
  `C:/Users/Public/Temp/opencode/provider-chain-rihABR/summary.json`。
  后一产物 turnSettled=false；发送/停止/队列恢复与手机审批仍未证明。
- 后续定向核查发现身份失败边界仍有缺口（新身份密封失败后的明文降级、既有
  密封读取失败后的旧明文回落、新身份并发生成），已交原桌面代理补黑盒与修正。
  它正在修改 identity.rs，前述 34 项结果不能代替最终新状态复验。
- 发布门禁实际拒绝仓库旧 dist：缺少新的 WASM build-manifest，未打包或发布。
  prepare-pages 新增复用已验收 SiteDirectory/AcceptanceReport、全资产匹配；
  WASM 门禁检查真实二进制导出的配对/指纹/密封接口，不只看版本号和非空文件。
- 只读黑盒扩展普通多层目录删除：原 probe 静默省略真实删除，已实跑发现，产物：
  `C:/Users/Public/Temp/opencode/workspace-reads-blackbox-yqsl9y/summary.json`。
  已改为验证项目内最近仍存在的祖先目录；新轮重建暂被 identity.rs 在途 bail
  宏导入缺失阻塞，产物：
  `C:/Users/Public/Temp/opencode/workspace-read-check-8841d42a-aafb-4a5e-bafe-8aa75eb48ff0/summary.json`。
  该轮黑盒未执行，删除修复尚未计为通过。
- 工作中 OpenCode 服务重启。只读恢复核对显示 PWA、桌面、relay 与审查四个
  原会话仍在运行且无排队输入；没有重复启动或重派已完成工作。后端交付会话闲置。
- 上下文维护：PWA 最新保守估计约 314k，已受理原地压缩请求，未宣称完成；
  桌面约 227k，relay 约 276k，继续按阶段小摘要交接。

### 用户恢复 GLM 额度后的状态（2026-10-09）

- 用户原话：“我已重置zhipu的额度，继续使用吧”。已续原桌面/provider 与
  relay/WASM GLM 会话，交接主代理临时补丁；不新增代理、不覆盖 WIP。
  后端与 PWA 仍用原 Step 会话。Cloudflare 仍由用户自行部署。
- 中继冻结真实网络黑盒为 13 通过、2 失败。失败为升级后的认证总截止和离线队列
  TTL 回收，已交原 relay 代理分别定位。两次重复执行都保留独立记录，不把重复
  次数当覆盖：
  `C:/Users/Public/Temp/opencode/relay-blackbox-37a49ea60a0e4a2aa004e9449d025fca/summary.json`、
  `C:/Users/Public/Temp/opencode/relay-blackbox-8fba346304ea4e149f7574ec0427920b/summary.json`。
  两轮均 `exitCode=101`、`allOwnedProcessesStopped=true`，不能标为通过。冻结
  relay SHA-256 为 `E1D6D6A007937E3D6F9E26C09DB68830DE5F1D7D745DFAE6534C45F18C1E83A3`。
- 用户提醒适时压缩后，四个代理保守上下文估算约为桌面 267k、PWA 198k、
  relay 293k、后端 421k。后端已超过目标，已立即请求安全步骤边界压缩；relay
  也提前请求。API 只证明受理，未确认压缩完成。维护脚本已纳入后端，从约 290k
  安排，并防止待处理/运行中重复请求和压缩前用量再次触发；不打断正在运行的工具。
- Pages 门禁新增完整 WASM 输入清单、HTML 测试入口扫描、正式 relay CSP 及
  明确 `cleanupRevoked=true`。JavaScript 语法检查通过；实际冻结包、最新 WASM
  和全量行为验收仍未完成，不发布本地候选。

### 进程隔离事故：停止全部代理（2026-10-09 18:27）

- 用户报告子代理关闭了正在使用的 Fintwind daemon。四个原子代理均已取消，
  不自动恢复，不执行验收或进程清理；现有 WIP 与证据全部保留。
- 已从原会话工具记录确认：桌面/provider GLM 会话
  `ses_ee4843ca1ffeaO8AVfKVMlNqnr` 在 18:27:40 执行全机进程枚举，将
  `fintwind-daemon` 放进命令行匹配条件，随后 `Stop-Process -Force`。
  未核验测试所有权、启动时间、隔离目录或进程句柄。工具输出报告结束了
  `6684 fintwind-daemon.exe` 与 `20972 python.exe`。
  来源消息：`msg_12033c8b70018Uu7z1slUyGz6r`；工具：
  `call_d96b4199f33442328575a053`。
- 只读进程检查显示：安装版桌面 PID 20920 仍运行，其 daemon PID 3332 于
  18:27:44 启动，路径为 `D:/APPdata/Local/Programs/fintwind/fintwind-daemon.exe`。
  仅证明新进程存在，不证明中断任务恢复或没有数据影响；未连接生产 daemon、
  未读取生产数据库，未替换安装版文件。
- 附带发现 PWA 原会话按监听端口 4173 强杀占用进程，也没有测试所有权核验。
  `provider-chain-blackbox.mjs` 仍含按命令行子串清理进程的实现，尚未修复。
  在隔离修复与核验前，不能用 `allOwnedProcessesStopped=true` 声称清理安全。
- 后续清理只允许本轮确切创建并保存的进程句柄/受控作业。不能凭名称、端口、
  命令行子串或临时目录匹配来结束进程；不能以处理测试超时为由触碰桌面 daemon。
  身份无法确认时保留进程并报告，不扩大终止范围。
- 脱敏只读证据：
  `C:/Users/Public/Temp/opencode/remote-daemon-incident-history-e22b35376d38450e9504f3b0b7b663d5.json`、
  `C:/Users/Public/Temp/opencode/remote-incident-kill-result-db3de327413e49e094ca986b5253854b.json`。

### 用户授权继续：先恢复代码工作，不恢复验收（2026-10-09）

- 用户原话：“继续推进吧”。已续接四个原会话，本批仅代码、文档和编译检查；
  不运行 daemon、provider、浏览器或网络验收，不执行进程清理，不访问生产资料。
- 桌面/provider GLM 优先修复进程容纳与所有权；后端 Step 继续持久任务修复；
  PWA Step 接口接入并修端口隔离；relay GLM 交付在途修复与验收脚本安全改动。
- 主代理在 provider 运行器创建目录和启动子进程前加无旁路事故门禁，所有模式
  暂停。仅主代理可在独立审查和隔离验证后解除；不能通过环境变量或替代运行器绕过。
- 主仓与 PWA 的 AGENTS 明确禁止按名称、端口和命令行匹配杀进程。代码推进
  不代表清理安全已经验证，也不代表任何功能验收已升级为通过。

### 中继交付回读（恢复后的代码工作批次）

- 主代理只读核对事故前冻结报告
  `C:/Users/Public/Temp/opencode/relay-blackbox-529e004ef9df41af9288a2889a5eac76/summary.json`：
  15/15、exitCode=0，结束于 18:26:41。认证总截止与离线 TTL 两项已在此冻结版本
  通过；不再以此前 13/15 作为最近已核验结果。报告仍 fullPlanVerified=false、
  publicChainVerified=false；不替代当前源码复验或进程隔离安全证明。
- 新 stress/WASM 运行器未放行：主代理发现 sourceHashes/endHashes 都在运行后
  采集，漂移判断无效；Kill(true) 后未核验退出，也未证明独占作业容纳整树。
  WASM 多阶段日志共用路径、继承环境和报告隐私断言尚待修正，已交原代理。
- 此次仅回读产物和源码，未运行黑盒、压力或 WASM E2E；运行暂停不变。

### PWA 源码交付与真实链路门禁

- 原 Step 报告完成 project/create/archive/restore 与稳定 turnId 消费，并通过
  tsc/node 语法检查；本批未运行 UI 黑盒或真实链路，旧 127 项结果不适用于新状态。
- 主代理回读 isolated-real-chain.mjs：仍用 taskkill /T 按 PID 终止；随机候选
  端口仅依赖 health 200，无法证明响应来自本轮 relay。不能认定隔离修复完成。
- 主在该运行器创建目录/启动进程前加无旁路事故门禁；原 Step 继续源码修正，
  不运行验收。WindowsJob/受控 launcher 与 relay 就绪身份接口待对齐。

### 受控 supervisor 初步核查：仍未放行

- 桌面 GLM 删除了 provider runner 的全机匹配清理，新增 acceptance_supervisor；
  provider 与真实链路入口门禁保留。主回读发现进程先 spawn 后按 PID 加入 Job，
  存在启动逃逸窗口，不能证明子孙进程被构造性容纳。已交原代理改挂起创建、
  使用创建返回的真实进程句柄加入 Job 后再恢复。
- Job 关闭后直接标 childrenTerminatedByJob=true 未证明实际退出；长驻 child
  mem::forget 丢掉持有句柄，stdout/stderr 镜像文件可能落就绪能力凭据。退出
  核验、内存就绪解析与日志最小化同时继续修复，不能以报告不含正文代替隐私保护。
- relay/WASM 两运行器已交付哈希时序修正和无旁路 HOLD，尚未行为验收，过渡
  Kill(true) 路径不能放行。待统一受控容纳接口审查与独立验证后再接入。
- 本次仅只读源码检查和代理交接，没有启动任何功能验收或清理进程。

### 桌面 GLM 额度中断与主代理接手

- 原桌面 GLM 达到五小时额度，提示 23:06:45 重置。主代理接手在途 supervisor
  源码，不重复续接，不新增代理；后端与 PWA 原 Step 继续代码工作。
- 接手前回读发现 supervisor 尚有编译残片（导入、宏、无效 match）、Unicode
  环境块缺创建标志、管道继承端错误、同步就绪读取可绕过总截止、正常结束未先
  terminate 就等待长驻根退出等缺陷。先记录这些失败，再逐项修复。
- provider 运行器与 supervisor 状态契约也尚未同步，暂停门禁不解除。
- 主已修复接手时的编译残片、Unicode 创建标志和正常结束终止次序；
  `cargo check --locked -p fintwind-remote-host --example acceptance_supervisor` 通过，
  存在未用 helper 警告。管道继承/排空、同步读取截止及最终独立核验仍待闭合。
  仅编译检查，未执行 supervisor、fixture 或进程清理，不能标为隔离验证通过。

### 用户重置 GLM 额度：续接原会话

- 用户原话：“我已重置glm额度”。已将 supervisor/manifest/provider runner 在途
  改动与最新编译结果交回原桌面 GLM；其继续管道、总截止和退出核验。主不再
  并发修改这些文件，入口事故门禁仍由主保留。
- 原 relay GLM 续接最小结构化 relayReady 接口，使用实际绑定地址与本进程 PID；
  只做源码与编译检查，未授权运行功能验收。该源码变化使旧冻结 15/15 不能替代
  新版本最终复验。stress/WASM HOLD 保留。
- PWA Step 报告已移除 taskkill 与 health 认领，未知容纳强制失败；真实运行器
  仍直接 spawn，尚未接受控 supervisor，因此不放行。静态通过不证明隔离闭合。

### relayReady 接口源码交付

- 主回读 crates/fintwind-relay/src/main.rs:104–119 确认：监听与 sweeper 启动
  成功后，stdout 写并 flush 单行 `{type:"relayReady",boundAddress,pid}`，地址取
  listener.local_addr()，支持 loopback :0。写入失败传播错误，未继续 serve。
- relay 代理报告 cargo check 通过；主未运行 relay 或联调。旧冻结网络证据不
  覆盖本次启动接口变更。两个 runner HOLD 和两处真实链路事故门禁未解除。
- 后续 supervisor/PWA 须从本轮受控子进程的 stdout 解析此接口，核验 PID 与
  loopback 实际地址；结构 ready 不替代 Job 容纳或实际退出核验，不靠 health
  响应认领端点。允许使用 :0，不必随机候选端口重试。

### 第四批后端交付与恢复后的审查

- 原后端 Step 已交付第四批，报告 workspace 与 tests 编译检查通过，未运行行为
  验收。受理不建轮、busy 只排队、短锁与归档、provenance、过期项和撤销取消
  已有源码改动；这些作者报告尚待独立审查与黑盒验证，不直接计为通过。
- 主已完成 bun run protocol:generate 与 protocol:check，均通过，协议仍为 2。
  浏览器 exporter 当前只导 Frame/SealedMessage/PairingInvite；daemon 内部的
  provenance 不因此扩展手机可指定字段。
- 服务重启后不重复上述检查，已续原唯一 code-reviewer 会话
  ses_ee3b88332ffeNDA678xVPXdJcW，只读审查明确的后端 WIP 范围；排除 GLM 在途
  supervisor/provider 验收 helper，禁止启动测试或清理进程。
- relay 原会话确认交付文件与 HOLD 完好，没有新行为验收。暂不重跑其历史轮，
  待安全 launcher 定稿、独立审查与验证后再对齐接线。

### GLM 周/月额度阻断：主接手隔离修复

- 原桌面 GLM 报告周/月上限至 2026-10-15 04:09:11。无文字完成不作为验收
  依据；主代理接回 supervisor 与 provider runner，保留全部在途改动，不重试
  或新建 GLM，不解除任何事故门禁。后端原 Step 复审继续。
- 接手读源码发现 stdio inheritable 三句柄虽已设置，但没有 HANDLE_LIST，
  仍不能排除继承宿主其它句柄；runner finally 先 kill 再等待可打断正常核验，
  超时抛出还可能跳过报告。这些失败先登记，修复与静态核验不代表运行安全已证明。

- 主已补 STARTUPINFOEXW/HANDLE_LIST 限定三个 stdio 句柄，修复 Pipes RAII
  成功/失败路径重复关闭；runner 改先等受控管理器自然收尾、超时再仅终止直属
  自有进程并将结果标失败。supervisor cargo check 与 runner node --check 均
  通过，仅静态检查，未运行任何隔离黑盒，门禁继续保留。

### 恢复推进：隔离运行器失败清单

### F13–F16 定向复审后的交接

### F17–F20 源码交付与复审范围

### 持久取消 F21–F25 交付后复核

### 第七批取消权威与终态交付

- 第七批完整复审撤回此前 host 崩溃中断 daemon 在飞请求的错误前提，未发现
  当前静态 P0/P1。submission 权威顺序、墓碑与 absence Unknown 已获源码确认，
  仍非行为证据。剩余主动恢复依赖手机重放、orphaned 查询失败丢项、多 C 按
  prompt 最新记录查询与终态三写边界继续收紧，不因评级 P3 就删除恢复要求。
- 原 Step 接续最小收尾：按原 C 精确查询、持久未结 C 自动恢复、失败保留待
  查询、终态事务边界。查询保持只读，不无条件取消正常在飞 Preparing 请求，
  不通过重新派发原 prompt 来触发恢复；不明副作用仍禁止自动重提。
- 观测模块新交付仍发现 ReadWorker 超时路径无界 join 与跨线程关闭使用中
  句柄的问题，原代理正在修明确 I/O 取消及有界退出，不集成或运行不可用接口。

- 作者交付 F26–F31：依据 submission 权威态收敛、恢复任意状态取消绑定、
  absence 改 Unknown、0007 独立无正文取消终态表、取消重放只查询；报告核心
  与 workspace tests 编译通过，零运行。全通过仅为静态编译，不是行为通过。
- 作者指出 host 单独崩溃不直接中断 daemon 在飞请求，修复定位到 daemon
  崩溃及实际提交状态。唯一 reviewer 复核此前假设，并重点核验正常在飞与
  query 收敛竞争、终态写入/清绑定/删行崩溃边界及原 C 精确匹配。
- start_remote_acceptance 新字段主此前已补并 check 通过，作者仍提示未补属
  过期交付信息；不重复修改。db/schema.ts/生成快照尚未对齐，未运行 db:push。
- 黑盒观测模块仍在修握手与通道闭合；未启动验收，不解除任何 HOLD。

- F21–F25 完整只读报告确认 0006 注册、取消绑定与恢复重链，已关闭此前纯
  内存取消主体。剩余派发中 pending 的终态恢复与 dispatched pending 恢复过滤
  仍需修复；行消失不能当 cancelled 证据，Accepted 只表示持久受理意图。
- 原后端 Step 继续核验真实可达崩溃序列：host 单独退出不当然中断 daemon
  在飞请求，需区分 daemon/host/双方崩溃与活请求。恢复/查询不能误终止正常
  派发，未知按权威证据落 Unknown，不重新驱动，取消终态须保留并重启可查。
- 仍只代码、文档、编译；功能验收门禁不解除，尚无本批行为通过证据。

- 原 Step 交付 daemon cancel 绑定、0006 迁移与 QueryRemoteCancel 查询驱动，
  报告核心编译通过；唯一 reviewer 正在核查 Accepted 前后与删行/终态落盘
  之间的崩溃窗口，不将仅有持久列当作完整恢复闭合。
- 主给既有 start_remote_acceptance helper 补 cancel_operation_id: None，实际
  cargo check --locked -p fintwind-remote-host --example start_remote_acceptance
  通过，三项未使用警告；仅编译、未执行。该 helper 不属于新 ownership 代理。
- 主检查 schema.ts 尚无 remote_prompts，Drizzle journal 仅到 0004；不盲跑
  db:generate，避免重新生成已有 0005/0006 SQL造成重复建表/加列。待本轮协议
  复审稳定后统一对齐手写迁移与生成快照，绝不 db:push 触数据库。

### 发布证据门禁补强

### 合成隔离黑盒交付：核心观察证据仍阻断

### 主代理接手后的实际合成运行

#### 自动推进：完整隔离矩阵与本地 provider 闭环（2026-10-10）

- 最新 provider 冻结轮：`C:/Users/Public/Temp/opencode/provider-chain-sget3j/summary.json`，
  8/8 pass，强制成功结算、精确取消隔离与所有清理字段通过；运行的是本轮复制
  的四个 exe 与三个合成 provider 文件，源码/runner 快照和哈希保留，结束复核。
- 后续手机入口调查：`fintwind-pwa/scripts/isolated-real-chain.mjs` 仍直接
  spawn daemon/host/relay/browser、直接喂配对 stdin，未通过已验证 supervisor
  容纳，事故门禁**未解除、未执行**。不能把当前 --local 的证据外推给它。
  下一工作是把整个手机浏览器驱动放进同一个受控 Job，并解决运行时配对批准
  的最小 IPC/句柄通道；随后再运行真实 WASM 配对/host 恢复，不运行旧 direct-spawn
  入口。旧 UI runner 同样须核对 Chrome 后代容纳后才跑。

- 后续已在当前源码重跑本地完整闭环：
  `C:/Users/Public/Temp/opencode/provider-chain-Jlvc6k/summary.json`，8 项 pass，
  turnSettled=true、containmentConfirmed=true。额外强制核验 exactCancelIdentityVerified。
- 三次独立 daemon 进程的真实 IPC 重启黑盒通过 prepare/recover/verify：
  `C:/Users/Public/Temp/opencode/queue-recovery-e1b6cbc947884127841660d977b304e7/summary.json`。
  同一隔离 DB：queued 原 submission/session/状态保留，不自动派发；取消墓碑
  重启仍可精确查询；未知取消、错误 target、另一 device、新 grant 均为 unknown；
  query 前后队列字节相等；恢复后取消再重启，不复活且终态保留。
  三轮均独占 Job，final 清理字段全部 true；没有启动 provider。
- 新行为黑盒实际发现并修复 queued cancel 没有保存 cancelOperationId/终态，
  删除 row 后只能 query unknown；现在先持久绑定，再按单事务写终态/删行。
- 另查出 live_dispatches 只有 Drop remove、从未 insert；已在 durable claim
  成功时加入，claim/insert 与 cancel 入口共享互斥边界，消除已 claim 尚未标
  live 的窗口。普通闭环和跨重启取消已验证；在飞 cancel/双方 crash 故障矩阵
  仍不计通过。F32-F36 host 恢复端仍需专门真实链路测试。

- 用户授权全自动推进；需要模型测试时仅用 GLM-5.3-Flash。本批主亲自运行，
  没有委派或调用真实模型，不触生产资料、不部署。
- 冻结完整所有权矩阵 15/15 pass、0 failed/blocked、cleanup.unverified=[]：
  `C:/Users/Public/Temp/opencode/ownership-frozen-3b756e37baf44077823d931f2da71cd7/matrix-report.json`。
  该目录含三个 exe、四个源码快照与 SHA256。assignmentFailure 注入仍明确未覆盖，
  不加故障开关或外层 Job 掩盖它，fullPlanVerified=false。
- crash 场景先等独立 observer 核验冻结拓扑、关闭 Job 副本，再由 observer
  暂扣最后 ACK 阻止正常 teardown 抢跑；runner 用自己创建句柄终止管理器。
  observer 持至少两个当时仍活着的进程真句柄、逐一核验退出，outsider 存活。
  crash 的 supervisor 创建句柄放入 Harness，panic 不丢所有权。
- 修复控制观察器 HANDLE_LIST 含不可继承句柄：用只读/等待权限的独立副本，
  显式继承并 RAII 关闭；修复 outsider-final 二次收尾覆盖顶层真实结果。
- 主检查隔离实现并独立跑完整矩阵后，PWA provider runner 从无条件事故阻断
  改成强制验收证据校验：全部 15 项、清理、outsider 与 supervisor SHA 必须匹配；
  没有 bypass 环境开关。只放行已接线的 --local，手机/真实链路门禁仍保留。
- 本地 provider 黑盒成功：
  `C:/Users/Public/Temp/opencode/provider-chain-KiFMQI/summary.json`。
  8 项通过，包括 trusted probe、创建/幂等受理/取消/派发、审批真实回复、
  单选/多选/自定义回答、归档、serve-only/无正文日志与成功任务结算。
  turnSettled=true、containmentConfirmed=true，所有 final 清理字段为 true。
- 实际链路修复：daemon 派发成功不再回内部 SubmissionSaved，而回远控
  RemotePromptState；只有 Dispatching/Accepted 回执才可标派发。
  取消终态事务先 BEGIN IMMEDIATE 预占写锁，避免 WAL 读后升级遇并发存储
  SQLITE_BUSY_SNAPSHOT，且使用 synchronous=FULL 持久取消结果。
- 合成 provider 修复：Windows execve 过早退出父进程导致 liveness 失真，
  改为保留父进程等待自己创建的子进程；历史与 idle 时间跟真实交互推进，
  不预先暴露完成记录。验收显式设 ask 策略并强制任务成功结算，不再容许
  turnSettled=false 的局部绿。此前失败冻结目录全部保留。
- 队列跨进程重启/取消恢复 F32-F36、手机配对链路、多设备、stress/WASM、
  公网与发布包仍未验收，不能将本地结果升级为全量完成。

- 主继续修复实际运行暴露的问题：WriteWorker 空闲 20ms 就退出；复制句柄
  SAME_ACCESS 导致权限未收窄；FixtureChannel 公告身份误带 fixture writer id；
  grant 返回裸句柄但局部 Channel 已析构；Job PID 变长数组按固定一项索引
  引发 observer panic；冻结 ACK 后仍等父进程保留副本导致永无 EOF。
- normal 的长期 ready root 不再私建未观测后代，后代由显式观察 fixture 创建
  并移交，observer 仍核验 Job 全体当前成员都有对应真实句柄，不降低判据。
- 新冻结 normal 实际通过：
  C:/Users/Public/Temp/opencode/ownership-frozen-f45f7b54276b4e4381a3148e7ef746a3/normal-report.json。
  scenario pass，0 failed/blocked，observer 五个真实进程归属/退出均验证，Job
  当前成员无未知项，Job 副本关闭、观察线程退出、outsider 存活均 verified。
  冻结三个 exe SHA 前后相同，未运行 daemon/provider 或 crash 场景。
- 报告顶层仍含旧设计 blocked 文案，不将场景 pass 升级为整个所有权验收。
  管理器崩溃路径与失败矩阵仍待跑，fullPlanVerified=false、provider HOLD 保留。

- 用户当前使用 OpenCode TUI，并要求主亲自推进；不再委派。主修在途 crash
  场景 Observation 类型错误与 join 内层 Err 判定，三个 examples build 通过。
- 冻结 normal 轮 ownership-frozen-6c125a061c4743238cb02244a55d0a6e 已产报告，
  无 panic，仍失败。实定位 fixture 消费 flag 后再次查找 flag 导致 exit 2，
  以及 JobQuery 发送晚于 RootChild，使观察者必然拒绝第一根进程。
- 主修 fixture 值消费、提前移交精确 Job 查询句柄后新冻结 normal 轮
  ownership-frozen-4c8c06951a0c4057b349755c6d5f9b20 仍失败，报告正常落盘，
  outsider 存活验证为 true，受测 final 容纳核验通过；观察握手仍未完成。
  不计 normal 通过，未运行 crash、daemon 或 provider，HOLD 保留。

- 观测模块交付后主接入前实读发现确定握手故障：read 等换行，但 handoff/ACK
  write 不带换行；正常消息也会超时。另有无换行缓冲无限增长、同步写绕截止、
  close_response 丢裸句柄不关闭、FixtureChannel 空处理等闭合缺口。
  已交原代理先修其模块/runner/fixture，主暂不接入不可用接口，不运行产物。
- 空 steps 拒绝已由主落盘并 check 通过，代理本轮仍称未落实是交付状态过期，
  已纠正；编译通过不证明握手或独立证据成立。功能 HOLD 保留。

- 纠正稿已去除命名 Job 和故障开关建议。主裁定空验收 steps 直接拒绝，已在
  supervisor 启动前校验；选择精确 Job 查询与真实进程句柄等待，不采用仅查询
  某个 Job 的弱判据。原 Step 开始只在其新文件中实现共享最小观察模块与
  runner/observer/fixture 接线，主等待精确模块接口后集成 supervisor。
- ACK 必须有独立回传通道；nonce 不是句柄，不纳入 HANDLE_LIST。观察者持有
  Job 副本时 kill-on-close 会被延迟，必须先关闭并确认再测试 crash，任何中途
  失败都不能靠 Job 名消失或根进程退出宣称后代已退出。仍禁止运行产物。

- Step 交付三个新增文件：process-ownership-plan.md、acceptance_ownership_blackbox.rs
  与 acceptance_ownership_fixture.rs，报告仅构建通过，零运行。场景表是实现
  判据，不是已通过覆盖；其约 3260 行 runner 仍缺受测子树独立同 Job/逐句柄
  退出证据，不能用于解除门禁。
- 主指出命名 Job 消失不能代替每个后代真实创建/移交句柄的退出等待证明；
  暂未接受 --job-name 或故障开关。原代理收敛最小受控句柄观测接口设计，必须
  先关闭观察者 Job 副本再验证 crash，并保留过程等待句柄与冻结合成拓扑证据。
- PWA runner 在同级 fintwind-pwa 仓库，交付中的不存在判断是路径错误；已要求
  更正。所有 WIP 保留，不重复启动代理、不运行产物，不解除 HOLD。

- 恢复推进后主读 prepare-pages.ps1：接受验收报告时检查根进程退出与 revoke，
  但未要求 descendantContainmentVerified，不能排除根退出但子孙未核验的报告。
  先记录此失败，再补显式 true 检查；缺字段同样拒绝，不修改实际验收判据。
- 此项仅发布证据校验，不执行打包、不上传、不运行受控进程或解除功能门禁。

- F17–F20 完整复审确认迁移和无 hash 重放拒绝的静态修复；同时确认 P1：取消
  意图未持久，recover 置 None 会让 queued prompt 在 host 重启后执行，或让
  dispatching 对应取消永远 Accepted。已交原后端 Step 补持久绑定及优先恢复。
- honour 先 take 后 RPC 错误丢意图也待修；不能机械放回后自动重发副作用。
  未知结果须持久隔离原 prompt、查询原操作真相，不能自动重新提交取消。
- 本地实际验收仍被此 P1 与进程隔离黑盒阻断。此前 1–3 小时仅估计，此新阻断
  意味着开始时间需在修复交付后重估，不据此缩减完整需求或提前解除门禁。

- 原后端 Step 交付旧 operations 库 content_hash 幂等迁移、旧无 hash prompt
  重放拒绝和 pending cancel 调度，报告两项 cargo check 通过；未运行测试或
  daemon，主未独立复跑，不能作为当前行为通过。
- 取消意图新增在 QueuedPrompt.cancel_requested，作者说明未新增 daemon 持久
  列，恢复时设 None。已交唯一 reviewer 定向复审：是否可由 SQLite 原取消
  operation 恢复已 Accepted 的取消，避免 host 崩溃后再次执行原 prompt；不能
  将 daemon 的 prompt 行存在等同取消意图持久。此项尚无闭合结论。
- 新黑盒仍只设计/编译，provider 和真实链路门禁均未解除。

### F13–F16 已完成的主端接入

- reviewer 静态确认并发建轮三态、精确 grant/旧行清扫、daemon 正文绑定和
  delete stopRequested 已落实，未做行为验收。新增阻断：host 旧 operations 表
  缺 content_hash 列迁移；另有 cancel 与 Busy 回 queued 的取消意图丢失窗口。
  已交原后端 Step 处理，不能删除旧去重库或靠手机无限重试。
- 主检查 PWA 发现 operations.ts 仍读 stopped 并宣称 stopped first，已改为
  stopRequested / stop requested，不把请求停止说成停止确认；现有整体 UI runner
  同步真实载荷与否定误导文案判据。tsc --noEmit 和 node --check 均通过，未
  运行 UI runner，也未重用旧 127-pass 作为当前证据。
- 原唯一 reviewer 已切到 supervisor/provider runner 的只读安全审查；不新增
  reviewer、不使用 GLM，功能验收门禁仍保留。

### 隔离运行器本批静态修复

- 唯一 reviewer 安全报告未发现静态 P0/P1，逐条核验 creation handle/Job/
  HANDLE_LIST/总截止/最终五项退出证明；仅静态结论，不能移除 HOLD。
- 主补 supervisor 本身环境白名单，processStart 缺明确 env 即拒绝，避免继承
  整个 process.env。node --check 通过，provider 门禁仍在。
- 主回读 seed_remote_workspace.rs:74–78 确认默认 seed 输出仅 synthetic 与两
  UUID，不含 daemon token；README 区分原始 stdout/stderr 不落盘与结构化 seed
  文件落盘。plan 仍含本轮合成认证 token，不含真实模型凭据，未运行该 runner。
- 已安排 Step 独立设计纯合成 process-ownership 黑盒，只允许新文件与编译，
  不允许运行。独立证据必须证明同一 Job 与真实子孙句柄退出；不靠缺 heartbeat
  或根进程退出宣称整树已退出。若受测程序缺观察接口，先交明确阻断不虚构证据。

- 用户要求继续，后续不使用 GLM。原 reviewer 定向核查 F13–F16 落盘修复，
  最后一次无文字交付不计为检查通过。
- 修改前读到 supervisor 三处失败：无 ready 的 spawn 不排空 stdout；ready 后
  超过累计 64 KiB 会关闭 stdout，可能打断子进程；Assign/Resume 失败的退出
  等待结果不进入最终 containmentVerified，存在失败清理仍被标已核验的风险。
- 本批先修上述静态路径，保留功能验收门禁。后台管道线程收尾、共享退出截止
  与合成进程隔离矩阵尚需完整审查，不因为一次编译通过就恢复验收。
- 主已修上述三处，并将管道线程保存在本轮句柄集合中：Job/根进程退出后，
  使用同一清理截止核验线程结束，未结束或 panic 均不确认容纳。ChildGuard
  Drop 不再为每个未退出根追加五秒等待；最终报告新增 startupCleanupVerified
  和 ioWorkersExited。最新 cargo check --locked -p fintwind-remote-host --example
  acceptance_supervisor 通过，仍有五项未使用代码警告。未运行任何验收产物。

### 第四批复审结论与定向修复

- 唯一 reviewer 的完整只读报告确认此前锁重入、设备来源 nil、operationId
  回显、createSession 未知字段和过期队首已从源码修复；busy 检查不把未建轮
  queued 输入算成忙，SQLite 去重存在。所有确认均非运行期验收。
- 新发现：dispatch 用 grant_version < stored，不能拦重配对 V2 对 V1 旧行；
  admit 快路径仅比摘要、不比正文。cancel 缺 grant 版本校验也需对齐。已交原
  后端 Step 先列失败矩阵再修，特别保留撤销清扫旧行的安全能力，不能机械加
  版本相等导致清扫永远失败。后台废弃与设备主动执行须分清授权边界。
- archive/options 未使用 provenance 审计维度被 reviewer 标为设计局限，暂不
  扩大审计需求；归档仅发 cancel 的时序风险与真实任务停止仍需行为验收，不能
  将已请求停止包装为停止确认。本批不启动验收、provider 或清理进程。

### GLM 收尾批：F32–F36 闭合与 supervisor subject 侧接线（2026-10-10）

**本批只做代码、文档与静态编译；零运行、零进程创建、门禁全保留、
`fullPlanVerified=false`。**

取消恢复（F32–F36，全部仅静态实现）：

- **F32**：`QueryRemoteCancel` 增 `cancel_operation_id`（protocol 上一批已有
  字段，daemon match 臂此前缺构造导致 E0027）。给出该 id 时按
  `cancel_term_by_id`（cancel id + prompt id 双条件）精确查，并校验终态记录
  属于该 prompt 与询问者（device/host/grant 精确相等，重配后旧 C 不得被新
  grant 读走）；行上绑定正是该 id 则回 pending；无绑定无终态回 `unknown`，
  绝不推断 cancelled。缺省维持按 prompt 最新终态的旧语义。查询仍纯只读。
- **F33**：host dedup `operations` 表增 `cancel_target` 列。迁移泛化为
  `ensure_columns`（同一 `IMMEDIATE` 事务内锁前/锁后双探针，content_hash 与
  cancel_target 共用）；`run_cancel_queued` 受理后即绑定目标，绑定失败按
  Unknown 回答，不留"接受了却没有恢复锚"的记录；daemon 连接恢复时扫描
  `kind='CancelQueuedPrompt' AND state='accepted' AND cancel_target IS NOT
  NULL` 主动进恢复队列——设备不再需要重放取消才能知道结局。orphaned 结构
  升级为带 `device_id` 的 `OrphanedCancel`，恢复时 device 取自行/记录。
- **F34**：`resolve_orphaned_cancels` 改为**先查后取**：peek 全集、逐条用
  `QueryRemoteCancel{cancel_operation_id: Some(该 C)}` 精确查询，只有拿到
  确定性终态并投递后才从集合移除；查询失败、绑定不符、仍 pending 一律保留
  条目下一 tick 只重查。原"先 drain 后查"丢项路径删除。
  `honour_pending_cancel` 与 `resolve_replayed_cancel` 同步改为按 C 精确查询。
- **F35**：daemon 增 `live_dispatches` 内存集合 + `LiveDispatchGuard`（RAII，
  CAS claim 成功后插入、函数任何出口包括所有 `?` 都移除）。dispatch 收敛点
  （原 daemon.rs:756 的 cancel_intent 检查）现要求 operation **不在** live 集才
  按 submission 权威收敛；在飞则原样回报行状态。拒绝"query pending 即取消
  Preparing"的无条件建议——那正是取消活请求的路径。
- **F36**：新增 `settle_cancel_and_delete_row`（读行+绑定 → 写终态 → 删行，
  单事务）与 `settle_cancel_term_and_clear`（读绑定 → 写终态 → 清绑定不删行，
  单事务）。`converge_pending_cancel` 与 dispatch 成功路径的 `settle_cancel_term`
  改用它们；submission settle（abandon）保持在事务之前——submission 权威
  先于 cancel 兑现。dispatch 失败出口新增 `delete_prompt_settling_cancel`：
  按 abandon 后的 submission 真值收敛绑定（NotSent/Rejected→cancelled，
  Dispatching/Accepted→dispatched，其余→unknown），在飞期间被绑定的 C 不再
  随行删除而消失。旧的三个独立方法（`record_cancel_term`、
  `resolve_cancel_intent`、`ensure_content_hash_column` wrapper）已无调用者，
  随之删除。
- **db 对齐**：`db/schema.ts` 补 `sessions.archived`、`remote_prompts`（含
  0006 两列）、`remote_cancel_terms`；手写 `meta/0005/0006/0007_snapshot.json`
  与 `_journal.json` 条目（id/prevId 链）。**0005/0006/0007 三个 SQL 文件一字
  未动**，未跑 db:push。核验：`bun run db:generate` 输出 "No schema changes,
  nothing to migrate"——生成器与手写迁移对齐、无重复、不产生新文件。Rust
  runner 只按文件名序应用 SQL、从不读 journal，运行时行为不变。备份在
  `C:/Users/Public/Temp/opencode/db-migrations-backup-20261010`。
- `start_remote_acceptance` 的 `cancel_operation_id: None` 主代理此前已补，
  本批未重复修改。

进程隔离前置（观测模块收尾 + supervisor subject 侧）：

- 观测模块上一 Step 的 partial 回读确认：IO worker 已重写为 `CreateThread`
  真句柄 + 两阶段有界 join + `progress` 传 `Result`。本批补齐三处：
  **`CancelSynchronousIo`**（持真实 worker 线程句柄，绝不 OpenThread 补认领；
  位于 `Win32_System_IO`，按批准最小加入 manifest features）——worker 卡在
  同步写时先取消、验证线程真正退出后才 close；**`WorkerThread::drop`**——
  spawn 后早退路径不再漏线程句柄（只关线程句柄，绝不关线程正在用的 I/O
  句柄）；**两处 `LineReader::spawn`/`WriteWorker::new` 调用点参数顺序颠倒**
  与 `ObserverHub::register` 吞 Result——examples 此前**编译不过**（不在
  workspace check 范围内被掩盖），均已修复，register 改为 fail-closed。
  "附加 2 秒超共享 deadline"确认已不存在；handoff deadline 用共享 total
  deadline。
- **supervisor subject 侧接线**：观测参数全或无解析（缺席=行为与从前逐字节
  一致）；通道打不开 = 该轮 observation 失败退出，不静默降级；每个 root
  child 在 assign 后 resume 前 `duplicate_for_observer`（掩码在模块内强制）+
  `Kind::RootChild` handoff，ACK 非 `Ok(true)` 即 kill+有界等待+**主动
  `TerminateJobObject`**（observer 可能持 job 副本，不能靠 kill-on-close）+
  按启动失败上报；冻结序列（`JobQuery`→`TopologyFrozen`）只发一次、在
  teardown 前、deadline 用共享 total deadline；teardown 后 channel worker
  同一 cleanup deadline 内收割，收割失败该轮不算绿；final 事件携带
  `observation: integrated|absent|failed|unreaped`。
- **精确阻断（不实现、不假装）**：`Kind::FixtureChannel` 注入没有 plan 契约——
  plan schema（deny_unknown_fields）无"观测 fixture step"字段、runner 场景
  组装亦无此 step；两侧单独实现任一半都是对方不认领的协议。Descendant 相关
  判据保持 notCovered。已写入 `process-ownership-plan.md` 第 2/2.3/8 节。
- PWA runner 只读核验：顶层 incident `throw` 仍在；`processStart` 无显式 env
  即拒绝；teardown 先等 supervisor 自然退出（15s），超时才对本轮
  ChildProcess 引用 `kill()`，无任何扫描。本批未修改 PWA 仓库任何文件。

编译证据（全部 `--locked`，仅静态检查，不是行为通过）：

- `cargo check --locked -p fintwind-core -p fintwind-remote-host` — 通过。
- `cargo check --locked --workspace` — 通过（剩余 warning 均为 relay 预存，
  本批未触碰 relay；core/host 的 dead-code 警告已随本批清理归零）。
- `cargo check --locked -p fintwind-remote-host --tests` — 通过。
- `cargo check --locked -p fintwind-remote-host --example
  acceptance_ownership_blackbox --example acceptance_ownership_fixture
  --example acceptance_supervisor` — 通过（supervisor 仍有 4 个上一批既有
  未用 helper 警告，非本批引入）。
- `bun run db:generate` — "No schema changes, nothing to migrate"（对齐证明）。

仍未验证（诚实清单）：全部 F32–F36 行为、观测 handoff 真实握手、
`subjectIntegrated` 三判据、supervisor crash 后代退出证明、公网/真实链路。
真实验收前置：独立数据目录 + 环境白名单 + 受控 Job 容纳边界确认后，按
`remote-control-acceptance-steps.md` 由 main 解除对应门禁后运行。

### GLM 续批：FixtureChannel 接线闭合（2026-10-10，第二轮）

**仍只代码/文档/静态编译；零运行、门禁全保留、`fullPlanVerified=false`。
本轮 owns supervisor/shared observation/ownership runner/fixture 及文档；
后端 F32–F36 已在上一小节交付，未重做、未扩大 scope。**

- **plan 契约落地**：`Step.observeDescendants: bool`（`#[serde(default)]`，
  `deny_unknown_fields` 不变）。缺省 `false` = 行为与无观测逐字节一致（仅
  3 stdio 进 `HANDLE_LIST`，env 无任何观测键）。prevalidation 拒绝该标志与
  `mode != "run"` 组合——run step 的 verified exit 就是"后代 hand-off 已
  完成"的同步点，不靠 sleep 猜，无需额外 ready/frozen 握手。
- **supervisor 授权顺序**：`Channel::create` → subject 端标 inheritable →
  observer 两端经 `duplicate_pipe_end_for_observer`（同一唯一移交点）→
  `Kind::FixtureChannel` handoff → observer ACK 注册 → 才创建 fixture 进程 →
  assign 进 job → `Kind::RootChild` 真实创建句柄 handoff → ACK →
  `ResumeThread`。grant/任何 handoff/ACK 失败 ⇒ 不 Resume、显式
  `TerminateJobObject` 自己的 job、有界清理——有 observer 副本在时不依赖
  kill-on-close。
- **Handoff 显式管道字段**：新增 `pipeRequestRead`/`pipeResponseWrite`
  （`#[serde(default)]`）。管道端不再冒充 `process_handle`/`job_handle`——
  管道没有 pid，`GetProcessId` 来源认证保持只属于真实进程句柄；observer 收到
  FixtureChannel 时显式拒绝 pipe 字段之外夹带 process/job 句柄的公告。
  三个 `Handoff` 构造点（supervisor offer、FixtureChannel、fixture
  Descendant）同步补字段。
- **fixture id 单调唯一**：supervisor 内计数器从 1 起，`WriterId::fixture(id)`
  结构唯一。
- **继承与环境**：观测 step 的 `HANDLE_LIST` = 3 stdio + observer 句柄 +
  该 fixture 的 request_write/response_read（`create_suspended` 增
  `extra_inherit` 参数，非观测 step 传空 = 继承契约不变）。值经本 step env
  五键 `FINTWIND_OBSERVATION_{HANDLE,NONCE,REQUEST,RESPONSE,FIXTURE_ID}`
  下发；nonce 是令牌非句柄、从不进 `HANDLE_LIST`。fixture 五键**部分**存在
  即退出码 2（broken grant fail-closed），五键齐全才启用，无 grant 照常运行；
  旧的 `--observation-*` argv 路径删除（plan 冻结，supervisor 只能经 env
  按步注入）。非观测 step 的 env/继承不含任何观测内容。
- **诚实失败传播**：fixture 的 `Descendant` handoff 未被接受 ⇒ step 退出码 5
  失败——已验证的 exit 0 永远不替没有发生的 hand-off 背书；supervisor 按普通
  step 失败处理并走既有 terminate/清理路径。
- **runner**：`scenario_normal` 与 `scenario_supervisor_crash` 的 plan 均声明
  `observeDescendants: true` 的可信 fixture step（`--grandchild`，建真实后代
  并 dup 移交、ACK 后不再 CreateProcess）；normal 的 step 事件计数 3→4；
  原"supervisor 无观测接口"的 `not_covered("supervisorTree.sameJob.
  handleVerified")` 删除（该判据现由 observer 逐真实句柄检查承担）。
- **文档矛盾修正**：`process-ownership-plan.md` 文件头归属（supervisor 等
  四文件自本批起由实施代理拥有，PWA runner 仍 main 所有只读）、第 1.3 节
  IO worker 描述（旧"内联 extern/超限关句柄"文本与现行官方
  `Win32_System_IO` + `CancelSynchronousIo` + worker 持有 I/O 句柄的实现
  矛盾，已按代码重写）、第 2 节契约全文、第 3 节 17/19 行与
  subjectIntegrated 说明、第 4 节结论、第 7 节编译验证——均按当前源码状态
  改写，不以 mtime 或旧段落为凭。`remote-control-batch8-failure-matrix.md`
  头部加 F32–F36 实现状态标注。
- 未做：不改后端/relay/PWA 任何文件；不运行任何产物；`assignment-failure`
  等不可注入类保持 NotCovered，未造故障旁路；未删任何其它 WIP。

编译证据（`--locked`，仅静态）：

- `cargo check --locked -p fintwind-remote-host --example
  acceptance_ownership_blackbox --example acceptance_ownership_fixture
  --example acceptance_supervisor` → 通过（4 个早期批次既有未用 helper
  警告，非本轮引入）。
- workspace/core/host/tests 在上一小节已列，本轮未改那些文件、无需重跑；
  examples 三目标在本轮修改后单独核验如上。

真实运行（未执行，main 依门禁决定）：先纯合成 normal + supervisor-crash
两场景——独立目录、环境白名单、受控 job 边界、runner 只终止本轮 supervisor
创建句柄并逐后代真实句柄有界等待；全 verified 前不触碰 provider 链路，
也不以桌面 release 构建为前置。

### GLM 诊断批：normal 首跑 panic 的源码修复（2026-10-10，第三轮）

**main 首次真实纯合成 normal 失败**（runner main 线程 panic
`failed to join on thread: 句柄无效 (os error 6)`，exit 1，normal-report 未
生成；冻结目录
`C:/Users/Public/Temp/opencode/ownership-frozen-ba11ec83e1124cc29941e966aaa6e840`
保留）。main 命令立即停止全部 runtime；本批只源码诊断/修复/编译，零运行。

- **确定缺陷已修**：`start_observation` 在 spawn observer 后立即
  `observer.join_io()`——observer 的 stdio drain 到 EOF 才结束，而 EOF 要等
  observer 退出（其 observe 循环跑满 deadline）；这既是 join 生命周期混用
  （main 提示方向之一），也让 observer 在 supervisor 运行前就被 join 到
  退出、必然收不到任何 hand-off。已删除；drains 统一由
  `Observation::finish`（observer 有界 wait 之后）收割。
- **panic 防御与失败可观测**：部分 Rust 版本的
  `std::thread::JoinHandle::join` 在内核等待失败时 **panic 而非返回 Err**
  （panic 原文与本次一致），unwind 出 runner 会跳过全部清理与 report——
  这解释了"panic 可见于终端而 report 不存在"。修复两层：
  `Child::join_io` 逐 drain 以 `catch_unwind` 包住 join，reaped 失败如实
  记入证据链（supervisor drains → `cleanup.unverified`；observer drains →
  `observation.observerIoReaped` 判据）；`runner_main` 主体包
  `catch_unwind`——panic 分支显式 `finish_outsider`（本轮独占 job 终止 +
  有界等待，句柄仍在本轮手中）并写出 `outcome:"panic"`、
  `fullPlanVerified:false`、`cleanup.unverified` 的**失败** report 后 exit 1。
  panic 由 RAII/kill-on-close 收束的树一律标注 UNVERIFIED，**不宣干净、不
  认领、不扫描**。
- **提示路径排查（如实）**：`as_raw_handle` 误 close 线程原生句柄、
  `WorkerThread`（CreateThread）句柄与 std JoinHandle 混用——两条均经全文
  grep 逐点排除，现源码中不存在；`close()` 全部调用点逐一核对，无一触碰
  std drain 句柄。**单行根因未唯一锁定**；若修复后重跑再现，新 report 的
  `panicMessage` 原文（现在必然落盘）即下一步取证入口。
- 其余：`start_observation` 过时注释（"supervisor 未实现 subject 侧"）更新；
  `process-ownership-plan.md` 第 7 节补 main 首跑记录与本批修复。未触碰
  后端/relay/PWA；未运行任何产物；HOLD 与冻结证据全保留。

编译证据（`--locked`，仅静态）：`cargo check --locked -p fintwind-remote-host
--example acceptance_ownership_blackbox --example acceptance_ownership_fixture
--example acceptance_supervisor` → 通过，0 error；supervisor 4 个早期批次
既有未用 helper 警告不变。main 重跑前需以当前源码重新 build 并记录新 sha
（旧冻结 exe 不含本批修复，不得复用其结果定性）。

### 主代理继续：候选构建与浏览器句柄边界（2026-10-10）

- 用户要求继续下一步；本轮不委派、不审查、不提交、不推送、不部署。
- 保留开始时 `crates/fintwind-core/src/persistence.rs` 的两处已有改动。
- 配对驱动已有内存中的验证码/指纹核对和 `stdin.end("y\n")`，无需为批准
  添加生产自动批准开关。但“Node 驱动整体放进 Job”目前只有容纳方案，
  **不等于**独立 observer 已取得 Chrome 每个后代的真实进程句柄。
  依据 `scripts/fixtures/remote-control/process-ownership-plan.md` 第 34–37 行，
  不能用 `IsProcessInJob(handle, NULL)` 或最终 Job 计数为零替代这一证据。
- 同级 PWA 的 `scripts/isolated-real-chain.mjs` 继续阻断所有真实链路模式。
  仅允许 `--build-snapshot <绝对路径>` 的构建准备，不启动 daemon、host、
  relay、provider 或浏览器。修复 Cargo 不支持的 `--debug` 参数。
- 原生构建快照新增内嵌依赖：`locales/`、`models_thinking_modes.json`、
  `db/migrations/`、`resources/opencode-browser-plugin.ts`；构建前后也检查
  新增/删除文件，不再仅比较原有文件。后续验收要求快照的输入集合完整。
- 构建准备新增独占冻结目录：复制四个原生产物、全部输入与构建脚本，
  逐文件复核 SHA-256；摘要以 `wx` 写入，不能覆盖已有证据。
  失败冻结目录保留，不把缺少摘要的目录作为成功证据。
- `scripts/build-wasm.mjs` 的来源证明补入协议内嵌的语言文件和思考模式表，
  并记录相关 crate 内的全部文件，而非仅 `.rs`/`.toml`。
- 两个脚本的 `node --check` 通过；真实链路入口实际以阻断错误退出，
  未启动验收应用。本轮首次原生/WASM 构建在来源集合补全前已启动，
  因此即使成功，也不能代替修改后版本的候选快照，需要随后重建。
- 手机真实 WASM 配对、Chrome 后代句柄交接、F32–F36 与公网链路仍未验证；
  `fullPlanVerified=false`。没有放宽原有事故隔离标准。

本轮候选构建收尾（均非运行时链路验收）：

- 修改后原生快照通过：4 个 debug 产物、182 项来源；摘要
  `C:/Users/Public/Temp/opencode/remote-native-snapshot-20261010-complete-inputs.json`，
  其中记录独占冻结目录、四个 exe 路径、来源与构建器 SHA-256。
- `node scripts/build-wasm.mjs` 以补全后的来源集合重跑通过，协议仍为 2；
  保留已有的 `unused_mut` 与 wasm-pack 可选元数据提示，不称为零警告构建。
- `node node_modules/typescript/bin/tsc -b` 和
  `node scripts/generate-remote-contract.mjs --check` 通过。
- 私有候选页面通过 Vite 生产构建与 Workbox 生成，未替换仓库 `dist/`：
  `C:/Users/Public/Temp/opencode/remote-candidate-h3PWxX/site/`。
  真实检查原生冻结产物/来源、WASM 输出/来源、PWA 构建前后来源及每项
  页面资产 SHA-256 后写出 `summary.json`，`passed=true` 仅指构建检查；
  `runtimeChainVerified=false`、`publicChainVerified=false`、
  `fullPlanVerified=false` 均保留。
- 可重复构建运行器为
  `C:/Users/Public/Temp/opencode/prepare-remote-site-20261010.mjs`；每次运行
  创建新目录，已有目录和失败证据不覆盖，冻结的运行器也随候选保存。
  命令：`node C:/Users/Public/Temp/opencode/prepare-remote-site-20261010.mjs`。
- Vite 报告 `crypto-bridge.ts` 同时静态/动态导入，不能拆到独立 chunk；
  这不是配对失败或行为验收通过的证据，本轮不扩大为打包优化改造。

### 主代理继续：真实后代句柄来源（2026-10-10）

- 根据 Microsoft 的 `DEBUG_PROCESS` / `WaitForDebugEvent` 文档，调试创建
  事件提供真实进程句柄，无需 `OpenProcess(pid)`。已先记录失败清单：
  `docs/remote-control-browser-ownership.md`，随后实现
  `examples/support/acceptance_debug_tree.rs` 和纯合成
  `examples/acceptance_debug_tree_blackbox.rs`（均在 remote-host crate）。
- 模块限制事件泵在创建线程；复制为查询+等待权限；进程必须属于确切
  Job；独立交接完成前不自动继续事件；不读取进程内存/正文；不吞掉业务
  异常；文件句柄与系统托管进程/线程句柄分开处理。
- 已在 supervisor 中仅加入编译入口，未更改其创建标志/等待/清理流程，
  未接真实浏览器驱动。Cargo 新增 `Win32_System_Diagnostics_Debug` feature。
  源码与构建输入已变化，上一候选只代表此前冻结代码，最终发布必须重建；
  此前 15/15 冻结 supervisor exe 的证据仍只约束其原哈希。
- 合成双进程黑盒首跑因零时限等待而失败，失败目录完整保留；改用有界
  等待后重复两轮通过。每轮持有两个事件来源的真实进程句柄，核验确切
  Job、两个退出事件、所有句柄终态和 Job 归零。证据完整路径与重复命令
  见上述浏览器归属文档。
- 仅该合成正常清理路径通过，不外推 Chrome、超时、observer 或 crash；
  `fullPlanVerified=false`。未启动浏览器/daemon/host/relay/provider，
  未委派、审查、提交、推送或部署。接下来接独立 observer 和合成故障矩阵。

### 主代理自动推进：独立 observer、Chromium、手机只读链路（2026-10-10）

- 用户要求“主动进行直到需要我操作”；主代理继续，不委派、不审查、不
  提交或部署，不使用真实模型密钥。开始时既有 persistence.rs 两处改动保留。
- supervisor 的 `debugDescendants` 接线已完成；仅允许独立 observer 下
  的最后一个 run 步骤；每个内核事件提供的真实后代句柄先交接、ACK 后
  才继续。结束时先终止本轮 Job，泵退出事件并等真实句柄，再冻结拓扑。
- 完整合成矩阵 19/19 pass（含原 15 项、调试树正常/exit-3/期限/调试器
  crash）。证据与失败保留记录：`docs/remote-control-browser-ownership.md`。
  最新冻结矩阵：
  `C:/Users/Public/Temp/opencode/ownership-debug-frozen-17a29d9b0dba4a5abe2680e19f6f9214/matrix-report.json`。
- sandbox 未关闭的测试版 Chromium 空白页两轮通过，14 个真实句柄交接
  并逐一等待，旁观者存活，Job 归零；安装版 Chrome 仍 CDP connect 超时，
  不把其清理通过外推为页面通过。后续固定使用已核验测试版路径与哈希。
- 新内部驱动：同级 PWA 的 `scripts/browser-ownership-probe.mjs`（创建管道
  CDP、connect/disconnect、无库 launch/close 回退）及
  `scripts/owned-real-chain-driver.mjs`（仅从受控调试计划启动）。旧
  `isolated-real-chain.mjs` direct-spawn 路径仍保留阻断，不提供 bypass。
- 运行器先核验完整 19 项与真实 Chromium 证据及 supervisor 哈希，再
  使用冻结原生 exe、逐资产核验的私有候选页面、隔离身份/profile/数据目录。
  Node 启动只负责受控作业内后代，所有终止交给 Job；无 PID 打开/认领。
- 本地 CSP 不允许随机 relay 端口，初轮配对请求在浏览器内被阻断。
  未放宽生产 CSP/未占固定端口；驱动加同源转发至 relay 自报并核验的
  随机 loopback 端口。页面字节不变，报告显式记载此转发方式，公网跨域
  仍未验证。失败目录逐轮保留。
- 6 项基础链路通过：daemon 合成 seed、真实 WASM XX 与代码/指纹比较、
  本机 stdin 批准、会话 attach、持久 rename、刷新身份以及撤销后旧通道
  的只读 attach 被拒（基础报告将 seed/配对/attach/rename/reload/revoke 分列）。
  空闲 relay socket 没有授权语义；不要求撤销后无业务请求时主动关闭全局
  设备 socket，以免影响此设备其它 host。现测试通过旧通道只读请求验证拒绝。
- 已修复真实手机分页缺陷：`src/components/WorkspacePanel.tsx` 和
  `src/api/session.ts` 将文件/目录/diff revision 从 number 改为 string，
  More 请求传递当前响应 revision；后端首屏校验与变更拒绝均不放宽。
  新私有候选：`C:/Users/Public/Temp/opencode/remote-candidate-XN6Nxb/`。
- 最新真实链路 **11 项通过**：基础六项 + 文件/目录分页、变更 revision
  与 binary 拒绝、Git diff 分页、业务缓存检查。读取前后合成工作区正文
  哈希一致；只保存检查/计数/资产哈希，不保存配对 nonce/code/private key。
  `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-4IRaqU/summary.json`：
  `passed=true`、`containmentVerified=true`、`inputsUnchanged=true`；
  `fullPlanVerified=false`、`publicChainVerified=false`。
- 可重复命令：`node C:/Users/Public/Temp/opencode/run-owned-real-chain-20261010.mjs`
  （运行器及两个内部驱动随每轮冻结保存）。原生来源冻结摘要：
  `C:/Users/Public/Temp/opencode/remote-native-snapshot-20261010-owned-chain.json`。
- 仍未覆盖：手机任务执行/审批/问题/在飞取消，手机恶意路径 wire、任务
  output、跨设备/跨 host 和公网跨域。19 项矩阵未合法注入 assignmentFailure，
  该项仍 NotCovered，不因 summary blocked=0 称其已覆盖。

### 手机任务执行、审批、问题与取消后续（2026-10-10）

- 后续发现并修复三个真实路径缺口：`main.rs` 的普通发送统一使用持久
  admission/queue/dispatch（不再要求空闲新任务已有 runtime）；已持久化
  但未启动的任务也保留打开视图与已核验空 transcript，首轮派发后升级
  到 daemon 返回的真实 runtime 并建立事件泵；create/rename/archive/
  restore/delete/fork/rewind 成功后向在线设备推送权威会话列表。
- `events.rs` 的空 transcript 只用于 LoadTaskState 确认存在且没有 native
  session 的任务，不能把 provider 查询失败当空内容。nil runtime 只是
  未启动视图哨兵，不订阅或 unsubscribe 伪造 runtime。
- PWA `src/api/session.ts` 关闭视图时结束该视图尚未完成的 attach 等待，
  防止旧等待阻止打开另一任务；收到该 session 的 turnStarted/turnFinished
  等生命周期边界时清除旧 offers，不影响其它 session 的 offers。
- 合成 provider 不使用真实模型。问询政策是本机设置，不算手机设置：
  `seed_remote_workspace --ask-session UUID ...` 仅对标记工作区内未启动
  任务写入 Ask，拒绝未知/已启动/外部项目。手机本身没有政策绕过入口。
- 驱动新建后等待两条会话并用 daemon.inspect 返回的新任务标题选择，
  而不是把尚未刷新列表中的旧任务误作新任务。早期失败曾混有此测试
  选择问题；失败目录保留，不以其单独证明新任务行为。
- `synthetic_opencode.py` 新增可选 expected_reply：实际 provider 收到的
  once、单选、多选与自定义文本逐项匹配，只记录 matched 布尔值，不记
  答案正文。负向场景故意设不匹配期望，要求流程失败且不发问题/成功终态。
  新增 interruptible 行为：停止等待回复，发真实 interrupted + idle，不
  继续预设成功脚本；保持 SSE 开放，避免测试服务器重连重复发布旧请求。
- 当前正常链路 **14 项**通过：前述只读能力 + 手机创建任务、首次发送、
  等待审批期间排队并精确取消、once 批准、单选/多选/custom 回复、成功
  终态与刷新身份/撤销旧通道。provider prompt 计数始终为 1，取消的
  队列项未运行，当前审批不受队列取消影响。
- 独立中断场景通过：不回答批准卡而点击 Stop，provider 收到一次 interrupt、
  一次 interrupted 终态，没有批准回复或成功终态；手机停止运行状态，
  旧审批卡不可继续操作。初轮发现旧卡仍可点击，以上 PWA 清理修复后通过。
- 最新候选页面：`C:/Users/Public/Temp/opencode/remote-candidate-QL2Htz/`。
  最新原生冻结：`C:/Users/Public/Temp/opencode/remote-native-snapshot-20261010-owned-chain-queue.json`。
  当前状态的独立证据（均 passed/containmentVerified/inputsUnchanged=true）：
  - 正常：`C:/Users/Public/Temp/opencode/owned-real-chain-frozen-slqTNs/summary.json`
  - 中断：`C:/Users/Public/Temp/opencode/owned-real-chain-frozen-V7AwNe/summary.json`
  - 不匹配回复负向：`C:/Users/Public/Temp/opencode/owned-real-chain-frozen-vdJv1t/summary.json`
- 可重复命令：`node C:/Users/Public/Temp/opencode/run-owned-real-chain-20261010.mjs`；
  中断加 `--cancel-running`，负向加 `--expect-reply-rejection`，各自独立目录。
  负向 passed 表示预期拒绝与清理得到验证，绝不是其业务任务成功。
- 既有 daemon 合成整体黑盒重验 8/8：
  `C:/Users/Public/Temp/opencode/provider-chain-VF5Doc/summary.json`（早于随后
  interruptible fixture 改动，因此不当作最新 fixture 全部行为的证据）。
- 计数更正：上节早期“11 项”实际报告列出 10 项，任务三项新增前的
  口头“14 项”实际为 13 项；以各轮 chain.json 检查列表为准。当前正常
  slqTNs 报告确为 14 项，不累加重复轮次。
- 仍待：手机/host/daemon 重启与双方崩溃、精确 C 查询恢复及查询失败保留、
  自动恢复 F32–F36、多设备批准竞争/双 host、手机恶意路径 wire、任务
  output、stress/WASM 安全与公网跨域；fullPlanVerified/publicChainVerified
  始终 false。不提交、push 或部署；用户此时仍无需操作。

### 双设备、任务输出与 host 安全重启（2026-10-10，续）

- 两个隔离 browser context 使用不同 device 身份配对同一 host；两端同时
  收到批准，竞争 once 只产生一次 provider permission reply。撤销第一端
  后，第二端 grant 未撤销且继续读取合成文件。此证据不覆盖同设备双 host。
- 手机刷新期间保留 queued 提示：刷新后重新 attach，恢复队列和批准卡，
  精确取消 queued 项；最终 provider 只有一次 prompt，不重复发送。
- 输出检查发现 HydrateSession 的本地消息仍只有用户提示；started task
  改为通过 trusted session_coordinates + FetchNativeTranscript 读取权威
  provider 消息，保持输出上限，失败不回退成旧内容，不记录正文。
- daemon `fetch_native_transcript` 原本只允许冷恢复，驻留 runtime 会报
  `native history read raced an attached runtime`。增加已连接 runtime 的
  纯只读返回：I/O 前记录 runtime UUID，I/O 后核验同一 runtime 与 native
  绑定，变更则拒绝，不进入冷历史 receipt/turn 写回。原离线恢复校验保留。
- 双设备真实链路现覆盖 Output 返回用户提示与助手成功结果，取消队列正文
  不出现在输出。初轮输出失败证据保留，不以正文流已成功替代 Output 验收。
- 新 `scripts/owned-lifetime-parent.mjs` 只在受控 Job 内等待自己的 stdin
  EOF 后自然退出；host 使用现有 parent watchdog 自行结束，再由同一 Job
  内驱动创建新 host。没有 kill/PID 打开/清理扫描，明确是**安全退出重启**，
  不是 crash；独立 observer 核验两代进程句柄和确切 Job。
- 首轮 host 重启卡在列表：恢复的 queued 项在手机尚未 attach 的窗口被
  当“ownerless”失效；drop_session 迭代持锁又进入 take_cancel_request，
  重复加锁导致 worker 卡住。现在 ownerless 仅等待重新 attach，不等同
  任务删除；归档 drop 在释放队列锁后逐项结算。重新 attach 也显式推送
  queue view 和仍有效 offers，避免首次 snapshot 先于视图建立时漏恢复。
- 当前原生冻结：
  `C:/Users/Public/Temp/opencode/remote-native-snapshot-20261010-host-attach.json`。
  当前页面候选：`C:/Users/Public/Temp/opencode/remote-candidate-arYmUL/`。
  最新对应代码的真实链路证据（均通过资产稳定性与独立容纳检查）：
  - host 安全重启、队列恢复/精确取消/批准后结算：
    `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-bk19P1/summary.json`
  - 双设备竞争 + 手机 queued 刷新 + 任务输出 + 撤销隔离：
    `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-jPmw3r/summary.json`
  - 运行中取消：
    `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-2JjnTl/summary.json`
- 可重复命令仍为 run-owned-real-chain-20261010.mjs；分别加
  `--host-restart`、`--multi-device --queue-reload`、`--cancel-running`。
  host restart 场景会主动刷新页面建立新通道，不能以此宣称不刷新自动重连
  也通过。旧 UI DOM 不当成新通道恢复证据。
- 仍缺：host 强制 crash、daemon 单独/双方 crash 与自动恢复、精确 C
  查询失败保留、双 host UI、恶意 wire 与 stress/WASM、公网跨域。浏览器
  缓存检查只核验 localStorage 字符串、缓存请求和数据库名单，不外推完整
  IndexedDB 正文扫描通过。所有 fullPlanVerified/publicChainVerified=false。

- 补充当前验证：存储检查已升级，真实页面只读扫描 localStorage、
  sessionStorage、CacheStorage 响应 bytes 及全部 IndexedDB store 记录，
  不写出记录正文；vault 必须恰有一个非空密封记录，AES-GCM CryptoKey
  extractable=false，实际 exportKey 被拒绝。仅扫描指定合成正文标记，
  不宣称形式化证明任意业务内容永不落盘。
  最新双设备 + queue reload + Output + 此存储检查通过：
  `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-cPM5IK/summary.json`。
- 最新 daemon 合成整体黑盒 8/8 重验：
  `C:/Users/Public/Temp/opencode/provider-chain-Eq7uMa/summary.json`。
  其运行时对应上述 host-attach 与 output-bound 代码，随后只修改测试驱动
  的存储检查，不改生产代码；检查结果不外推后续未知修改。

- 新增不刷新自动重连失败检查：运行器参数
  `--host-restart --host-auto-reconnect` 要求旧 host 结束后手机输入框先
  disabled，再创建新 host；不以旧 DOM 或 relay socket 活着作为恢复证明。
  最新失败：`C:/Users/Public/Temp/opencode/owned-real-chain-frozen-uRAgIv/`
  是安全重启早期诊断；真正不刷新检查失败证据为
  `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-paQ4LW/summary.json`，
  `hostRestartStage=old-channel-disabled`。relay 连接持续时手机未及时识别
  host 离线。需端到端存活检测，relay ping 不能替代 host 在线证据。
  本次未增加轮询整份快照来掩盖此缺口，不宣称自动恢复已通过。

### 端到端存活与创建句柄崩溃恢复（2026-10-10，续）

- protocol 2 增加加密 LivenessProbe/LivenessReply；只核验当前 Noise 周期
  与单个 UUID，不读取 daemon/工作区。relay ping/ack 不更新存活时间。
  空闲 5 秒发一项 probe，5 秒无匹配回复则关闭通道并用新 Noise 重连，
  不重放旧密文或重新提交 prompt。关闭时移除计时器/可见性监听；隐藏页
  不发送 probe，恢复前台时先重建过期通道再恢复操作。
- host 安全退出、不刷新恢复已通过（13pTAD）；之后补真正 crash 注入。
  新 acceptance_owned_crash_child 仅在受控 Job 中启动有合成标记的冻结
  host/daemon。随机一次性 stdin 命令触发 Rust Child::kill 的 Windows
  创建句柄 TerminateProcess，wait 核对 exitCode=1；无 PID 打开/扫描、
  无产品故障开关。独立 observer 仍核验真实后代句柄、确切 Job 和退出。
- daemon 单独 crash 要先观察旧工作区控件失效，再重启 daemon；host 保持
  存活。修复手机 workspaceUnavailable 清除旧 runtime 的 offers/catalogs，
  保留持久 operation/queue 身份。双方 crash 恢复只证明 queued 取消与
  不重放，不代表原运行任务未知结算或全部 F32–F36 已验收。
- 同时修复已打开视图未跟随 sessionList 更新标题的问题；不换 sessionId。
- 更正历史“durable rename”：seed 是未启动草稿，daemon task-state 回读
  只证明内存更名。StateStore::save 明确不持久保存草稿，这是原有行为，
  本轮不改该产品规则。现对真正已启动任务更名并验证跨 daemon crash
  及手机刷新保留；不再把 seed 回读写成持久更名通过。
- 最新原生冻结 remote-native-snapshot-20261010-owned-crash.json（5 个
  exe，185 个源码输入）；页面 remote-candidate-nekKDt/summary.json。
  路径均在 C:/Users/Public/Temp/opencode/ 下。对应通过证据：
  - host 创建句柄 crash、不刷新重连、原审批继续正常结算：
    owned-real-chain-frozen-tBla1Y/summary.json。
  - daemon 单独 crash、host 存活、原队列恢复取消、旧审批不可用：
    owned-real-chain-frozen-OwtOLA/summary.json。
  - host + daemon crash、持久任务名称、原队列取消、不虚构成功：
    owned-real-chain-frozen-E1YBWu/summary.json。
  - 双设备竞争/撤销隔离 + queue reload + Output：
    owned-real-chain-frozen-0iyFDE/summary.json。
  - 运行中 Stop：owned-real-chain-frozen-YzxCFh/summary.json。
  - 空闲 probe 回复 + 隐藏页无发送 + 前台新通道与读取（无刷新）：
    owned-real-chain-frozen-4tzmTy/summary.json。
  每轮 inputsUnchanged/containmentVerified=true；仍是同源 loopback 转发，
  不外推公网验收。失败目录全部保留；早期失败不计通过。
- 当前所有权基线仍为 ownership-debug-frozen-660f2dd57e814af1b2fe1b22f288bbea/
  matrix-report.json（19/19）与 browser-ownership-frozen-1317b08141fc4563823bdf17a06ec2d7/
  matrix-report.json（3/3）。bounded acceptanceDeadlineMs 只由黑盒读取，
  传给 supervisor 前移除该字段；不扩大容纳或清理权限。
- 可重复命令 node C:/Users/Public/Temp/opencode/run-owned-real-chain-20261010.mjs；
  host crash 加 --host-restart --host-auto-reconnect --host-crash；双方再加
  --daemon-crash；daemon 单独使用 --host-restart --host-auto-reconnect
  --daemon-crash --daemon-only。存活场景 --idle-liveness --foreground-liveness。
- 仍待精确 C 在飞/查询失败保留、原任务未知结算、双 host UI 与逐 host
  隔离、恶意 wire、stress/WASM、公网/真实模型及最终统一资产。未提交、
  push、部署；fullPlanVerified/publicChainVerified 始终 false。

### 双 host 与隐藏运行（2026-10-10，续）

- 手机可信记录改为逐 host 列表，仍保留旧 HOST_KEY 兼容迁移与当前目标。
  每项保存配对公钥、指纹、relay 与该 relay 注册的 deviceId，不保存正文。
  配对另一台不覆盖第一台；相同 hostId 的公钥/relay 变化在配对前拒绝，
  失败时恢复此前目标。菜单切换先关旧连接、清除目标相关内存；旧 effect
  回调按 cancelled 拒绝，SessionView 按 hostId/sessionId 隔离。
- 已打开 /pair 页面再扫描新链接可能只触发 hashchange，不重新挂载 React；
  现在监听 hashchange 并使用 in-flight guard，配对期间阻断旧 KK 连接竞争。
- 两台真实独立 daemon/host/workspace、同一设备密钥的真实 XX 分别配对，
  切换后分别读到不同合成文件；撤销第一台 grant 后第二台仍可读取。
  最后忘记第二台仅移除其本机记录，第一台记录/设备身份保留，第二台
  服务器 grant 仍在，符合“忘记不等于撤销”。双设备同时批准只回复一次。
- 用户要求不再使用可见终端。runner 以 windowsHide+管道隐藏执行，底层
  CreateProcess 启动器设置 STARTF_USESHOWWINDOW/SW_HIDE；crash helper
  使用 CREATE_NO_WINDOW。首版为全部 supervisor 后代加 CREATE_NO_WINDOW
  导致无控制台的 fixture 子孙启动出现额外系统成员，19 项矩阵有 6 项
  失败，已改为隐藏但保留控制台继承；失败产物保留，不放宽拓扑检查。
  隐藏版全矩阵 19/19 和 browser 3/3 重新通过。
- 最新冻结：remote-native-snapshot-20261010-hidden-crash.json；当前页面
  remote-candidate-LfXlBA/summary.json。隐藏容纳证据：
  ownership-debug-frozen-facf3305d45e47d094960aef5e407900/matrix-report.json，
  browser-ownership-frozen-11c2446e71804203a79a4ddb11997181/matrix-report.json。
  全部路径在 C:/Users/Public/Temp/opencode/ 下。
- 最新双 host + 双设备 + queue reload + Output + 撤销/忘记隔离通过：
  owned-real-chain-frozen-4qytNu/summary.json；运行参数
  --multi-host --multi-device --queue-reload。源码/冻结字节未漂移且独立容纳
  验证通过。这不是每台 host 均拥有两枚设备的全关系矩阵，也不是公网证据。
- 后续页面变更后，前节 host/daemon crash 与前后台存活报告仅代表旧页面
  状态，需在最终页面统一复验。精确 C 在飞/查询失败、原任务未知结算、
  恶意 wire、stress/WASM、公网与最终包仍未关闭门禁。未提交/push/部署。

- 隐藏版统一复验发现 daemon-only 旧队列重复恢复：同 operationId 的本机
  项与持久 row 被再次 enqueue。仅跳过已有 ID 又会漏掉断链窗口本机投影
  已结算、但 daemon 仍 queued 的记录。改为 PromptQueues::restore 原位
  对账，使用持久 state，保留 cancel_requested；先恢复队列再发布新 daemon
  连接，重新附加视图同时推 queue snapshot。daemon-only 连续两轮通过：
  owned-real-chain-frozen-oHnYQU/summary.json 与
  owned-real-chain-frozen-EAmnHq/summary.json。早期 OwtOLA 不能替代这次
  更严格复验，失败目录 4kfJeU/TN1z0z 等保留。
- 增加冲突 hostId/公钥拒绝验收：使用已信任 ID 配上另一枚合成公钥，
  pairing 之前拒绝，grant 文件和可信记录不变；原连接恢复。发现同步拒绝
  的 React 批处理可能令 host/busy 最终值未变化而不重启已关闭连接，增加
  显式 connectionGeneration，拒绝后的旧目标使用新通道恢复。
- 当前 native 冻结更新为 remote-native-snapshot-20261010-recover-reconcile.json，
  当前页面为 remote-candidate-iNWuAd/summary.json。最新冲突身份拒绝 +
  双 host + 双设备 + queue reload + 撤销/忘记隔离通过：
  owned-real-chain-frozen-m7bcuM/summary.json。上一页面 LfXlBA 对应原生
  recover-reconcile 的双方 crash、双 host 和前后台存活分别为 THViqm、
  9ibqrn、NOTY5e；仍需最终页面重跑，不混同 siteSha256。

- 精确 C 新真实链路检查：seed_remote_workspace --verify-phone-cancel 仅
  在合成 host/workspace 标记齐备时只读读取 host operations 的 C/target，
  使用真实配对 grant 查询 daemon。正确查询重复两次，错误 C/target/host/
  device/grant 共五次均 unknown；前后 ListRemotePrompts 字节一致。
  不新增取消/派发、不输出正文、不写 host 数据库。该检查是在手机实际
  取消后执行，不能替代手机 wire 查询或在飞 C 故障验收。
- --cancel-result-restart 在取消后再次用创建句柄 crash/restart，再查询同一
  C/target。daemon-only（KPrfMy）与双方（WJ3t7O）均通过，正确 cancelled
  绑定保留，队列不复活，prompt 计数仍为 1。完整证据目录均为
  C:/Users/Public/Temp/opencode/owned-real-chain-frozen-<名称>/summary.json。
- 当前统一 native：remote-native-snapshot-20261010-phone-c-query.json；页面
  仍为 remote-candidate-iNWuAd，完整 siteSha256 相同。最新通过：
  host crash + 精确 C：SvA296；运行中 Stop + 精确 C：hFTLjM；双 host +
  双设备 + 队列刷新 + 冲突身份拒绝 + 精确 C：yW2rey。之前当前页面
  daemon-only/双方：62uDh6/QC04Dg；前后台存活：kvdsXF。前后台并行
  首轮 DBkGss 失败，顺序复验通过；不据此宣称任意并发/压力验收通过。
- 仍未验收：在飞取消与查询故障后 host 保留重试、原运行任务未知结算、
  手机恶意 wire、stress/WASM 与公网/真实模型。fullPlanVerified=false。

### 正常会话管理与当前入口阻断（2026-10-10，续）

- 用户原话：“不做恶意测试了，容易让openai停止模型服务”。本轮未执行
  恶意 wire；外层和内部运行器都在启动前拒绝该模式，已有文件保留，不以
  正常读取替代其验收。不委派、不调用真实模型、不提交、push 或部署。
- 新增正常 `--session-management` 情景：真实手机控件归档已完成任务，
  刷新后保留标记、身份和历史；恢复不重新发 prompt；删除确认先取消、
  再确认删除唯一合成草稿，daemon 只读回查其它任务 ID 集合不变。
  编码前失败清单已写入 `remote-control-chain-acceptance.md`。
- 首轮 `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-xkVQU9/summary.json`
  为 **失败，不能计作通过**：内部 chain 的 18 项正常行为检查均通过，
  inputsUnchanged=true，清理/后代观察字段通过；但 preconditions 的
  runner.notInAnyJob 与 supervisor.notInAnyJob 均失败，runnerInAnyJob=true。
  当前 Fintwind 工具执行入口带上级 Job，因此不能证明无外层 Job 掩盖的
  独立容纳。失败目录保留，不扩大清理范围、不使用 breakaway 绕过。
- 外层运行器追加单独的 ownership preflight 阶段；失败时写 blocked 摘要，
  不再启动 daemon、host、relay、provider 或浏览器。修改后只做 JavaScript
  语法检查；新门禁未执行，不能宣称本轮管理验收或入口修复已行为通过。
- 隐藏执行包装器：
  `C:/Users/Public/Temp/opencode/invoke-owned-chain-management-20261010.ps1`。
  它使用 CreateNoWindow、环境白名单和管道，仅输出产物路径，不打印原始
  stderr。独立运行前仍必须通过上述 preflight，不能由隐藏窗口替代容纳证明。
- 下一步需要不在上级 Job 内的独立入口；入口解决前只推进源码和静态检查。
  fullPlanVerified/publicChainVerified 继续为 false，公网和用户发布未执行。
- 用户通过选择工具选定“独立隐藏入口”。已准备
  `C:/Users/Public/Temp/opencode/start-normal-management-acceptance.vbs`，须由用户
  从 Windows 文件管理器双击启动；本会话不代为启动、不使用 breakaway。
  VBS 隐藏启动 PowerShell 7，PowerShell 再以 CreateNoWindow 启动 Node；
  全部实际子树仍由已冻结的 supervisor 独占 Job 管理和独立 observer 核验。
  运行完成仅弹出固定结果与独占 `remote-management-click-*/launch.json` 路径。
- 包装器使用环境白名单、独立 HOME 和十分钟等待上限；超时不扩大进程清理，
  标为 unverified。launch.json 仅存状态、产物路径、运行器哈希和时间；
  原始 stdout/stderr 不落盘。实际验收摘要仍由运行器在独占目录保存。
- `check-management-launcher-syntax.ps1` 的 PowerShell 解析与两个 JavaScript
  `node --check` 均通过。VBS/独立入口尚未由用户执行，未计作行为通过；
  正常管理仍待外部入口下的完整冻结验收，用户无需开启可见终端。

### 用户独立隐藏运行：正常管理通过（2026-10-10，续）

- 用户从独立入口运行后提供成功弹窗。主代理读取实际
  `C:/Users/Public/Temp/opencode/remote-management-click-rad53B76.tmp/launch.json`
  与其关联报告，而非只凭截图判断通过。launch exitCode=0，
  maliciousTestsEnabled=false；未运行恶意 wire、未调用真实模型。
- 本轮冻结真实链路：
  `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-ElfNJ8/summary.json`。
  **18/18 正常行为检查通过**，包含实际 XX 配对、本机确认、附加、分页读取、
  创建/发送/排队/精确取消、审批/问题/成功结算、Output、身份刷新和撤销，
  以及新增的完成任务归档/恢复、删除确认取消与仅删除指定合成草稿。
- 独立 ownership preflight 2/2、实际链路 ownership 3/3 通过；两轮
  runnerInAnyJob=false，runner/supervisor 不属于上级 Job。实际调试树移交
  127 个真实进程句柄，独立 observer 的确切 Job/拓扑/退出检查均 verified，
  cleanup.unverified=[]，旁观者全程存活并由本轮自有作业清理。
  supervisor final 的 allChildrenExited、ioWorkersExited、
  startupCleanupVerified、debugCleanupVerified、jobActiveProcessesZero
  均为 true。该独立入口通过不解除本会话工具入口的上级 Job 限制。
- 主代理随后仅做只读产物复核，10 项证据核对通过：全部输入 SHA-256、
  当前原生/PWA 源码与冻结副本、完整 20 项页面资产、包装器/运行器、原始
  报告哈希及清理字段均匹配。provider 只有 1 次 prompt、1 次成功终态，
  管理操作没有重新提交或中断已完成任务。复核不是新增行为测试，不累加到 18 项。
  复核产物：
  `C:/Users/Public/Temp/opencode/normal-management-evidence-nhOu85/summary.json`；
  可重复命令：
  `node C:/Users/Public/Temp/opencode/verify-normal-management-evidence-20261010.mjs`。
- 代码状态仍使用 native `remote-native-snapshot-20261010-phone-c-query.json`
  和页面 `remote-candidate-iNWuAd`。本轮主未启动或清理服务，只读取已完成
  的本轮产物；未提交、push 或部署。
- fullPlanVerified/publicChainVerified 仍为 false：本轮未覆盖在飞取消与查询
  故障保留重试、原运行任务未知结算、选项实际应用、历史分支/回退、完整
  双 host × 双设备关系、stress/WASM 最终复验及公网跨域。恶意 wire 按用户
  决定保持未执行，不冒充通过。

### 正常测试批次准备与重新核验入口（2026-10-11）

- 用户要求“继续所有测试”。沿用“不做恶意测试”、隐藏运行、独立合成数据
  的范围；无真实模型、单元测试、watcher、视觉测试、提交或部署。
- 编码前失败方式及未具备条件的项目见 `remote-control-normal-suite.md`。
  正常批次包括完成任务管理、忙任务归档、已启动任务删除、Stop、刷新恢复、
  安全重启/崩溃、双设备/双 host、存活检测和实际 waiting worker 更新。
  未具备条件的选项应用、历史操作、故障查询、完整关系矩阵及旧压力运行器
  保留未验收，不以本批次替代。
- 新入口：`scripts/invoke-normal-remote-suite.ps1`，顺序批次：
  `scripts/run-normal-remote-suite.mjs`。外层冻结运行器另存为
  `C:/Users/Public/Temp/opencode/run-owned-real-chain-20261011.mjs`，没有覆盖
  10 月 10 日的成功轮运行器。正常批次拒绝恶意模式并跳过旧双 host 情景中
  的冲突身份输入；清理无法独立核验时停止后续轮。
- 10 月 11 日当前工具入口的 `--preflight-only` 实测通过：
  `C:/Users/Public/Temp/opencode/owned-real-chain-frozen-USPV2h/summary.json`。
  preconditions 与 outsider-final 共 2/2；runnerInAnyJob=false，runner 和
  supervisor 不属于上级 Job，cleanup.unverified=[]。未启动业务链路。
  这是本次实际启动环境的证据，不将此前入口失败改写为通过，也不对所有
  工具启动方式作推断；每个后续轮仍须独立 preflight。
- 隐藏 PowerShell 包装器解析、批次/外层/证据复核 JavaScript 语法检查通过。
  备用用户双击入口为
  `C:/Users/Public/Temp/opencode/start-normal-remote-suite-20261011.vbs`。
- 首轮静态检查准备失败：
  `C:/Users/Public/Temp/opencode/normal-remote-static-ZJ5bZI/summary.json`，
  checks=[]；误列不存在的 `tsconfig.node.json`，已移除并重新启动。
  此失败保留，不计编译或行为通过。
- 主代理已通过隐藏包装器启动正常批次，启动前检查逐轮执行；结果待完成后
  读取并复核，不在运行中宣称通过。产物复核脚本：
  `C:/Users/Public/Temp/opencode/verify-normal-suite-evidence-20261011.mjs`。
- 第二轮静态检查保存为
  `C:/Users/Public/Temp/opencode/normal-remote-static-PFIRb9/summary.json`：
  7/10 静态检查通过，输入未变化；workspace、现有 test targets、remote examples
  三项编译失败。WASM、PWA 类型、协议漂移、三个驱动语法及合成 provider
  语法通过。这不是行为验收，也不是整体静态检查通过。
- 编译诊断：
  `C:/Users/Public/Temp/opencode/normal-native-diagnostic-3tSLnq/summary.json`。
  ring 报 stddef.h 不存在，SQLite 报 stdarg.h 没有 include 路径；检查脚本的
  白名单漏掉当前 MSVC 的 INCLUDE/LIB，而非已证明的远控源码错误。
  已补入编译器与 SDK 变量并重新检查；仍不继承模型凭据，不更改业务源码。
- 补齐 MSVC 编译环境后的第三轮静态检查 **10/10 通过**：
  `C:/Users/Public/Temp/opencode/normal-remote-static-6P3Sob/summary.json`。
  workspace、现有测试目标（仅编译）、remote examples、WASM 目标、PWA 类型、
  跨端协议漂移、三个驱动与合成 provider 语法全部 exitCode=0，inputsUnchanged=true。
  未执行单元测试，也不把此结果算作真实链路或公网通过。
- 主代理只读复核第三轮静态结果：
  `C:/Users/Public/Temp/opencode/normal-static-evidence-9iUcuU/summary.json`，
  3 项证据检查通过；十项退出码/不升级覆盖、冻结检查器和全部记录的当前
  输入 SHA-256 均匹配。复核脚本为
  `C:/Users/Public/Temp/opencode/verify-normal-static-evidence-20261011.mjs`。

### 正常批次首轮结果与两项复测（2026-10-11，续）

- 首轮批次：
  `C:/Users/Public/Temp/opencode/normal-remote-suite-Vp1s7p/summary.json`。
  **13 项业务情景中 11 项通过、2 项失败，无未运行项**；另有独立 preflight
  通过，所以摘要计数为 passed=12/failed=2。inputsUnchanged=true；13 个业务轮
  都 containmentVerified=true。不能把 12 当作业务情景通过数，也不能把失败轮
  内已完成的子步骤累计为整个场景通过。
- 通过范围：完成任务管理、已启动任务删除、显式 Stop、手机队列刷新、host
  安全重启和 crash、daemon 单独及联合 crash 后精确取消结果再次重启保留、
  双设备审批、双 host 切换、空闲与前台恢复。完整两 host × 两设备关系仍未覆盖。
- 失败轮：忙任务归档 `owned-real-chain-frozen-MD7cxh` 在归档前的精确 C 只读
  查询 helper 退出；页面更新 `owned-real-chain-frozen-ih2lBZ` 在浏览器创建管道
  connect 超时，未进入页面更新步骤。第二轮 chain 的 failedPhase 错沿用了
  已通过的 seed 阶段，browser.json 确认为 connect/timeout=true；不将其误记为
  daemon seed 或 Service Worker 业务缺陷。
- 主代理复核：
  `C:/Users/Public/Temp/opencode/normal-suite-evidence-vZ9xND/summary.json`，
  evidenceVerified=true、batchPassed=false；输入、完整页面清单、原始报告哈希、
  每轮独立前置与整树句柄清理均复核，成功轮只有一次显式 prompt。失败轮
  保持失败。最初复核器误要求启动前失败轮必须有 provider 日志，首份复核
  `normal-suite-evidence-I4E9ki` 因此失败；已允许此类失败轮缺失日志，不允许
  成功轮缺失，原报告均保留。
- 为诊断忙任务归档前失败，仅读取原数据库副本：
  `C:/Users/Public/Temp/opencode/normal-cancel-inspection-vpub82y8/summary.json`。
  观察到 exact cancellation 的 cancelled 终态存在，prompt 已不在等待表，
  host/device/grant 均匹配，原始数据库及 WAL/SHM 哈希未改变。这只证明结束
  后落库状态，不证明当时 helper 已成功，也不足以确认瞬态失败的根因。
- 业务实现未改；驱动仅追加失败子进程的固定错误类别和退出码，以及正确的
  浏览器连接阶段。未保存原始输出、未增加自动重试、未放宽判据。两处 JavaScript
  语法通过后，在全新目录单独复测忙任务归档与更新，其余 11 项不在复测范围。
  原首轮输入对应的静态/行为结果不宣称适用于此后的驱动改动。
- 两项独立复测 **2/2 通过**：
  `C:/Users/Public/Temp/opencode/normal-remote-suite-7q4iiX/summary.json`，
  scope=targeted-normal-recheck，另有 preflight 通过，未重跑的 11 项明确列在
  omittedScenarios。忙任务归档轮 `owned-real-chain-frozen-k8B0AG` 14 项检查；
  页面更新轮 `owned-real-chain-frozen-PE4kDf` 16 项检查。均 exitCode=0、
  inputsUnchanged=true、cleanupVerified=true。未修改产品业务实现或放宽判据，
  所以复测成功不等于首次瞬态失败根因已确认。
- 复测的主代理只读复核：
  `C:/Users/Public/Temp/opencode/normal-suite-evidence-ZsDWEC/summary.json`，
  evidenceVerified=true、batchPassed=true。已核验归档产生实际 interrupted 终态
  且无重发，更新确有 waiting worker、输入保留直到显式 Reload、身份保留且
  草稿未持久化；仅是本次两项复测通过，不把首轮整体改为通过。
- 下一步使用当前驱动与批次源码完整重跑 13 项，统一输入版本；保持
  fullPlanVerified/publicChainVerified=false，未覆盖项目继续列明。
- 当前驱动版本的静态复验再次 **10/10 通过**：
  `C:/Users/Public/Temp/opencode/normal-remote-static-9zsWn1/summary.json`；
  主代理只读复核为 `normal-static-evidence-jP4v3c/summary.json`，3 项通过。
  已在静态编译结束后启动完整批次，避免将编译负载与浏览器启动叠加；
  未增大超时、未自动重试业务步骤、未修改产品源码。

### 同一输入完整正常批次通过（2026-10-11，最终轮）

- 最终完整批次 **13/13 正常业务情景通过**，另有独立 preflight 通过：
  `C:/Users/Public/Temp/opencode/normal-remote-suite-oQ0cKY/summary.json`。
  scope=full-defined-normal-batch，omittedScenarios=[]，failed=0、notRun=0，
  inputsUnchanged=true；摘要 passed=14 包含 preflight，不是 14 项业务情景。
  运行期间没有源码改动，不混用首轮和两项复测结果。
- 全部 13 轮的真实 XX/KK、relay/host/daemon/浏览器链路和各自场景判据通过。
  包括实际忙任务归档并停止、已启动任务删除、队列/Stop、刷新、重启/crash、
  两设备审批、双 host 切换、前后台存活，以及 waiting worker 保留输入直到
  用户 Reload。各轮只有一次明确 prompt；归档/Stop 各有一次 interrupted，
  已启动任务删除恰有一次 provider 删除。没有真实模型调用。
- 主代理只读证据复核：
  `C:/Users/Public/Temp/opencode/normal-suite-evidence-wSGA2s/summary.json`，
  evidenceVerified=true、batchPassed=true。每轮独立 preflight 和业务 runner
  均不属于上级 Job；整树退出、I/O worker 退出、Job 空、独立 observer、旁观者
  隔离和输入/资产/报告哈希均通过。每轮核验 114–145 个真实进程句柄。
- 汇总产物（静态 10/10 + 正常行为 13/13 + 独立复核）：
  `C:/Users/Public/Temp/opencode/normal-remote-acceptance-W0w7PV/summary.json`。
  汇总检查 3055 个跨静态/行为重复记录的输入哈希一致，保留原报告哈希、
  逐轮计数、未覆盖项和可重复只读复核命令；不启动任何额外服务。
- 首次失败批次和两项复测目录均保留；首次瞬态失败根因未完全确认，
  firstFailureRootCausesResolved=false，不因为最后通过而声称已修复产品缺陷。
  此轮只改验收驱动、合成 provider 记录与文档，没有改产品业务实现。
- fullPlanVerified/publicChainVerified 继续为 false。仍待选项实际应用、历史
  分支/回退、在飞取消/查询失败重试、原运行任务未知结算、完整双 host × 双设备
  关系、stress/WASM 性能复验及公网跨域。恶意测试按用户决定未执行。
  未提交、push、部署、触及生产数据或终止其它会话进程。

### 暂停前扩展场景与恢复（2026-10-11）

- 用户先要求暂停，随后评估 OpenTunnel；未修改连接架构。用户决定保留原方案，
  并以“继续吧”恢复开发。仍不运行恶意测试、watcher、视觉测试或真实模型；
  Cloudflare 页面由用户自行部署。已有 WIP 和全部失败轮不回滚、不覆盖。
- 扩展正常清单已为 17 项，旧统一 13/13 与静态 10/10 只对应旧冻结输入，
  不是当前扩展版本已通过。完整双 host × 双设备关系单项曾通过，最终须重跑。
- `normal-remote-suite-JjmtFB/summary.json` 中正常 WASM 测量通过、历史失败；
  `normal-suite-evidence-xKxvbK/summary.json` 为 evidenceVerified=true、
  batchPassed=false。采用真实页面 bridge 与独立原生 Noise peer，预热后测量
  三轮双向 1 MiB；不冒充真实手机或非调试生产性能。
- 最新已运行历史轮 `normal-remote-suite-SDn10v/summary.json` 仍失败在
  historyStage=fork-inspection；复核 `normal-suite-evidence-wPDt24/summary.json`
  为 evidenceVerified=true、batchPassed=false。分支显示标题已修正，随后发现
  分支继承源 native_session_id，已修正原生身份并清除 runtime_event_cursor，
  此身份修正尚未运行通过。
- 模型、Agent 与变体提交、可选 Agent 目录发现及真实 Agent 应用已写入代码；
  `session-options-apply` 三轮场景尚未验收。桌面 IPC 协议为 13，外部远控协议
  仍为 2；新的页面、WASM 与 native 必须重新冻结配套使用。
- 暂停前 `remote-native-snapshot-20261011-history-options.json` 构建失败 exit 101，
  没有成功摘要，原输出未保留足够诊断，根因未知。后续
  `normal-native-diagnostic-Xiq1EH/summary.json` 的 cargo check 通过，不当作
  成功构建或行为证明。暂停后原已启动 WASM 构建自然成功结束，未继续测试。
- 恢复轮先启动同一六产物编译命令的过滤诊断，不启动业务服务。snapshot 入口
  追加独立的编译环境白名单与 build-diagnostic.json；失败仍不写成功 snapshot。
  当前诊断已完成：`normal-native-build-diagnostic-LhQMtN/summary.json`，同一六产物
  cargo build exitCode=0，耗时约 209 秒；原 exit 101 未复现，根因仍未知。
  随后 Inspector 追加正常取消查询模式，此编译结果不代表该后续修改已编译。
  未重新提交、push 或部署。
- 新历史判据要求读回两个独立原生身份和持久轮次；选项复核同时核对 Inspector
  和真实合成 provider 请求计数。不降低判据、不重试业务步骤、不延长截止。
- 上述相对路径均位于 `C:/Users/Public/Temp/opencode/`。后续结果须追加到本节，
  fullPlanVerified/publicChainVerified 保持 false。
- 恢复核对发现旧精确取消 helper 内含错误 host/device/grant 等五种负面查询。
  保留旧 helper，但正常驱动改用 `--verify-phone-cancel-normal`，只重复查询实际
  配对的原 C/P；产物明确 identityBoundaryNegativeVerified=false。不执行这些
  负面查询，不用它们增加正常检查计数；新模式尚待冻结后运行。
- 最新配套原生 snapshot 已成功：
  `remote-native-snapshot-20261011-resumed-history-options.json`，六个二进制、
  186 个来源输入；编译诊断为 `remote-real-chain-UI4Wbc/build-diagnostic.json`。
  私有生产页面候选 `remote-candidate-PEAzTM/summary.json` 构建通过，使用当前
  WASM、源码与正式页面配置。候选仅用于隔离测试，不是发布或公网验收。
- 扩展运行器已指向上述 native/site。已启动 `history-fork-rewind` 与
  `session-options-apply` 两项定向正常批次，每轮仍先独立核验容纳边界；
  当前行为结果待完成后记录。未改旧成功批次或混用旧页面。
- 用户反馈推进太慢，建议复用 ZCode/Codex 与现有 PWA。本阶段不扩展功能、
  不重做 PWA，先关闭历史与选项两项阻断，稳定后统一验收；原剩余故障恢复、
  正常压力与公网项目仍明确未验收，不因缩短当前批次而删除其判据。
- 定向批次 `normal-remote-suite-fA0vx0/summary.json`：历史通过，选项在忙任务
  控件检查失败；全批仍失败。只读复核 `normal-suite-evidence-qLM8MA/summary.json`
  为 evidenceVerified=true、batchPassed=false，整树清理与输入哈希通过。
  历史实际三次 prompt，分支与源 nativeSessionId 不同，源回退保留一轮且分支
  不受影响。合成链路通过不代表真实 OpenCode worktree 快照恢复。
- 选项失败定位为运行器寻找不存在的 `Options` 按钮；实际入口为已有
  `Models and agents`。仅修正运行器标签，并允许复用已加载目录，不改页面，
  不扩大超时或重试业务动作。保留失败轮，下一轮单独复测选项。
- 标签修正后的 `normal-remote-suite-gvePh0/summary.json` 仍失败；已通过忙任务
  禁用检查，读取两模型与三个可见 Agent，随后停在选择选项阶段，尚未 Apply。
  容纳与整树清理通过。追加固定子阶段及合成选择控件的 ID/值/禁用状态诊断，
  不保存用户正文或原始异常；下一轮只诊断此阻断。
- 诊断轮 `normal-remote-suite-xwMebc/summary.json` 停在 select-effort-b：模型 B 与
  自定义 Agent 已被选择且可用，但页面没有变体控件。只读复核
  `normal-suite-evidence-3T4OOl/summary.json` 为 evidenceVerified=true、batchPassed=false。
  源码核对发现内部 ProviderModel/ProviderAgentPreset 没有 camelCase 序列化，
  host 却以 reasoningEfforts/serviceTiers/contextWindows/isDefault 读取它们。
  仅修复 host 这五处内部字段名；外部协议与 PWA 不变。准备新冻结输入再验收。
- 五处字段修正后的 native snapshot 构建通过：
  `remote-native-snapshot-20261011-catalog-traits.json`；诊断
  `remote-real-chain-6QLarI/build-diagnostic.json`。配套私有页面
  `remote-candidate-i3i9FI/summary.json` 构建通过，与前一候选的 PWA 应用资产相同。
  已更新扩展入口并启动选项单项；结果尚待完成。不部署，不更改生产文件。
- `normal-remote-suite-7RR1rY/summary.json` 已能显示和选择 careful，Apply 仍失败。
  只读复核 `normal-suite-evidence-igO696/summary.json` 为 evidenceVerified=true、
  batchPassed=false。读取已清理轮的独立 operations 数据库确认一次选项请求为
  failed，固定拒绝原因为“the task permission mode changed on this computer”。
  list_projection 刻意剥离实际权限/变体，adapter 却回传列表默认 FullAccess；
  daemon 正确拒绝覆盖实际 Ask。应用选项改为 HydrateSession 读取实际设置，
  不放宽权限校验。诊断只输出白名单原因与合成配置；不重发业务步骤。
- 实际权限读取修正已构建冻结：
  `remote-native-snapshot-20261011-hydrated-options.json` 与
  `remote-real-chain-ET6OTX/build-diagnostic.json`，六产物、186 个输入。
  配套候选 `remote-candidate-Q44eSA/summary.json` 构建通过；PWA 应用资产未变。
  选项单项已使用新配套输入启动。最终证据汇总器改为要求四个显式摘要，
  校验 17 项完整批次，避免继承旧版硬编码静态结果；旧冻结汇总仍保留。
- `normal-remote-suite-vAAHSS/summary.json` 仍在 first-apply 阶段失败；
  `normal-suite-evidence-QOebma/summary.json` 为 evidenceVerified=true、batchPassed=false。
  已清理轮的只读数据库确认一次 Apply 已 executed，模型 B、自定义 Agent 和
  careful 已持久化。任务选项行为并未据此标为通过：尚未运行下一轮 prompt。
  等待回执改为检查状态行内的实际结果 span，并在不匹配时记录固定文案分类及
  Inspector，不存原始异常、不中断判据、不自动重发。
- 回执等待轮运行期间只读核对发现 SessionView 的 USER_OPERATION_KINDS 缺少
  applySessionOptions；实际保存成功仍不会显示结果行。已定位这处页面过滤，
  待该轮完成再修改，避免运行中输入漂移；不移除可见结果判据。
- `normal-remote-suite-aPjTLO/summary.json` 保留为失败，独立复核
  `normal-suite-evidence-xZsrdV/summary.json` 为 evidenceVerified=true、batchPassed=false。
  已结束本轮后给 USER_OPERATION_KINDS 补上 applySessionOptions；复用现有结果
  状态行，没有新增说明文案或组件。准备仅重建私有页面，不重复编译 native。
- 选项轮 `normal-remote-suite-McrmGH/summary.json` 的业务驱动报告通过，但独立
  复核 `normal-suite-evidence-FEQSMd/summary.json` 拒绝：三次 prompt 有六次成功
  终态。源码/日志确认每个新 SSE 从零执行旧轮，旧连接也继续执行后续轮，
  原生消息被重复追加。该轮不当作验收通过，保留失败复核。
  测试服务补 SSE generation 与空闲订阅起点，旧订阅退场、不重演已完成轮。
  业务驱动同步补上三成功终态/九条原生消息判据；复核计数不放宽。
  仅变更运行时 fixture 和测试驱动，native/PWA 字节不变，无须重复编译。
- 修正 fixture 后的 `normal-remote-suite-MY5t8s/summary.json` 选项单项通过；
  `normal-suite-evidence-RZ6d1A/summary.json` 为 evidenceVerified=true、batchPassed=true。
  恰好三次实际 prompt/三次成功终态/九条原生消息，两次模型应用、三次 Agent
  应用，模型 B/reviewer/careful → 模型 A/build/无变体，刷新不重发、重复 Apply
  只提交一次。权限始终 Ask。此结果只覆盖单项，不混成 17 项完整通过。
- 已启动当前输入的十项静态检查；完成并复核后运行完整 17 项正常批次。
  使用 `remote-native-snapshot-20261011-hydrated-options.json`、
  `remote-candidate-KpLBw0/summary.json` 及当前 fixture/驱动，暂不扩展功能。
  原在飞取消/查询失败、真实 worktree 恢复、压力、真实手机性能、公网验收等
  未覆盖项目继续保留；fullPlanVerified/publicChainVerified=false。
- 当前配套输入静态检查 **10/10 通过**：
  `normal-remote-static-NH96gY/summary.json`；只读复核
  `normal-static-evidence-O34Xdj/summary.json` passed=true、3 项检查通过。
  现有测试只编译、不执行，没有新增单元测试。静态检查结束后已启动同一输入
  完整 17 项正常批次，避免编译负载与浏览器验收重叠。运行期间不修改源码。
- 完整批次 `normal-remote-suite-cqd4AC/summary.json` **11/17 正常业务项通过**，
  另有 preflight 通过；六项失败均在 question answers / turn settles 阶段：手机
  queue reload、host 安全重启、host crash、双设备、双 host、完整关系矩阵。
  源码与输入未漂移，各轮独立清理完整；只读复核
  `normal-suite-evidence-q5wC4M/summary.json` evidenceVerified=true、batchPassed=false。
  当前完整批次不得标为通过，不拼接旧版局部通过。保留全部失败轮并定位共同
  结算问题，下一轮只复测 queue reload 以确认具体停在 transcript 还是 idle。

### 17 项正常批次最终复核（2026-10-11，最新输入）

- 已修复手机页面的生命周期状态合并缺陷：transcript 已更新且 provider 已成功
  结束时，旧的 session-list `working` 状态仍会让 Stop 按钮保持显示。PWA 使用
  `live.running` 的三态值，收到 `turnFinished` 后以实时边界覆盖滞后的列表状态。
  独立 PWA 提交：`abfab8e`（`fix remote session stop state after turn settles`）。
- 修复后的 `phone-queue-reload`、host 安全重启、host crash、双设备审批、双
  host 切换、完整关系矩阵均先独立定向通过；随后使用同一套最新输入完整重跑。
- 完整正常批次 **17/17 业务场景通过**，另有独立 preflight 通过：
  `C:/Users/Public/Temp/opencode/normal-remote-suite-fyeRfz/summary.json`。
  `batchPlanVerified=true`、`omittedScenarios=[]`、`inputsUnchanged=true`，
  所有业务轮 `exitCode=0`、`failedPhase=null`、`cleanupVerified=true`。
- 主代理只读证据复核：
  `C:/Users/Public/Temp/opencode/normal-suite-evidence-rIyc78/summary.json`。
  `evidenceVerified=true`、`batchPassed=true`；复核确认完整页面资产、运行器、
  每轮输入/报告哈希、独立 preflight、整树清理和正常覆盖边界均未被升级。
- 当前配套页面 candidate：
  `C:/Users/Public/Temp/opencode/remote-candidate-hcpc7h/summary.json`；旧页面
  candidate 未覆盖。失败首轮和全部定向复测产物均保留，不混用旧结果。
- 当前源码静态检查 **10/10 通过**：
  `C:/Users/Public/Temp/opencode/normal-remote-static-lJkHjI/summary.json`；
  只读复核 `C:/Users/Public/Temp/opencode/normal-static-evidence-3aaQHN/summary.json`
  通过 3 项检查。未执行单元测试或业务服务启动。
- `fullPlanVerified=false`、`publicChainVerified=false` 仍保持不变。未覆盖真实
  OpenCode worktree 快照恢复、在飞取消/查询失败重试、原运行任务未知结算、旧
  relay 压力测试、真实手机性能与用户部署后的公网跨域链路；恶意测试按用户决定
  不运行。
