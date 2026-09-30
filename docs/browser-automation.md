# 内置浏览器 AI 协作：同页操控验证

本阶段只验证 Playwright 能否操控真实的 `BrowserView` / WebView2
composition controller，不注册 AI 工具，不启动 OpenCode 或 daemon。
后续的会话授权、审批、接管与日志工具必须建立在验证结果之上。

## 先验证的失败模式

- 连接到另一个浏览器进程、错误标签或已有用户 profile。
- 读取快照成功，但点击、输入不是可信输入，或自动等待不能处理动态页面。
- 跨源 iframe、同文档导航、弹窗不能操作，或宿主 URL / 标题没有同步。
- 读取、导航、点击或输入抢占 GPUI 键盘焦点。
- Playwright 断开连接关闭了宿主页面，重连后映射失效。
- 宿主关闭一个页面后，留下孤儿 target 或影响其余页面。
- 启动失败、超时、取消或清理失败时，没有留下可核查结果。
- 调试端口在普通构建中被意外打开，或调试参数污染用户登录数据。

行为验证使用本机 HTTP 测试页面和一次性测试 Cookie；不访问真实账号、
不使用 dev watcher、不进行视觉比对。截图能力必须另行显式开启。

## 运行

仅 Windows，已安装 Rust、Bun 和 WebView2 Runtime：

```sh
bun install --frozen-lockfile
bun run browser:poc
```

脚本使用固定版本的 `playwright-core`，不下载另一套 Chrome。
验证构建使用 `browser-poc` feature 和独立的 `target/browser-poc` 目录，
不会替换 watcher 使用的 `target/debug/fintwind.exe`。普通构建不包含验证入口。
脚本不继承 `WEBVIEW2_*` 环境覆盖；直接启动验证入口时也拒绝这些覆盖。
验证宿主还会保守拒绝存在 WebView2 注册表策略的环境，并校验实际生效的
user data folder，避免企业策略把调试端口带到用户真实 profile。
这可能使受管理的机器无法执行本阶段验证；不要为测试删除或修改系统策略。
创建环境后的目录核对只是一层兜底，不能代替创建前的环境与策略检查。
验证宿主最长运行十分钟；即使 runner 被强制关闭，测试调试端点也不会无限期保留。
运行时会打开并激活一个独立测试窗口，请勿最小化或遮住它。

每次运行产生 `target/browser-poc/runs/<UUID>/`，包括报告、宿主状态、进程日志
和独立的 `profile/`。重复运行不会复用 Cookie，也不会清理其他运行的产物。
已完成编译时可用 `bun run browser:poc --skip-build`。
仅需要验证截图 API 时使用 `bun run browser:poc --screenshots`，不进行像素比对。

报告中的失败就是能力限制，不通过强制重置焦点等手段掩盖。
后续是否采用 Playwright 为操作引擎，以报告的实际结果为准。
高层点击与输入的验收对象是可见的原生页面，隐藏标签和最小化窗口不在通过范围内。

## 本机验证结果（2026-09-30）

环境：Bun 1.4.2、Playwright 1.63.0、WebView2 Chromium 154.0.4258.37。
两个独立的新 profile 运行均为 `passed`，各有 16 项行为检查通过，截图项跳过。
第二次还确认关闭 beta 后 alpha 和 gamma 均能继续接受输入。

| 验证范围 | 结果 |
| --- | --- |
| 三页准确映射、结构化 ARIA 快照、不串页 | 通过 |
| 新 profile 无遗留 Cookie，测试页共享合成 Cookie | 通过 |
| 可信点击 / 输入、动态等待、遮挡处理、跨源 iframe | 通过 |
| 同文档导航的宿主 URL / 标题同步 | 通过 |
| 读取、导航、点击、输入不取得原生键盘焦点（含事件计数） | 通过 |
| 按需读取 console / HTTP 404 诊断 | 通过 |
| 弹窗沿用本页导航、没有孤儿 target | 通过 |
| Playwright 断开不关闭宿主页面，重连可继续操作 | 通过 |
| 宿主关闭一个页面后，其余已验证的页面仍可操作 | 通过 |
| 错误配置与继承的 profile 覆盖被拒绝 | 通过 |

可核查的本地产物（`target/` 不提交）：

- `target/browser-poc/runs/b65475dc-f9ac-498c-b140-03ecbbd0b66c/report.json`
- `target/browser-poc/runs/2b0cf469-e9dd-44c2-b633-db30df9e2603/report.json`

两次使用同一宿主二进制；报告保留其 SHA256、依赖版本、逐项结果和耗时。
每个目录还包含 `host-state.json`、ARIA 快照、诊断与进程日志。
前期失败报告也保留，未删除或改写为成功。

本轮发现 runner 的 `windowsHide: true` 启动配置会使验证 GUI 停止绘制，
造成后创建的页面没有原生尺寸、保持隐藏，高层点击等待超时。
显示并激活测试窗口后，两次复跑均通过；没有使用 `force` 点击、注入点击事件
或在每次操作后强行恢复焦点来绕过失败。

结论：Playwright 同页操作在此可见窗口环境中可行，可进入协作桥接阶段。
这不是对隐藏页、任意站点、生产授权或真实 Agent 工作流的兼容保证。
弹窗仍按应用现有策略在本页打开，不表示已实现自动新建标签。

## 权限边界

CDP 是完整浏览器控制接口，而不是只读或逐标签授权接口。
回环地址和随机端口不能阻止其他本地进程访问；带 token 的前置代理也不能
阻止绕过代理直接连接原始端口。因此本阶段的调试端口只用于隔离验证，
不能作为生产版浏览器工具的授权实现。

WebView2 COM 调用保留在创建宿主的 STA/UI 线程，异步完成。
文件、网络、编码和结果整理不进入渲染路径。诊断页面 ID 仅用于本机测试
映射，不作为未来生产授权凭据。

## 后续顺序

1. 根据本机验证结果确定操作适配器及其限制。
2. 实现客户端 / 会话 / 标签绑定、可撤销授权、取消、单步审批与人工接管。
3. 将页面、元素和调试日志接入 OpenCode 工具与对话上下文。

基础资料：

- [Playwright WebView2](https://playwright.dev/docs/webview2)
- [CDP 接入与 noDefaults](https://playwright.dev/docs/api/class-browsertype#browser-type-connect-over-cdp)
- [WebView2 原生 CDP](https://learn.microsoft.com/en-us/microsoft-edge/webview2/how-to/chromium-devtools-protocol)
- [WebView2 STA、异步完成与不可重入约束](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/threading-model)
