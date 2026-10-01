# 浏览器自动化产品缺口修正

> 以下记录保留上一轮实现与验证状态。用户随后反馈“UI太丑，人一动页面就
> 失去共享，点击也无效”，新的交互契约见
> [browser-interaction-fixes.md](browser-interaction-fixes.md)：不再将普通聚焦 / 滚轮 / 点击
> 视为接管，只有显式停止才撤销。下文旧的聚焦撤销报告不作为新交互验收结果。

## 用户反馈与已确认方向

用户实测反馈：“每次操作都要授权，算什么自动化。而且连浏览器都打不开。
工具也用不对。”截图中「完全访问」已选择，但模型先报桌面浏览器未连接，
之后通过 Fintwind 工具读到页面，又因选择器不可靠去抓网站源码。
用户确认选择「跟随完全访问」：完全访问下可自动打开本会话专用标签并连续
操作；其他手动标签仍须明确共享。最终完整产品实际测试继续由用户执行。

## 写实现前列出的失败模式

- 只改工具说明，实际上没有从真实上下文到 GUI 的打开入口。
- 同时暴露 OpenCode 内置桌面 browser 与 Fintwind 页面工具，模型连接错宿主。
- 没有 id 的按钮 / 链接仍没有可操作目标；猜选择器，或对陈旧目标操作另一元素。
- 完全访问仍逐步审批，或者简单去掉全部授权，串会话、操控未共享的手动标签。
- 自动化标签跨导航失去授权，每一步都要求重新共享；反过来旧文档引用仍有效。
- 授权降级、切换会话、接管、关闭、断连后仍能打开 / 操作或自动恢复旧授权。
- 新开页面的超时、取消和加载失败被标为成功；取消后迟到加载又自动共享。
- 浏览器打开能力成为通用 daemon token、任意脚本、任意 CDP 或本机文件入口。
- UI 帧注册主机、轮询文件或阻塞 RPC；网络、等待和内容整理阻塞 UI。
- 只能加载新标签但没有显示面板；WebView2 把键盘焦点抢走触发立即接管。
- 快照读敏感字段，或把第三方网页文字视为工具 / 系统指令。
- 敏感字段正文已过滤，但用控件的包裹 label / aria-labelledby 读取原始
  textContent，间接把 textarea 的默认内容或隐藏字段文本当成控件名称导出。
- 以成功构建、模拟 owner 或旧报告声称完整用户界面已验收。

## 本轮实现契约

- 新增 `fintwind_browser_open`，仅经真实 `context.sessionID` 映射到当前 runtime。
  GUI 显式注册一个连接范围、当前会话 / runtime 范围的打开能力；不是查找
  全局当前窗口，多个候选不猜测。不打开 / 共享其他手动标签。
- 自动打开面向「完全访问 + 构建」的本机选中会话；计划模式不获得自动操作
  能力。新标签共享同一浏览器 profile，不将专用标签误称为账号 / Cookie 隔离。
- 完全访问下，共享页面连续自动执行；切到非完全访问立即撤销自动授权，
  原有手动共享 / 逐次审批路径保持可用。断连、切会话、runtime 更换和人工
  接管均撤销，不重放，不自动重试副作用。
  新 runtime 可以根据仍生效的完全访问设置登记全新打开能力；已撤销的页面
  和待处理动作不继承。旧 cursor 尚未更新时，不重复发布被退役的打开能力。
- 自动权限绑定标签，文档导航不要求重新共享；每次导航仍使旧文档操作守卫
  与元素引用失效。普通手动共享的原有导航撤销行为保留。
- 快照提供无需网页 id 的文档范围元素引用；操作优先使用观察所得引用，
  陈旧 / 脱离文档的引用明确拒绝。可见性与敏感字段过滤继续保留。
  插件可以接收清晰的 `ref` 参数，但传入原生动作时统一规范化到 `selector`；
  不增加一个只有模拟宿主支持、真实原生协议拒绝的平行动作格式。
- Fintwind 的私有 OpenCode 服务去掉无法连接本宿主的内置 `browser` 工具，
  并添加临时模型上下文说明；不改用户全局配置、不卸载用户 MCP 服务。
- 参考成熟接口的观察 → 引用操作 → 再观察与有界等待，不宣称等价于其
  完整框架；保留原生 WebView2，不新增公开远程调试端口。
- 打开过程中取消、加载失败或用户聚焦页面，均不得在迟到加载后自动共享。
  接管 / 关闭自动标签会同时停止当前打开能力，避免 AI 开替代标签绕过停止。

