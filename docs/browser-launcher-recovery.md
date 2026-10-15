# 浏览器启动器与授权排查

2026-10-15。用户在本机「完全访问 / 构建」会话调用 `fintwind_browser_open`
时收到 `no live browser launcher is registered for this session runtime`。

## 原因与修正

GUI 的 `publish_browser_host` 只保证消息进入发送队列。OpenCode 工具通过另一条
连接请求打开页面，可能先于注册消息到达 daemon。原实现立即拒绝，晚到的注册
也无法挽回已失败的调用。整体 daemon 黑盒检查已复现这个顺序问题。

另一个独立缺口是启动器注册依赖完全访问和未暂停状态。断连重绑会设置暂停，
而监督模式根本不注册启动器。因此请求到不了 GUI 的授权判断，也无法主动申请
打开新页面。截图没有记录当时的连接事件，不能判断该次故障具体命中了哪条路径。

当前实现分开处理路由与授权：

- 当前选中的本机构建会话具有有效 runtime 时注册启动器，页面权限仍单独校验。
- daemon 最多等待两秒接收该 session/runtime 的真实注册。等待不会发送页面动作，
  不持锁休眠；取消、断连或 runtime 更换会终止等待。成功后仍经原有精确 scope
  复核，只发送一次打开请求。
- 完全访问且未被用户暂停时自动打开新页。其他情况显示网址并请求本次授权，
  批准后创建监督页面，保留逐次操作审批。
- 断连撤销旧页面授权，重连后新页遵循当前访问模式。人工接管则保持暂停，
  新页须重新批准，不恢复旧页面授权。
- 审批过期、取消、切换会话或连接失效后，迟到的批准不创建页面。

现有 daemon 动作预算为 30 秒。新页面审批等待 10 秒，加载等待最多 15 秒，
为消息传递与撤销保留余量；本次没有扩大所有浏览器动作的超时。

## Codex 与智谱 ZCode 的参考

Codex 的本地命令权限和浏览器权限分别管理。公开文档说明了浏览器网站权限，
没有公开本机 browser host 的内部注册和恢复协议，不能据此推断其握手机制。
参见 [Codex 权限文档](https://learn.chatgpt.com/docs/permissions)和
[浏览器文档](https://learn.chatgpt.com/docs/browser?surface=app)。

智谱 ZCode 的公开源码核对版本为
`aac4755666d09fdcd70272fcf063c077a639015f`：

- [browser-client](https://github.com/zai-org/ZCode/blob/aac4755666d09fdcd70272fcf063c077a639015f/apps/zcode-cli/packages/browser-use-plugin/src/browser-client.ts)
  检查宿主注入的 bridge，再初始化浏览器运行时。
- [desktop bridge](https://github.com/zai-org/ZCode/blob/aac4755666d09fdcd70272fcf063c077a639015f/packages/desktop/src/host/browserControlMainBridge.ts)
  可以先返回 IAB descriptor；descriptor 可见并不证明页面已经可操作。
- [guest manager](https://github.com/zai-org/ZCode/blob/aac4755666d09fdcd70272fcf063c077a639015f/packages/desktop/src/main/browserView/browserGuestManager.ts)
  在新建标签时等待实际 guest attach，失败时清理临时标签。
- [permission flow](https://github.com/zai-org/ZCode/blob/aac4755666d09fdcd70272fcf063c077a639015f/apps/zcode-cli/packages/core/src/tool/executor/permission-flow.ts)
  根据执行模式和规则决定允许、询问或拒绝，通过 broker 异步等待 UI。
  浏览器 API 位于 `node_repl` 的 `js` 调用中，不能把一次工具审批误写成每次点击
  都有单独审批。

这些参考支持区分工具可见、宿主可路由和页面可操作。Fintwind 的两秒上限用于
等待真实注册，不以延时结束作为就绪证明；页面仍须加载完成才能返回页面授权。
本次保留 Fintwind 的独立浏览器工具和既有监督审批，没有迁移为 Node REPL。

## 验证与边界

新增场景复用 `cargo run --locked --package fintwind-core --example browser_tools_e2e`，
运行真实 daemon、broker、registry 和 transport，GUI owner 为协议模拟。

旧 broker 的迟注册场景失败，报告 `e2775252-c6d2-4d9b-924d-9c52d6d590f2` 中记录
`the fake owner observed the expected event did not happen within 12s`。
该次基线另有断言关键词遗漏和后续清理导致的失败，不把全部失败归因于本缺陷。

最终报告为
`target/browser-tools-e2e/runs/1f3039cd-15ec-47da-beca-973984742877/report.json`，
33/33 通过，已核对报告中的源码与可执行文件 SHA256 与实际文件一致。
新增覆盖迟注册、等待中取消后不执行，以及无宿主时限期返回可采取的错误。

Rust 工作区全部目标检查、浏览器插件定向 TypeScript 检查和改动文件格式检查通过。
项目根目录的全量 TypeScript 检查会纳入 `target` 中历史探针，存在无关错误。
没有运行完整产品 GUI、真实 provider 浏览器交互或视觉验收，黑盒通过不能代替这些验证。
