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