## 参考（2026-10-01 读取实际接口）

- Microsoft Playwright MCP：
  https://github.com/microsoft/playwright-mcp （结构快照、精确元素引用、导航、
  动作后观察、有界动作 / 加载等待；不照搬其任意执行能力）。
- Browser Use `browser_use/tools/service.py`：
  https://github.com/browser-use/browser-use （新标签导航、按观察所得元素索引
  点击 / 输入、引用失效时重新观察；不照搬导航重试或另建 Agent）。
- OpenCode V2 plugins：https://opencode.ai/v2/docs/build/plugins
  （tool transform remove、context hook；需真实 v2.0.16 验证）。

Context7 本轮因 OAuth token 过期不可用，改读官方 V2 文档；未修改账号配置。
本轮最多两次针对新修正的审查，不重新展开已完成的旧第三阶段审查。

## 本轮验证范围与可重复命令

以下是验证命令，不将历史通过复用为本轮结果。新结果只记录到对应本轮报告。

| 命令 | 验证范围 |
| --- | --- |
| `bun run browser:tools` | 专用 transport、真实会话绑定、打开能力路由及取消 / 撤销 / 串会话拒绝。 |
| `bun run browser:tools-opencode` | 本机真实 OpenCode + 隔离模拟 provider 的工具循环、七工具声明与内置错误宿主工具移除。 |
| `bun run browser:collaboration` | 独立原生 PoC：原有监督模式与新增自动模式、无 id / 离屏引用、可信输入、导航及点击链接跨文档、接管和降级，共 30 项行为检查。 |
| `cargo check --locked --workspace --all-targets` | 正常配置 Rust 编译检查；不代替运行行为。 |
| `cargo fmt --all -- --check`、`bunx tsc --noEmit --lib ESNext,DOM`、`git diff --check` | 格式、TypeScript 类型与差异检查。 |

原生 PoC 只验证真实 WebView2 适配器，不运行完整 Fintwind 产品界面自动验收；
模拟 provider 不代表 GLM-5.3-Flash 等真实模型已正确选择工具。最终正常应用
的完全访问设置、直接打开与完整任务体验，继续由用户按手工文档验收。

### 已执行的本轮通道验证

- `cargo run --locked --package fintwind-core --example browser_tools_e2e`：
  30/30，通过。桌面协议 9、浏览器工具协议 1。
- 报告：`target/browser-tools-e2e/runs/ca842497-2598-4207-8080-cab4af607a88/report.json`。
- 范围：当前 core / protocol / client 打开通道，包括无宿主拒绝、精确打开路由、
  错误连接应答拒绝、取消先到、宿主撤销、串会话 / 旧 runtime 拒绝、page 和
  launcher 能力隔离，以及真实 `deltaY` wire 字段与滚动边界。
- 该报告不覆盖 GUI 的完全访问设置、真实新标签打开或真实模型工具选择。
- 格式化后对当前 core 树重新运行 `bun run browser:tools`，仍 30/30，通过；
  最新报告：`target/browser-tools-e2e/runs/5f0b6e8e-099d-4296-808f-919e880d1710/report.json`。

### 已执行的本轮 OpenCode 工具循环

- `bun ./scripts/browser-tools-opencode.ts`：20/20，通过；本机真实 OpenCode
  `v2.0.16` 与真实 daemon / driver，provider 和 GUI 是隔离模拟宿主。
- 报告：`target/browser-tools-opencode/runs/725cf588-d3fe-4baf-8ec3-54438cf2c9ed/report.json`。
- 模型请求中声明七个 Fintwind 工具，未出现那 45 个错误桌面宿主的内置工具；
  插件上下文指导到达 agent-loop。`ref` 在插件内被规范化为原生 `selector`，
  没有一个只能通过模拟宿主、却被真实 native 协议拒绝的额外字段。
- 本次宿主未注册 launcher，`open` 真实到达 daemon 并按预期拒绝；该结果
  证明工具路径与缺权限时拒绝，不证明完整产品已成功打开页面。
- 收尾对当前源码再运行 `bun run browser:tools-opencode`，仍 20/20，通过；
  最新报告：`target/browser-tools-opencode/runs/76657b8b-acb4-492b-b499-09a6d1379243/report.json`。
- 旧 `browser:tools-native` 保留监督模式真实模型场景，明确禁用新增 open /
  scroll；本轮不重新发起真实模型调用、不将旧报告计入本轮自动模式通过。

