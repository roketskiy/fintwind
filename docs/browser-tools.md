# OpenCode 浏览器工具：第三阶段

> 以下保留第三阶段原始契约与历史验收证据。用户实测指出自动化产品缺口后，
> 已确认改为跟随「完全访问」，新增直接打开与可靠元素引用；本轮变更契约
> 和新验证记录见 [browser-automation-fixes.md](browser-automation-fixes.md)。
> 本文旧报告中的逐次审批 / 导航后重新共享仅描述当时实现，不是新自动模式的
> 期望；历史通过不能作为新修正通过的证明。

## 当前工具契约

| 工具 | 用途 |
| --- | --- |
| `fintwind_browser_open` | 在当前本机会话的完全访问 / 构建模式下，打开并等待本会话专用标签，再返回页面授权。 |
| `fintwind_browser_list` | 列出本会话自动化标签和明确共享的手动页面；空列表不是“桌面浏览器没连接”。 |
| `fintwind_browser_snapshot` | 读取有界页面观察结果及准确元素引用，不要求网页控件有 `id`。 |
| `fintwind_browser_click` / `fintwind_browser_fill` | 优先操作最新观察所得 `ref`；陈旧引用拒绝，不猜其他控件。 |
| `fintwind_browser_scroll` | 对授权页面做有界滚动，之后重新观察。 |
| `fintwind_browser_navigate` | 导航授权页面；自动模式保留标签权限，但必须重新观察新文档。 |

完全访问下不逐次审批；监督模式保留审批。已有手动标签不因完全访问而自动
共享。普通滚动、点击和聚焦不撤销共享；地址栏旁的停止共享才表示接管。
接管、关闭、权限降级、切会话和断连会撤销旧自动权限，不重放副作用。
私有服务会移除连接其他桌面宿主的 OpenCode 内置浏览器工具，不修改用户全局
配置或删除用户 MCP 服务。具体能力边界见手工验收文档。
交互修正与本轮验证另见 [browser-interaction-fixes.md](browser-interaction-fixes.md)。

以下为原阶段实现及验证记录，供追溯而不是当前完整产品验收结论。

## 目标与既有边界

把第二阶段已验收的本地 daemon / WebView2 协作桥接到 Fintwind 管理的
OpenCode V2 私有服务。Agent 仍使用原有推理与工具执行流程，不引入另一个
Browser Use Agent，不开启普通页面的远程调试端口。

保留显式页面共享、会话 / runtime / 页面 / grant 四元组、逐次审批副作用、
人工接管、有限快照、即时取消、不重放和不自动重试。第三阶段不扩大原生
适配器的主文档、可见唯一 CSS 选择器与普通文本输入边界。

## 实现前列出的失败模式

- 把插件实例的 `location` / cwd 当作调用会话，同一私有服务上的两个任务串页。
- 工具参数允许模型指定另一个 Fintwind 会话或 runtime，绕过调用身份绑定。
- 插件获得 daemon 主 token，能够读任务数据库、发终端命令或关闭 daemon。
- 浏览器专用凭据仍可订阅通用事件、发布共享、伪造页面结果或使用通用 RPC。
- runtime 更换、OpenCode 会话变化、私有进程退出后，旧映射与凭据继续有效。
- 子会话未映射却继承父任务页面，或未知会话退回到“当前页面”。
- `context.signal` 取消只中断等待，没有取消 GUI 中的审批 / 尚未发出的输入。
- 取消早于请求登记或连接建立时丢失；迟到响应使已取消工具重新成功。
- 断线后自动重连重发副作用，或浏览器工具流量进入通用响应缓存与事件重放。
- 页面文字、标题或选择器被当作系统指令；工具能执行任意脚本 / CDP / 本机文件。
- 插件文件、配置、日志与报告保存主 token 或浏览器 token；打包后找不到插件。
- 只做假的插件上下文调用或注册检查，却宣称已验证真实 OpenCode 工具执行。
- 全局服务 / 用户配置被测试修改，或测试启动真实账号与不受控的模型请求。
- 新启动与事件恢复路径改变，破坏其他任务和已有私有服务恢复功能。

