# Changelog

All notable changes to Fintwind. This file is the **source of truth for the
release notes published with each GitHub release**: the Release workflow
extracts the section whose heading matches the version being released and
publishes it as the release body.

Format follows [Keep a Changelog](https://keepachangelog.com). Add a new
`## [<version>]` section at the top for each release, matching the version in
`Cargo.toml`.

Fintwind versions are independent of [waku](https://github.com/egoist/waku).
Do not copy upstream version numbers into this file.

Write release notes for the final product users receive, not the development
history. When a feature is still unreleased, fold its fixes and refinements into
the original feature bullet instead of adding separate entries for them.

## [unreleased]

## [0.2.6]

- 全新双层侧栏：最左侧为固定图标栏（会话、审阅、设置），第二栏随模式切换；窗口顶部新增贯穿全宽的顶栏，窗口控件固定贴右，不再与会话标签争抢宽度
- 新增独立的「审阅」页，取代原右侧面板的审阅入口：可选择跟随会话或任意已记录的工作目录，按「已暂存 / 未暂存」分组，支持单文件与全部文件的暂存、取消暂存、丢弃（删除未跟踪文件保留单独确认弹窗）
- 审阅页 Diff 支持并排 / 统一视图切换与文件间导航；查看历史快照时写操作控件自动隐藏
- 新增提交对话框：可只提交已暂存内容，或暂存全部并提交；支持用编码代理按已暂存内容生成提交信息，以及提交并推送
- 分支行提供刷新、Fetch、Pull（仅快进）、Push 与分支切换
- 用量从设置分类移出，成为第一栏的独立模式，页面全宽渲染；设置与命令面板不再重复提供入口
- 各主题家族适配新的 L 形外框：顶栏与第一栏、第二栏之间以轻微色差分层，深浅模式均适配
- 更新应用图标与品牌 logo
- 修复：提交信息字号不一致；没有已暂存更改时打不开提交卡片
- 优化左上角控件位置

## [0.2.5]

- 更新下载改为择优取源：国内网络下 GitHub 的多个地址并非全部可达，下载前会实测发布页与两个公共中转，取最快且确实传来二进制字节的那个；卡片显示「正在选择下载源…」。下载后仍按 Release 公布的 sha256 校验，中转只能影响速度、无法影响内容
- 一键更新不再把安装包留在临时目录：装完由新启动的应用负责清理，不再每次更新累积约 12 MB

## [0.2.4]

- 通用设置新增「浏览器工具」开关，默认关闭：开启后才向模型注入内置浏览器工具与使用说明；切换即时生效（无需重启会话），关闭状态下工具与说明完全离开模型上下文，误触发调用也会被守护层拒绝
- 更新卡片新增「一键更新」：自动下载与当前架构匹配的安装包，按 Release 公布的 sha256 校验无误后静默安装，无需确认也不需要管理员权限，装完自动重启应用；解压即用的便携版没有安装记录，仍只提供前往 Release 下载的入口。GitHub 的多地址在部分网络下并非全部可达，下载前会实测发布页与两个公共中转并取最快者，校验始终以 Release 公布的 sha256 为准，中转只能影响速度、无法影响内容
- 内置浏览器工具扩展为 16 个：新增截图（返回模型可见的图像）、evaluate（受控执行页面 JavaScript，与其他变更操作同等授权）、按键、下拉选择、悬停、双击、拖拽、坐标点击与关闭页面；截图以有界 Media 通道送达，close 仅限会话自动打开的页面。evaluate 结果为 undefined 时按 null 返回而非误报原生错误，CDP 协议层拒绝改报可采取的受控错误；open 在分享发布经受住一个轮询周期后才向模型报成功，发布被拒不再谎报 pageId

## [0.2.3]

- 支持 OpenCode v2.0.19（此前仅保证到 v2.0.16）
- 新增 Git 历史面板：从会话胶囊即可进入，带本地/云端标签；超长提交信息不再溢出卡片、遮挡按钮
- 新增 12 套可切换主题家族：VS Code Modern、Codex、Nord、Linear、Notion、One、Proof、Raycast、Rose Pine、Solarized、Vercel、VS Code Plus，原有 Fintwind 主题保留
- 文件界面支持多开与回退
- 优化用量统计：子代理的 token 计入总量，模型和提供商排行按子代理实际使用的模型归属，费用按各自模型估算，并改用 models.dev 官方 API 价
- 优化模型界面的刷新与加载状态
- 侧边栏项目会话改为逐步显示
- 对话结尾补充角色、模型和耗时；信息缺失时省略，不再显示不可靠的 token 统计
- 修复浅色模式下 pwsh 行内预测提示不可见的问题
- 顶部会话标签统一为 190 px 宽

## [0.2.2]

- Open multiple sessions in tabs within one window, with more stable unread indicators
- Improve theme contrast and keyboard accessibility across the interface
- Refine token-speed display, file-drop attachment handling, and session activity presentation
- Remove the obsolete Todo section from session cards to match the OpenCode 2 protocol

## [0.2.1]

- Attach images and PDFs inline as data URLs so the model sees them directly instead of re-reading the files
- Follow OpenCode 2.0.11+ contract changes, keep session listing working on v2, and trace sessions back to their owning task
- Refresh the activity card UI, with the shimmer highlight now using the theme accent color on activity and thinking cards
- Merge content that trails a turn into its closing message
- Add a show/hide toggle to API key inputs

## [0.2.0]

- Send image and text attachments through OpenCode's files channel instead of splicing `@path` into the message, and drop files from the desktop onto the transcript or composer to attach them
- Show images that were already saved in a session's history
- Add a context-window panel with clearer usage stats
- When a message cannot be reverted, show a disabled button and the reason instead of hiding the control
- Add an in-app update card when a newer release is available
- On the Usage page, open a single day to see hourly charts and a usage breakdown
- Shimmer running tool names, and keep that treatment on subagent cards
- Rename native sessions through OpenCode's stable PATCH endpoint so the title syncs to the server
- Add catalog support for GPT-6, Claude Opus 5.5, and MiMo V2.6
- Say when a built-in provider check is reading the local model catalog, not testing the provider API
- Fix the info capsule turning transparent on hover, and even out sidebar shortcut spacing

## [0.1.8]

- Share one private OpenCode server across the app instead of a per-project pool, so sessions stay in sync with fewer conflicts
- Rebuild the provider page with login and logout for built-in providers
- Revert and restore a session through OpenCode's native revert and unrevert, keeping the same session
- Add a collapse control on the question card, and move where a new project is created
- Show more detail on subagent cards
- Smooth the Usage page, and count reasoning tokens in turn token speed the same way the OpenCode TUI does
- Remove the OpenCode Go plan from the context-usage popover
- Keep streaming text from being split apart when a tool call arrives
- Update StepFun model information
- Raise the frame rate of the session activity halo
- Remove the web client

## [0.1.7]

- Show raw OpenCode tool names (read, grep, execute, …) on activity cards, replacing generic icons, and render nested Code Mode tool calls from `toolCalls`
- Add settings to change the UI and code fonts
- Update the model thinking-level reference table for 2026 models

## [0.1.6]

- Add a Usage page that scans OpenCode session records for token and cost stats, with KPI cards, an activity heatmap, daily bars, and per-model ranking
- Cap the expanded height of the project sidebar, tool lists, and expanded cards so long lists stay inside the window
- Probe OpenCode 2.0.5's `/api/status` health endpoint first, falling back to `/api/health` for older CLIs
- Keep a session's transcript visible after the backend process exits, instead of dropping the render
- Fix remote MCP OAuth login so the authenticate flow completes instead of failing on the CLI callback chain

## [0.1.5]

- Typeset inline and display math natively instead of approximating it with Unicode characters, so formulas keep their real glyphs, sizes and theme color
- Accept the `\[…\]` and `\(…\)` delimiters models emit far more often than `$…$`, without CommonMark unescaping them into plain text
- Add Copy Expression to a formula's context menu
- Keep unfinished math in a streaming message from swallowing the markdown that follows it
- Typeset formulas on background workers with cached results, so the UI thread never blocks on the math engine

## [0.1.4]

- Show the app version in General settings, and an Update button next to Settings when a newer GitHub release is available
- Give sidebar session cards a provider-colored model avatar with a spinning halo while the session works, replacing the cramped three-line layout
- Simplify access modes to Ask, Auto-accept edits, and Full access; sessions saved with the old Auto mode open as Full access
- Fix subagent permission prompts and form replies being routed to the parent session, which left tools stuck as running until manually stopped

## [0.1.3]

- Add a font size setting
- Add the ability to remove a project
- Add an MCP marketplace and remote MCP OAuth login, with live connection status
- Fix the subagent lifecycle status in the UI
- Keep subagent user-message bubbles from shrinking as the panel widens

## [0.1.2]

- Show turn-footer token stats and provider retry cards
- Reconnect the daemon in place after a disconnect instead of failing permanently
- Keep late-arriving reasoning from splitting into an orphan thinking block
- Improve compaction UX and window chrome

## [0.1.1]

- Drive the `opencode` CLI instead of `opencode2` after OpenCode merged the two

## [0.1.0]

First Fintwind release, forked from [waku](https://github.com/egoist/waku) 0.1.7.
Versions from this point are Fintwind's own; they are not waku releases.

- Windows-only native desktop for OpenCode (Rust + GPUI)
- Drive a single OpenCode backend; remove other agent providers
- Group sessions by project with a card-style sidebar and per-group new session
- Model picker with provider filter, company brand icons, input modality, and
  thinking-effort variants
- Virtualize reasoning, show tool calls step by step, and render LaTeX in the
  transcript
- MCP management UI
- Close the window to quit and stop the OpenCode backend on exit
- Title-bar action to open the project in File Explorer