### 原生首次运行发现的问题（未当作通过）

- `target/browser-bridge-e2e/runs/41ad08ee-2782-45e5-8e25-5bceb3ae746c/report.json`：
  当时 29 项中 28 项通过，连续导航一项失败；保留报告，不覆盖或删除。
- 原因：自身导航替换文档后，最终应答仍要求旧文档 guard 有效，把已经发出的
  导航误报为授权取消。点击导航链接的最终鼠标释放也有同一终态问题。
- 修正将“后续步骤必须仍在旧文档”与“最终输入已经发出”区分；发出前仍校验
  权限、可见性和取消，中间输入步骤仍受旧文档限制，显式取消 / 超时仍报结果
  不确定；最终只应答 `issued` 与 `requiresObservation`，不宣称页面加载成功。
- 重跑增补点击页面链接跨文档后的真实观察与后续操作；当前完整应用 GUI
  验收边界不因这个适配器检查变化而改变。

### 当前原生适配器验证

- `bun run browser:collaboration --skip-build`：30/30，通过，清理全部通过；
  使用本轮此前刚构建的独立 PoC 宿主，不是旧正常程序。
- 报告：`target/browser-bridge-e2e/runs/e886d60a-9300-41ec-b127-bb7051b78164/report.json`。
- 原生宿主 SHA256：`08b7bdd30642c2585796b7fa08653160105673b64de192224739fb7dc5fe09d5`。
- 同一宿主再运行加强后的模型式顺序（导航应答后立即 snapshot，不由 runner
  先等待页面完成加载），仍 30/30，通过；最新报告：
  `target/browser-bridge-e2e/runs/a277fe23-e505-4c64-b1a4-dec20854c0c4/report.json`。
- 已证实：无 id 控件按观察所得引用点击 / 输入，可信事件、无需审批、滚动和
  离屏目标操作，导航及点击链接后同标签继续操作，旧引用拒绝，权限降级与
  人工聚焦撤销；原有监督模式 21 项也保留通过。快照验证覆盖 label 包裹
  textarea 默认内容不泄漏。
- 中间一次 runner 已预加载修正前的 fixture 变量拼写，导致页面生成失败，
  报告 `target/browser-bridge-e2e/runs/6a0c1a26-bc63-4265-b897-bf15e44da201/report.json`
  保留，不当作通过；正确脚本的新进程重跑才产生上述 30/30 报告。

### 正常产品构建与静态检查

- `cargo check --locked --workspace --all-targets` 与加 `--features browser-poc`
  的检查均通过；`cargo fmt --all -- --check`、
  `bunx tsc --noEmit --lib ESNext,DOM`、`git diff --check` 通过。
- 保留既有 `ModelsDevCost` 未使用及依赖 `proc-macro-error2` future-incompat
  警告；未为了本轮修正改无关代码。
- 正常构建通过：
  `cargo build --locked --package fintwind --bin fintwind --package fintwind-daemon --bin fintwind-daemon`。
- 本次正常产物（不启用 PoC、不自动运行、不替换安装版）：

| 文件 | SHA256 |
| --- | --- |
| `target/debug/fintwind.exe` | `e560ca5ad338e0f1dc33577f634bc17a216ac441e4756614ee6fdd0c43aa4d57` |
| `target/debug/fintwind-daemon.exe` | `1a69c4088f19b7f154a2296976bc17d7c2f5e0866e084fa67a574d08eecadf85` |

这些正常产物的完整产品 GUI 测试仍由用户执行；未新增完整产品验收入口、
未启动 watcher、未执行截图 / 视觉测试，未自动提交或推送。

### 本轮代码审查（1/2）

- `code-reviewer` / Step 5 Preview 对当前新修正 WIP 完成 Standards + Spec
  审查；未发现需修复的功能性 bug 或重大漏洞，需求项无缺失。
- 记录一项 P3：终态 `issued` 守卫只区分显式取消，不独立区分同步原生
  聚焦与自身文档替换。在极短的回调竞态下，已发出的动作可能返回 `issued`
  而不是取消错误；它不表示页面加载完成，之后页面授权与打开能力仍会撤销。
  审查未发现可验证越权；不为该提示进一步扩大本轮实现。
- 审查对应最终当前工作区。正常产品打开的全部 GUI 路径仍缺行为验收；
  审查没有将代码阅读 / 编译或隔离模拟当作这条路径已经实际通过。
- 本轮只进行这一轮新修正审查，没有重开旧第三阶段第三轮审查。