## 接入契约

- 提供列出共享页面、读取快照、点击、输入、导航五项工具。
- 每次调用使用真实工具执行上下文的 `sessionID`；daemon 根据 driver 创建 /
  恢复成功时建立、随 driver 释放的内存映射，确定 Fintwind 会话与当前
  runtime。模型只选择已共享
  页面及其 grant，不可通过参数指定 Fintwind 会话或扩大权限。
- 子会话首版不继承授权：没有明确映射就拒绝，不从 cwd、标题或父会话猜测。
- 插件仅接收随机、进程范围、可撤销的浏览器专用凭据。专用入口只允许
  list / invoke / cancel，不提供任务、终端、页面发布、通用事件与 shutdown。
- 请求和取消以独立随机请求 ID 匹配；取消、超时和断连均为终态，不自动重试。
  已发给浏览器的动作可能已发生，工具结果须说明重新观察，而不是声称已回滚。
- 插件加载失败不应阻断普通聊天 / 事件恢复：记录固定诊断
  `browser-plugin-unavailable`，不建立浏览器会话映射，从而拒绝工具调用。
  不能把普通聊天成功、注册成功或进程健康等同于页面可操作。
- 仅注入 Fintwind 拥有的私有服务，不修改用户 / 项目配置和公共 OpenCode 服务。
- 先核验本机实际 V2 二进制的插件加载与工具契约，再决定嵌入文件形式；
  不以最新文档代替安装版本的实际行为证据。

### 实现形状

- `resources/opencode-browser-plugin.ts` 编入 daemon，启动私有服务时只生成
  自有随机目录下的无凭据插件文件。配置仅覆盖该子进程的
  `OPENCODE_CONFIG_CONTENT`，保留已有 JSON 内联配置，不改全局 / 项目文件。
- 插件捕获独立浏览器凭据后删除插件运行时中的对应环境变量。配置、工具
  定义、共享列表和结果均不携带该凭据。该机制不是操作系统安全沙箱：
  拥有同一用户的任意本机执行权限的进程仍在可信计算边界内。
- 专用 WS 入口 `/v1/browser-tools` 使用独立版本 1，不改变桌面端 wire 8。
  拒绝网页 Origin，模型输入不能发送通用 daemon 命令或 Fintwind scope。
- 每次工具调用使用独立连接；重连不复发操作。broker 在执行 / 观察的临界区
  检查调用连接仍有效，避免取消、解绑或断连先到后，迟到 worker 才开始操作。
- 插件实例的清理仅取消该实例的调用，避免一个 location 卸载误伤另一个
  location。私有进程退出由后台监视撤销能力；runtime 绑定释放也取消 pending。

## 当前检查记录

- 在 E2E 新文件加入之前，`cargo check --locked --workspace` 与
  `bunx tsc --noEmit --lib ESNext,DOM` 通过；默认 TypeScript 配置未包含 DOM，
  已有 Playwright 页面回调需要显式增加检查库，未为此扩大修改既有配置。
- `bun run browser:collaboration`：全新 profile，21/21 通过。报告：
  `target/browser-bridge-e2e/runs/6da604f7-bc60-4b56-a601-c864a10afa0b/report.json`。
  这是既有 native bridge 的验收，不代表新插件或真实模型已经验收。
- 原有 `opencode_recovery_fast_faults` 首次运行揭示：强制浏览器插件激活会阻断
  不执行插件的 provider fixture。报告保留于
  `temp/recovery-e2e/04f135df8b7749bd85dc44494a64bc57/result.json`。
  已将浏览器激活改为可诊断、拒绝工具的降级，普通聊天不因此失败。
  复验 `opencode_recovery_fast_faults` 的 6 个行为场景全部通过，报告：
  `temp/recovery-e2e/3994c5c3bfdc4adaac39cb849b21d7d9/result.json`。
- 在 E2E 新文件加入之前，`cargo check --locked --workspace --all-targets` 通过；现有测试代码在
  `src/app/usage_page.rs` 有无关的未使用 import 警告，上游
  `proc-macro-error2` 仍有 future-incompatibility 提示，未为本轮修改它们。
