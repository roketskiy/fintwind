# fintwind

[English](README.md) | 简体中文

[OpenCode 2](https://opencode.ai/v2/docs) 的原生 Windows 桌面客户端，使用 Rust 和 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 构建。在一个窗口内管理本机与 SSH 远端项目、使用多个会话标签页，并查看回复、思考过程与工具活动。

[下载](https://github.com/roketskiy/fintwind/releases/latest) · [更新日志](CHANGELOG.md)

会话演示：切换模型、进行中的会话，以及滚动一条很长的对话记录。

https://github.com/user-attachments/assets/beb2dcb5-88dd-4f2b-9415-153af9c93bf9

## 安装

### 1. 安装 OpenCode 2

fintwind 依赖本机的 `opencode` CLI（安装包不含该程序）。在 PowerShell 中通过 [Node.js](https://nodejs.org/) 或 [Bun](https://bun.sh/) 安装 `@opencode/cli`：

```powershell
npm install -g @opencode/cli
# 或
bun install -g --trust @opencode/cli
```

重新打开 PowerShell 窗口，运行 `opencode --version` 确认安装，并运行 `opencode` 通过 `/connect` 连接模型提供商（或沿用已有配置）。fintwind 会自行启动私有本地服务，无需手动运行 `opencode serve`。安装 fintwind 后，也可在「设置 → 通用 → 运行环境」安装 OpenCode 或通过内置 `upgrade` 命令更新。

本版支持到 OpenCode 2.4.1。可选功能的版本要求：

- **定时任务 MCP**：OpenCode 2.1.4+（使用实验性客户端托管 MCP 接口，在「设置 → 通用 → 功能」开启），详见 [Agent 创建定时任务](docs/scheduled-tasks-mcp.md)。
- **图像生成**：OpenCode 2.2.1+ 及已连接的图像模型。
- **官方 Goal**：OpenCode 2.2.1+（[Goal 说明](docs/goals.md)）。
- **后台压缩**：OpenCode 2.3.1+。

### 2. 安装 fintwind

- **系统要求**：Windows 10（1809+）或 Windows 11（x86_64 / Arm64），支持功能级别 11_0+ 的 Direct3D 11 驱动；Git 功能需在 `PATH` 中安装 [Git for Windows](https://git-scm.com/download/win)。
- **安装程序**：从[最新 release](https://github.com/roketskiy/fintwind/releases/latest) 下载 `fintwind-<version>-<arch>-Setup.exe`，按用户安装到 `%LOCALAPPDATA%\Programs\fintwind`，无需管理员权限，支持应用内校验并一键更新。
- **便携版**：解压 release `.zip` 后运行 `fintwind.exe`，`fintwind.exe` 与 `fintwind-daemon.exe` 须保留在同一目录。

官方安装包与便携包均内置 SSH 远端后端。SmartScreen、CLI 检测与数据目录见 [Windows 安装与故障排查](docs/windows.md)。

## 功能

- **项目与会话标签页**：按项目管理会话（现有文件夹、GitHub 克隆、新建文件夹）或使用可配置的通用工作区，支持标签排序、未读提示与旧会话清理。
- **会话控制**：切换模型、思考程度与访问模式（询问、自动接受编辑、完全访问）；支持工作时排队或插话、`/` 工作区命令与 `@` 文件技能补全、`/btw` 临时旁问（[说明](docs/btw.md)）、Git 回合回退恢复，以及转发子智能体提问和审批。
- **提示词脱敏**：默认开启，在发给模型前将检测到的凭据与邮箱替换为可跨轮复用的引用，并在输入框与历史中标出，展示及工具执行时按规则还原。规则改编自 [Gitleaks](docs/licenses/gitleaks.txt) 与 [CredSweeper](docs/licenses/credsweeper.txt)。
- **对话渲染与预览**：虚拟化列表展示流式回复、思考、工具调用、子代理与后台任务；原生渲染 [Mermaid 图表](docs/diagrams.md)（[Ondine](docs/licenses/ondine-MIT.txt)）、LaTeX 公式、本地与内联图片及可复制链接，并提供统一的全屏预览器（缩放、旋转、复制与导出 PNG）。
- **文件、Git 与终端**：多标签浏览文件，审阅目录树变更（含嵌套仓库与单提交更改），查看多分支 [Git 历史](docs/git-history.md)泳道图，执行提交、拉取与推送，支持文件/图片/PDF 附件与内置终端。
- **SSH 远端工作区**：在同一窗口内并行使用本机与 Linux 服务器（x86_64/ARM64）项目，远端后端自动部署升级。
- **手机远程控制**：在手机网页通过扫码或配对链接建立端到端加密的 WebRTC 直连，切换工作区、管理会话、使用 `@` 提及与 `/btw` 旁问、查看后台任务与压缩状态、处理审批和问答。详见 [手机远控说明](docs/remote-control.md)。
- **用量与订阅**：整合的用量仪表盘提供可切换趋势图的指标卡片、周期对比、按模型/提供商/项目/全量构成分布、周矩阵与热力图，以及已连接供应商（含本机 Claude Code、Codex）的订阅余额查看。详见 [供应商额度说明](docs/provider-quota.md)。
- **模型、提供商与 MCP**：内置精简离线 models.dev 模型表、供应商 OAuth 登录与拒信诊断、自定义凭据与图像模型配置，以及分类 MCP 服务与登录管理。
- **Goal、Todo 与定时任务**：集成官方 [Goal](docs/goals.md)、会话 Todo 与变更行数统计、按星期/间隔/单次运行的定时任务（含时区记录），以及可选的 [定时任务 MCP](docs/scheduled-tasks-mcp.md)（OpenCode 2.1.4+）。
- **稳定性与上下文**：支持 OpenCode 自动与后台压缩、[上下文构成](docs/context-composition.md)分块图、重复输出保护、回合历史恢复、保持唤醒与校验更新。
- **外观与统计**：提供 14 套主题家族、浅色/深色/定时自动模式、界面与代码字体定制，以及仅发送本地随机安装 ID 的匿名启动计数。

## SSH 远端连接

准备一台可 SSH 访问的 Linux 服务器（x86_64 或 ARM64）及**无密码短语**的私钥文件（服务器克隆项目需 Git；OpenCode 可在连接后安装）。

1. 在「设置 → SSH 连接 → 新建连接」填写服务器信息并选择私钥，点击「保存并连接」。
2. 点击「添加项目」选择服务器，按现有文件夹、GitHub 克隆或新建文件夹添加项目。
3. 通过侧边栏底部切换「本机」与各远端主机。

连接过程会校验主机指纹并通过 SSH 隧道加密传输（无需开放额外端口），支持在设置侧栏切换目标主机以管理远端 OpenCode、提供商、MCP 与技能；移除连接仅删除本机连接信息。

## 本地数据与项目范围

fintwind 不设账号也不提供云端同步。应用状态保存在本机 SQLite 与文件中，原生会话数据由本机或远端服务器上的 OpenCode 管理；提示词仍会发往你在 OpenCode 中配置的模型提供商（默认开启提示词脱敏）。

本项目是 [EGOIST](https://github.com/egoist) 的 [waku](https://github.com/egoist/waku) 的独立 [GPL-3.0-only](LICENSE) fork，非 OpenCode 官方桌面端，仅面向 Windows 与 OpenCode 2。

![任务管理器：fintwind 69.4 MB，fintwind-daemon.exe 25.7 MB](docs/media/runtime-memory.png)

*任务管理器示例快照（桌面端 69.4 MB、daemon 25.7 MB，不含 OpenCode 与模型进程，不代表性能基准）。*

## 架构与开发

桌面端通过 [`fintwind-client`](crates/fintwind-client) 与 [`fintwind-daemon`](crates/fintwind-daemon) 使用 [`fintwind-protocol`](crates/fintwind-protocol) 通信，[`fintwind-core`](crates/fintwind-core) 负责存储、Git 与 OpenCode 集成；SSH 远端通过隧道复用同一协议。

从源码构建需要 Windows、MSVC C++ 工具链与 Windows SDK、[Rust 1.97+](https://www.rust-lang.org/tools/install) 及 [Bun](https://bun.sh/)，规范与打包流程见 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [RELEASING.md](RELEASING.md)。

```sh
bun install
bun run dev
```

## 许可证

[GNU General Public License v3.0 only](LICENSE)。
