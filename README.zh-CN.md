# fintwind

[English](README.md) | 简体中文

[OpenCode 2](https://opencode.ai/v2/docs) 的原生 Windows 桌面客户端。
使用 Rust 和 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 构建，
不是 Electron 套壳。在一个窗口内管理项目、打开多个会话标签页，并查看 agent 的回复、
思考过程和工具活动。

[官网](https://fintwind.xyz) · [下载](https://github.com/roketskiy/fintwind/releases/latest) · [更新日志](CHANGELOG.md)

会话演示：切换模型、进行中的会话，以及滚动一条很长的对话记录。

https://github.com/user-attachments/assets/beb2dcb5-88dd-4f2b-9415-153af9c93bf9

## 安装

### 1. 安装 OpenCode 2

fintwind 依赖本机的 `opencode` CLI，应用安装包不包含它。
在 PowerShell 中执行以下命令之一，**两种方式任选一种即可**。

使用 [Node.js 和 npm](https://nodejs.org/)：

```powershell
npm install -g @opencode/cli
```

或使用 [Bun](https://bun.sh/)：

```powershell
bun install -g --trust @opencode/cli
```

Bun 的 `--trust` 用于允许运行这个包必需的安装脚本。
这里使用的是 V2 的包名 `@opencode/cli`；其他安装方式见
[OpenCode 官方安装文档](https://opencode.ai/v2/docs/)。

重新打开一个 PowerShell 窗口，检查安装并启动 OpenCode：

```powershell
opencode --version
opencode
```

在 OpenCode 终端界面中通过 `/connect` 连接模型提供商，
或使用已有的提供商配置。fintwind 使用 OpenCode 的模型、提供商凭据和 MCP 配置。
**无需手动运行 `opencode serve`**：fintwind 会启动自己的本地私有服务。

可选的定时任务 MCP 需要 OpenCode 2.1.4 或更新版本。在「设置 → 通用 → 功能」中
开启后，Fintwind 自动管理实验性 MCP 连接。详见 [Agent 创建定时任务](docs/scheduled-tasks-mcp.md)。

可选的图像生成需要 OpenCode 2.2.1 或更新版本，以及已连接的兼容图像模型。
官方 Goal 适配依据 OpenCode 2.2.1；旧版本没有官方 Goal 时不能使用。
功能范围见 [Goal 说明](docs/goals.md)与 [图像模型配置](docs/image-generation.md)。

### 2. 安装 fintwind

- **系统：** Windows 10 1809 或更新版本，或 Windows 11；x86_64 或 Arm64。
- **图形驱动：** 支持功能级别 11_0 或更高的 Direct3D 11 驱动。
- **Git 功能：** 安装 [Git for Windows](https://git-scm.com/download/win)，并确保 `PATH` 中可用。

从[最新 release](https://github.com/roketskiy/fintwind/releases/latest) 下载
`fintwind-<version>-<arch>-Setup.exe` 并运行。安装程序按用户安装到
`%LOCALAPPDATA%\Programs\fintwind`，不需要管理员权限。

使用便携版时，解压 release 中的 `.zip`，运行 `fintwind.exe`。
**`fintwind.exe` 和 `fintwind-daemon.exe` 必须放在同一目录**，应用会从自身目录启动 daemon。

有新版本时，更新卡片可以一键升级：自动下载与当前架构匹配的安装包，按 Release
公布的 sha256 校验后静默安装，不需要管理员权限，装完自动重启应用。解压即用的
便携版没有安装记录，更新入口仍会打开对应的 release 页面。
SmartScreen、CLI 检测、数据目录和 WebView2 说明见
[Windows 安装与故障排查](docs/windows.md)。

## 功能

- **图像生成。** 在通用设置中启用 OpenCode 的实验性图像工具，并从当前工作区可用的图像模型中选择默认模型。
- **官方 Goal。** 通过 `/goal <目标>` 设置目标，支持查看、暂停、恢复和清除；目标评估与续跑由 OpenCode 管理，旧版 Fintwind 目标不会自动迁移。
- **会话 Todo。** 在会话胶囊中查看 Agent 的任务列表与完成进度，同时查看待处理的增删行数。
- **保持唤醒。** 可选在 Fintwind 运行期间保持屏幕和系统唤醒。
- **便携 OpenCode。** 在通用设置中选择 OpenCode 可执行文件，无需将便携版加入 PATH，支持恢复自动检测。
- **旧会话清理。** 按最后活动时间预览并清理超过 30、90 或 180 天的会话，默认 90 天；
  仅清理 Fintwind 中可见且关联 OpenCode 的会话，同时删除双方的对应记录。
- **上下文构成。** 面积分块图展示八类上下文的估算占比，将压缩摘要、工具调用和结果分别统计；
  实际占用与估算分开显示，不可测内容明确标记。
- **应用内更新。** 正式版按发布时间选择最新正式 Release，即使修正后的版本号更小也会提示更新。
  排除草稿与预发布，当前已是最新发布时不重复提示。
- **项目与会话标签页。** 按项目管理会话，在同一窗口打开多个会话，支持标签拖动排序和未读提示。
  未关联项目的新会话共用固定通用工作区，可在「设置 → 通用 → 数据管理」中更改文件夹；已有会话保留原目录。
- **会话控制。** 切换模型、模型支持的思考程度，以及访问模式（询问、自动接受编辑、完全访问）。
  agent 工作时可以排队或插话，通用设置可选择默认发送方式，排队的消息支持拖拽排序；
  提问卡片支持跳过、收起与展开。符合条件的 Git 回合可通过 OpenCode
  原生功能回退和恢复。`/btw` 基于会话中已定稿的上下文提一个一次性的旁问，不打断进行中的任务。
- **回合恢复。** 从 OpenCode 持久化历史补齐错过的回复与结束状态；历史响应支持最多 64 MiB，
  图片或工具结果导致单页过大时自动缩页读取。
- **提供商品牌图标。** 模型选择器与提供商设置使用可识别厂商的品牌图标，按需适配深浅主题。
- **清晰的 agent 活动。** 查看流式回复、思考、工具调用、子代理活动和嵌套 Code Mode 调用，
  保留 OpenCode 的工具名称。后台 shell 命令以胶囊形式挂在会话卡片上，完成的活动组会自动折叠。
  对话记录和思考过程使用虚拟化列表，限制每帧需要处理的内容。
- **文件与 Git。** 浏览工作区文件并打开多个文件标签页。审阅页按目录树分别展示已暂存、未暂存、未跟踪和已忽略文件，按需加载目录，并支持查看嵌套仓库和单文件差异。Git 提交历史以提交关系图呈现。
  可从输入栏添加附件，也可将文件拖到对话中；图片和 PDF 会内联发送给 OpenCode。
- **终端与浏览器。** 提供内置终端和可选的 WebView2 浏览器面板。在设置中开启浏览器工具
  （默认关闭）后，agent 可以操作你共享给会话的浏览器标签页——打开、观察、点击、填写、滚动、
  导航、截图——并受会话访问模式约束。会话界面本身仍由原生 GPUI 渲染。
- **外观设置。** 保留 Fintwind 主题，并提供另外 13 套主题家族；支持浅色、深色、跟随系统，
  以及界面和代码字体、字号设置。动效遵循系统的「减弱动态效果」偏好。
- **用量统计。** 根据本机 OpenCode 会话记录展示活动热力图、按日和按小时图表、模型与提供商排行，
  并计入子代理用量。每次打开默认显示单日，时间范围依次为单日、7 天、30 天与全部。显示的费用是估算值，不是账单。
- **匿名启动统计。** 正式版和 beta 每次启动向项目官网上报一个本地随机生成的安装 ID，
  用于统计每日活跃的桌面数量；不包含会话、项目路径或账号信息。
- **提供商管理。** 内置提供商可以登录和退出，一眼可见哪些连接需要重新登录；
  请求被拒时能看到提供商自己的拒绝原因（额度、密钥或模型）。自定义提供商的 API 密钥由 OpenCode 凭据托管，模型可继承提供商协议或单独指定协议，包括 OpenAI Images 和 Google Images。
- **定时任务。** 从侧边栏时钟图标进入，按指定星期与时间或固定间隔执行指令。
  可新建会话并选择项目、模型与权限，也可复用已有会话；支持暂停、恢复、立即运行和查看运行结果。
  Fintwind 完全退出后任务不会执行，错过的运行不会补跑。
  通用设置可开启「定时任务 MCP」，允许 Agent 创建任务，默认关闭，需要 OpenCode 2.1.4+；
  未明确的任务信息由 Agent 向用户询问。详见 [MCP 说明](docs/scheduled-tasks-mcp.md)。
- **MCP 管理。** 浏览搜索、办公、设计、数据、自动化、运维与商务等分类，
  添加远程服务器、完成 OAuth 登录并查看连接状态；有厂商官方端点的条目优先使用官方服务。
- **原生 Mermaid 图表。** 在对话中直接查看流程图、时序图、类图、状态图等图表，
  支持切换源码，编译失败时显示原因。可在通用设置中为新会话开启「Mermaid 可视化提示词」，
  让模型使用内置校验工具检查图表；已有会话保留创建时的偏好。
- **原生公式排版。** 支持行内和独立公式，包括 `\(…\)` 和 `\[…\]`，排版工作在后台执行。

## 本地数据与项目范围

fintwind 没有自己的账号，也不提供云端同步。应用管理的项目、任务和附件存储在本机的
SQLite 与文件中，OpenCode 原生会话数据由 OpenCode 管理。
**本地存储不等于离线推理：** 提示词及相关内容仍会发送给你在 OpenCode 中配置的模型提供商和工具。

本项目是 [EGOIST](https://github.com/egoist) 的 [waku](https://github.com/egoist/waku)
的独立 fork，采用 [GPL-3.0-only](LICENSE)，不是 OpenCode 官方桌面端。
fintwind 只发布 Windows 版本，只使用 OpenCode 2 作为 agent 后端。
你仍然可以通过 OpenCode 使用多个模型提供商；其他 agent 后端不在本 fork 的范围内。

### 内存快照

一次任务管理器截图中，桌面进程占用 69.4 MB，`fintwind-daemon.exe` 占用 25.7 MB，
两者合计约 95 MB。这不是性能基准，也不是完整 agent 运行环境的总内存：
不包含 OpenCode 和其他进程，实际占用会随工作负载变化。

![任务管理器：fintwind 69.4 MB，fintwind-daemon.exe 25.7 MB](docs/media/runtime-memory.png)

## 架构

桌面端通过 [`fintwind-client`](crates/fintwind-client) 与独立的
[`fintwind-daemon`](crates/fintwind-daemon) 进程通信，使用
[`fintwind-protocol`](crates/fintwind-protocol) 定义的带鉴权、带版本的 WebSocket 协议。
[`fintwind-core`](crates/fintwind-core) 实现 daemon 管理的存储、工作区与 Git 操作，
以及 OpenCode 集成。daemon 监听回环地址，并使用每次启动时生成的令牌鉴权。

Release 桌面端配置位于 `~/.fintwind/app.json`，Debug 使用 `temp/app.json`。
daemon 的提供商设置位于 `~/.fintwind/settings.json`。
未关联项目的新会话默认共用 `%LOCALAPPDATA%\Fintwind\projects`，可在通用设置中选择其他文件夹。
已有会话保留原目录，包括旧版位于 `~/.fintwind/projects/<日期>/<slug>` 的工作区。

## 开发

需要 Windows、MSVC C++ 工具链与 Windows SDK、
[Rust 1.97 或更新版本](https://www.rust-lang.org/tools/install)，以及 [Bun](https://bun.sh/)。
环境配置与检查要求见 [CONTRIBUTING.md](CONTRIBUTING.md)。

```sh
bun install
bun run dev
```

开发 watcher 会构建桌面端和独立的 `target/debug/fintwind-debug-daemon.exe`；
只修改 provider 相关代码时，可以替换 daemon 而不重启 Debug 桌面端。
Release 构建会把 `fintwind-daemon.exe` 放在 `fintwind.exe` 旁边。

打包和发布维护流程见 [RELEASING.md](RELEASING.md)。

## 许可证

[GNU General Public License v3.0 only](LICENSE)。