- 首轮审查前，范围内生产代码
  `cargo check --locked --package fintwind-core --package fintwind-daemon` 通过；
  插件与真实凭据交接模块的定向 TypeScript 检查也通过。同期全目标与全部
  TypeScript 检查因两个并行 E2E 文件尚未写完而失败，不能作为当前整体通过
  的依据。最终检查 / 行为验收以后续记录为准。
- 第一轮审查完成：移除存活检查中的撤销副作用，由已有后台监视负责；
  常驻服务缺浏览器能力时普通聊天降级、不报启动硬错误。另修正握手与限额
  注释和上述检查记录。多 location、子会话身份与真实 signal 待行为验收。
- 上述首轮修正后，生产 core / daemon 的 `cargo check --locked`、插件与
  `scripts/browser-tools-stepfun.ts` 的定向 TypeScript 检查通过。
  原有恢复 E2E 再次 6/6 场景通过：
  `temp/recovery-e2e/0e1c60b0e8ca4a1ca34086aa275827a7/result.json`。
- 真实 OpenCode + 本机模拟 provider / GUI 的首次工具循环未通过：
  `target/browser-tools-opencode/runs/7c5aae2f-9887-4dc4-9f15-ed83edf105c6/report.json`。
  仅启动 / 发布两项通过，其余 10 项被模型工具快照缺少浏览器工具阻塞。
  该报告不能证明插件已成功执行；注册、激活与可执行工具快照是不同证据。
   当时尚在核验 Code Mode 可见性；后续已确认为默认 Code Mode 池问题，
   不是“纯对象导出无效”。
- 工具循环脚本已补独立数据库覆盖、环境凭据过滤、发布接受等待与真实
  driver 结算等待，第二个会话使用不同工作目录以核验多 location 加载。
  这些脚本修正后的通过结果见下方 `117baf5c` 运行。
- daemon 专用通道 E2E 使用真实 server、broker、registry、transport 和
  `DaemonClient`，GUI owner 与 OpenCode session / runtime 是模拟的。
  代理交付的 4 次 20/20 通过中，已核对两次报告：
  `target/browser-tools-e2e/runs/b8bcb265-a5f3-4bd5-bda4-93ed02f3a11e/report.json`、
  `target/browser-tools-e2e/runs/62840e2e-b0c7-4742-a307-6421acfabf1e/report.json`。
  主代理发现终态断言同时接受超时，随后收紧为“收到拒绝或证实连接关闭”，
  以之后严格断言的复验结果为最终依据；不把模拟页行为当 native 页验收。
- 在已交付两个 E2E 文件并修正 TypeScript 类型后，工作区全目标检查与
  `bunx tsc --noEmit --lib ESNext,DOM` 再次通过。后续正在写的 native 模型
  runner 和本段严格断言修正尚不在这次检查结果内。
- 工具快照根因已由隔离探针证实：v2.0.16 仅把 `options.codemode === false`
  的工具放进直接模型快照；不写 options 的插件工具进入 Code Mode 池。
  同一工具实测无 options 时 12 个直接工具、无浏览器工具；显式 false 时
  13 个直接工具、浏览器调用完成。生产插件已对五项工具显式设为 false，
  不叠加 namespace，避免双前缀。探针产物：
  `C:\Users\Public\Temp\opencode\codemode-probe-ce5d6903f6e6/REPORT.md`。
  该修正后的首轮复验记录为下方 `9bac3c0c`，最终通过为 `117baf5c`。
- 修正直接工具可见性后的运行
  `target/browser-tools-opencode/runs/9bac3c0c-994a-4d5e-840a-ef0254ebbd68/report.json`
  仍未通过：模型能调用浏览器工具，但 daemon 缺少 session binding。隔离日志
  证实旧 `/api/plugin/await-activation` 路由为 404，不能用于激活判定；且
  多 location 重新 import 插件后读取不到已删除的环境凭据。后者已改为
  进程范围非枚举内存缓存，再删除环境变量，避免凭据继承到普通子进程。
