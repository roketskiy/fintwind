# fintwind

[English](README.md) | 简体中文

[OpenCode 2](https://opencode.ai/v2/docs) 的原生 Windows 桌面客户端。
使用 Go 和 [MyGo](https://github.com/egoist/mygo) 构建，
不是 Electron 套壳。在一个窗口内管理项目、打开多个会话标签页，并查看 agent 的回复、
思考过程和工具活动。

[下载](https://github.com/roketskiy/fintwind/releases/latest) · [更新日志](CHANGELOG.md)

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
安装 fintwind 后，也可在「设置 → 通用 → 运行环境」中通过 npm 或 Bun 安装、更新
OpenCode。应用内更新仅适用于当前 npm 或 Bun 管理的安装；便携版等其他安装方式需自行更新。

重新打开一个 PowerShell 窗口，检查安装并启动 OpenCode：

```powershell
opencode --version
opencode
```

在 OpenCode 终端界面中通过 `/connect` 连接模型提供商，
或使用已有的提供商配置。fintwind 使用 OpenCode 的模型、提供商凭据和 MCP 配置。
**无需手动运行 `opencode serve`**：fintwind 会启动自己的本地私有服务。

### 2. 安装 fintwind

- **系统：** Windows 10 1809 或更新版本，或 Windows 11；x86_64 或 Arm64。
- **图形驱动：** 支持功能级别 11_0 或更高的 Direct3D 11 驱动。
- **Git 功能：** 安装 [Git for Windows](https://git-scm.com/download/win)，并确保 `PATH` 中可用。

从[最新 release](https://github.com/roketskiy/fintwind/releases/latest) 下载
`fintwind-<version>-<arch>-Setup.exe` 并运行。安装程序按用户安装到
`%LOCALAPPDATA%\Programs\fintwind`，不需要管理员权限。

使用便携版时，解压 release 中的 `.zip`，运行 `fintwind.exe`。
**`fintwind.exe` 和 `fintwind-daemon.exe` 必须放在同一目录**，应用会从自身目录启动 daemon。

更新时从 release 页面下载新的安装包或便携版。
SmartScreen、CLI 检测和数据目录说明见 [Windows 安装与故障排查](docs/windows.md)。

## 功能

Go 版支持项目与会话标签页、提供商设置、MCP 服务器设置、源码管理和内置终端，
通过本机 OpenCode 进行 agent 交互。原 Rust 版的部分功能尚未在 Go 版提供。

网页链接和登录授权由系统浏览器打开；Fintwind 不内嵌浏览器，也不提供浏览器自动化工具。

## 本地数据与项目范围

fintwind 没有自己的账号，也不提供云端同步。应用管理的项目、任务和附件存储在本机的
SQLite 与文件中，OpenCode 原生会话数据由 OpenCode 管理。
**本地存储不等于离线推理：** 提示词及相关内容仍会发送给你在 OpenCode 中配置的模型提供商和工具。

本项目是 [EGOIST](https://github.com/egoist) 的 [waku](https://github.com/egoist/waku)
的独立 fork，采用 [GPL-3.0-only](LICENSE)，不是 OpenCode 官方桌面端。
fintwind 只发布 Windows 版本，只使用 OpenCode 2 作为 agent 后端。
你仍然可以通过 OpenCode 使用多个模型提供商；其他 agent 后端不在本 fork 的范围内。

## 架构

Go 桌面端和 daemon 的入口位于 [`cmd/`](cmd)，业务实现位于 [`internal/`](internal)。桌面端从自身目录启动
`fintwind-daemon.exe`。daemon 监听本机回环地址，并使用每次启动生成的令牌鉴权。
应用数据位于 `%LOCALAPPDATA%\Fintwind`，daemon 的提供商设置位于
`%USERPROFILE%\.fintwind\settings.json`。

原 Rust 实现的源码归档于 [`legacy/rust/`](legacy/rust/README.md)，不再构建和发布。
维护与验证工具统一放在 [`tools/`](tools/README.md)。

## 开发

需要 Windows、PowerShell 7 和 [`go.mod`](go.mod) 指定的 Go 版本。
在仓库根目录运行 `MyGo.cmd`，构建并使用已有 Fintwind 数据启动应用。
开发与构建说明见 [CONTRIBUTING.md](CONTRIBUTING.md)，打包流程见 [RELEASING.md](RELEASING.md)。

## 许可证

[GNU General Public License v3.0 only](LICENSE)。