- 专用通道终态断言收紧后，21/21 行为检查连续三次通过（新增并发回包保存
  场景）：`target/browser-tools-e2e/runs/037f204e-63e8-4645-b18a-b2f07d62f1c7/report.json`。
  只接受真实拒绝或证实连接关闭，超时不通过；清理 7/7，源 / exe SHA256
  已用独立工具核对。它仍不是 OpenCode 工具 / WebView2 / 模型执行证据。

### 第二轮审查前的真实工具与模型验收（2026-10-01）

- 专用通道补充“大列表有界拒绝”后 **22/22 连续三次通过**。16 页合法 URL
  形成 69,536 字节列表时，149 字节拒绝信封不含页面 URL，随后正常列表仍可
  读取。最终报告与源 / exe SHA256：
  `target/browser-tools-e2e/runs/17be1e1f-af36-498a-8b44-5cf26efffe34/report.json`。
- `GET /api/plugin` 带正确 directory 查询真实插件状态，以 `fintwind.browser`
  的 `state.status === active` 收敛为准，空数组表示尚在加载；10 秒有界等待。
  不再使用不存在的 `/api/plugin/await-activation`。状态探针：
  `C:\Users\Public\Temp\opencode\plugin-state-probe-9ef431e076ed/REPORT.md`。
- `bun run browser:tools-opencode` **12/12 通过**。真实私有 OpenCode / driver /
  插件 / daemon / broker，provider 与 GUI 是本机模拟。覆盖五项工具执行、
  不可信结果包装、两个不同目录的会话隔离、逐次审批 / 拒绝、真实 interrupt
  传递取消信号、owner 断连不重发。报告：
  `target/browser-tools-opencode/runs/117baf5c-5868-4eac-8cb0-9bb238caf9f3/report.json`。
- 用户已授权的 `bun run browser:tools-native --skip-build` **7/7 通过**。
  StepFun `stepfun-ai-step-plan/step-5-preview`、`low` 档位真实调用，真实
  WebView2 PoC 宿主的本机三页 fixture，非完整产品 GUI。模型读取到未在 prompt
  提供的可见随机 marker；密码 / 隐藏值 / Cookie 未出现在模型侧工具记录中。
  点击及输入仅批准后执行，原生事件 `isTrusted` 为 true，未串页；真实 Cancel
  撤掉审批，人工 revoke 后迟到批准无效。清理全成功，errors 为空。报告：
  `target/browser-tools-native/runs/9126a57f-c777-40ac-b28a-e510b3ea10e9/report.json`。
- 版本：OpenCode `v2.0.16`、Bun `1.4.2`、WebView2 `154.0.4258.48`、桌面
  协议 8 / 浏览器工具协议 1。报告包含宿主、daemon example、插件 SHA256。
  StepFun 仅从已有选中账号只读取 key，通过独立子进程环境传递，不复制用户
  DB，不改全局配置，不把 key 写到配置 / 报告；原生宿主不继承该 key。
- 插件状态等待与进程内凭据缓存修正后，既有私有服务恢复 E2E 再次 **6/6**
  场景通过：`temp/recovery-e2e/32ab1df61f674e86ac45318cd2734423/result.json`。
- 本机模拟 provider 驱动真实 OpenCode `subagent` 引擎的补充探针，确认插件
  `execute` 的 `context.sessionID` 等于引擎创建的 child session ID，不等于
  parent session ID；真实 context 同时包含 `AbortSignal`。结合专用通道的
  未映射子会话拒绝测试，父授权不会因工具执行身份被替换而隐式继承。
  证据：`C:\Users\Public\Temp\opencode\child-session-probe-cc8e1b36d59d/REPORT.md`
  与 `findings.json`。未验证真实 Fintwind 子会话到原生页完整链路、子会话
  续跑、后台子任务或孙会话，首版仍不新增子会话授权传播。
- 最终审查前代码状态：`cargo check --locked --workspace --all-targets`、
  `cargo fmt --all -- --check`、`bunx tsc --noEmit --lib ESNext,DOM` 通过。
  无关既有 import 与上游 future-incompatibility 警告未修改。
- 尚未完成完整产品界面共享按钮 / toast / connection supervisor 的 E2E；
  原生测试宿主行为不能代替该验收。真实导航与真实子会话身份分别以五工具
  本机模型循环 / 上述补充探针证据为准，不宣称本次 Step 模型调用覆盖了它们。

### 可重复运行

```sh
bun run browser:tools
bun run browser:tools-opencode
# 会发起已授权的真实 StepFun 请求；需先有 browser:poc 构建，可能产生费用。
bun run browser:tools-native --skip-build
```

`browser:tools-opencode` 自动构建隔离 daemon example；`--skip-build` 可复用
同一构建。native runner 可用 `FINTWIND_OPENCODE_BINARY`、
`FINTWIND_STEPFUN_CREDENTIAL_DB` 或 `--credential-db` 指定已确认的路径，
可用 `FINTWIND_STEP5_VARIANT` 选择档位。失败报告保留，不覆盖、不放宽断言。

## 验收与审查

优先使用隔离 profile、本机 fixture、真实 Rust server / broker / WebView2
与真实 OpenCode 私有进程。保留逐项 JSON 报告、实际版本与构建哈希。区分
插件加载、真实工具执行、原生页面行为和真实模型调用四种证据，不混为一谈。
不添加实现后的单元测试，不做截图或像素测试，不启动 dev watcher。

第三阶段真实 provider / 原生页面验收及两轮审查已完成；
用户授权仅限 Step 5 Preview 隔离会话和本机页面，不扩展为网页账号读取或
项目代码上传权限。本次最多两轮审查，
执行检查后再审查；第二阶段的两轮结果不替代第三阶段审查。不自动提交或推送。

第二轮审查已完成，无已确认的越权 / 需求违背发现。指出的两项收尾问题及修正：

- OpenCode 自己可能把环境 key 写入测试隔离 DB / WAL / 日志；只扫描报告目录
  不能证明无残留。停止自有进程后对该 run 的隔离根目录做有界、逐块二进制
  扫描（含 DB），结果写报告，随后仅删除该 run 自己创建的 UUID 目录。
  不能跟随符号链接、删除父目录 / 用户 DB，不能把 key / 匹配字节写报告。
- JSON 转义或多字节输入可能超出消息预算。插件发送前校验 UTF-8 字节数，
  输入过长明确拒绝；不能降级为自动重试，不能在校验前打开动作连接。
- 上述修正后复验，不再增加第三轮审查；先前报告保留并注明对应代码状态。

### 第二轮审查后的复验

- 增加插件发送前的 8 KiB UTF-8 输入 / 32 KiB 序列化信封校验后，真实
  OpenCode + 本机模拟 provider / owner **13/13 通过**。新增检查确实经真实
  工具执行收到两种明确大小拒绝，owner 没有新增审批或动作，不重连 / 重试。
  报告：`target/browser-tools-opencode/runs/a14c0a41-8645-4dbc-abe2-adc09fff840d/report.json`。
  修正 fixture 类型前的失败报告 `7629cbd3-40f4-4ce9-accb-abc9b41ec199`
  保留，不计入通过证据。
- 上一轮原生模型运行的隔离根目录已按新逻辑补扫，包括 DB / WAL 等二进制
  文件，9 个文件 / 19,174,184 字节中未命中所选 StepFun key，随后删除仅该
  run 的 UUID 目录。没有重新发送模型请求。补充记录：
  `target/browser-tools-native/runs/9126a57f-c777-40ac-b28a-e510b3ea10e9/isolation-cleanup.json`。
- 原生 runner 已接入自动扫描与删除；SDK 若持久化已知凭据、扫描不完整或
  进程无法停止，报告均判为失败。只保留无凭据验收报告，不保留 SDK 隔离 DB。
  原 `9126a57f` 模型行为报告对应改动前插件；新插件与自动清理的复验结果
  独立记录，不覆盖原报告。
- 新插件与自动隔离清理的真实 Step 5 Preview / WebView2 复验 **7/7 通过**，
  errors 为空、cleanup **4/4**。扫描 9 个文件 / 19,174,725 字节，含隔离
  DB / WAL，未命中所检查的 StepFun key 或 daemon token，隔离根目录已删除。
  报告：`target/browser-tools-native/runs/7465e5c4-1e16-4b6a-8db8-a11be6239bf4/report.json`。
  当前插件 SHA256 与报告 `pluginSourceSha256` 一致（`299009de…9644ef6`），
  daemon example 哈希与本轮 13 项真实工具循环相同（`48ea4c3b…422bd5`）。
- 修正后专用通道再跑 **22/22**，startup / cleanup 各 **7/7**：
  `target/browser-tools-e2e/runs/9818729b-6c76-415e-8c8c-915392000494/report.json`。
- 修正后既有私有服务恢复 **6/6** 场景再次通过：
  `temp/recovery-e2e/f2efda1fcae34b7da2c2619864c6e2c5/result.json`。
- 收尾代码状态（上述发送前检查、隔离扫描 / 删除及 fixture 修正已包含）：
  `cargo check --locked --workspace --all-targets`、`cargo fmt --all -- --check`、
  `bunx tsc --noEmit --lib ESNext,DOM` 通过；既有无关警告未更改。
  第二轮审查结束后仅修正上述两项并复验，没有第三轮审查；没有提交或推送。

## 手工验收交接

完整产品界面的最终实际测试按用户要求由用户自行完成；不新增自动化控制
入口，也没有额外进行第三轮审查。手工清单见
[browser-manual-acceptance.md](browser-manual-acceptance.md)。

- 2026-10-01 收尾：为完整 GUI 自动验收临时添加的 feature、控制入口和
  观察辅助代码已全部撤回，正常产品文件无残留内容改动；保留此前核心 WIP。
- 正常产品 `cargo build --locked --package fintwind --bin fintwind
  --package fintwind-daemon --bin fintwind-daemon` 成功，不启用 PoC / 产品验收
  feature，产物为 `target/debug/fintwind.exe` 与
  `target/debug/fintwind-daemon.exe`。没有启动、替换或关闭正在运行的安装版。
- 此代码状态再次通过 `cargo check --locked --workspace --all-targets`、
  `cargo fmt --all -- --check`、`bunx tsc --noEmit --lib ESNext,DOM` 及
  `git diff --check`；仅有既有无关警告，没有重新进行模型或完整 GUI 测试。
- 本次产物 SHA256（后续重建可能变化）：
  - `fintwind.exe`：`918e9f60cf4235667af36eff4c14ce7c9f71f56bd926ada5b81b3568a48eb852`
  - `fintwind-daemon.exe`：`d86fbe4bb74d4185e59f0bca08c12b16abfb863325bda536d6f596a55fda4870`
- 未提交、未推送。完整应用的控件交互、连接恢复提示及真实模型导航仍等待
  用户实际验收；上述构建成功不将这些项目标为已通过。

## 资料来源

- https://opencode.ai/v2/docs/build/plugins （工具 transform、sessionID、signal）
- https://opencode.ai/v2/docs/build/plugins/rpc （插件 RPC 与取消）
- https://opencode.ai/v2/docs/plugins （插件发现与配置）
- `docs/browser-collaboration.md` （已完成的第二阶段契约与验收边界）
- 本机 PATH 二进制 `E:\bun\bin\opencode.exe` 当前报告 `v2.0.16`；
  隔离探针确认默认导出 `{ id, setup }` 可直接加载，无需 npm 依赖；配置入口
  必须指向插件目录，不能照最新文档写裸 `.ts` 文件路径。工具执行必须走
  真正 agent loop，没有 HTTP 通用 tool invoke 端点。
- 探针资料：
  `C:\Users\Public\Temp\opencode\probe-v2016-5b13cc5c520a/REPORT.md` 与
  `findings.json`。本轮验证版本为 `v2.0.16`，不宣称更早版本均支持。
