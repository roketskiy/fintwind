# fintwind

简体中文 | [English](README.en.md)

fintwind 是 [OpenCode](https://opencode.ai) 的原生 Windows 桌面应用，使用 Rust 和
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 构建，
项目、会话与对话记录全部保存在你自己的机器上。

> fintwind 是 [EGOIST](https://github.com/egoist) 的
> [waku](https://github.com/egoist/waku) 的 fork，同样采用
> [GPL-3.0](LICENSE) 授权。与 waku 不同，本 fork 收敛为单一 OpenCode 后端：
> 移除了其余所有 agent provider 和 macOS/Linux 构建，专注 Windows 优先开发。
> 多 provider、跨平台的需求请前往上游项目。

## 安装

从[最新 release](https://github.com/roketskiy/fintwind/releases/latest) 下载
`fintwind-<version>-<arch>-Setup.exe` 运行即可。
同时提供便携版 `.zip`。系统要求与尚未支持的功能见
[docs/windows.md](docs/windows.md)。

## 环境要求

fintwind 只驱动一个 agent 后端：本地 [OpenCode](https://opencode.ai) 服务。
请先安装并登录 `opencode` CLI；fintwind 会自动拉起它，
会话、模型、provider 与 MCP 服务器全部来自这一个后端。

## 功能亮点

- **原生渲染，不是 Electron 套壳**：主界面由 Rust + GPUI 直接 GPU 渲染，
  仅右侧可选的浏览器面板使用系统内置的 WebView2。
- **长对话也流畅**：对话记录与思考过程做虚拟化渲染，每帧只构建可见内容；
  流式输出在高刷新率屏幕上依然顺滑。
- **遵循系统设置**：深/浅色主题跟随系统外观，动效尊重系统
  "减弱动态效果"设置，菜单、侧栏等关键交互支持键盘操作。
- **Windows 原生集成**：系统托盘、原生菜单、内置终端、按用户安装与
  签名自动更新，行为符合 Windows 惯例。
- **本地优先**：项目、会话、对话记录与附件全部保存在本机
  （SQLite + 本地文件），daemon 也在本机运行，无账号、无云端依赖。
- **默认零遥测**：遥测默认关闭，配置就是本地 JSON 文件，随时可备份迁移。
- **会话控制**：统一界面切换模型、思考程度与访问模式；agent 工作时可
  排队或插话后续消息；基于 Git 的任务可带对话感知检查点回退。

## 架构

原生桌面端是独立 `fintwind-daemon` 进程的 RPC 客户端。Provider 会话运行在
[`fintwind-core`](crates/fintwind-core) 中，隐藏在
[`fintwind-protocol`](crates/fintwind-protocol) 的带鉴权、带版本化的
WebSocket 契约之后。桌面端只依赖
[`fintwind-client`](crates/fintwind-client)，不依赖 daemon 的具体实现。
daemon 拥有任务 SQLite 数据、上传的附件、provider 原生的会话分叉，以及全部
工作区文件系统与 Git 操作；它返回的路径一律指 daemon 所在主机。
桌面端只保留展示状态和可丢弃的预览缓存。

daemon 监听仅回环地址，并使用每次启动生成的一次性令牌对客户端鉴权。

无项目的任务工作区位于 daemon 主机的 `~/.fintwind/projects/<日期>/<slug>`
下。daemon 首次加载时会把旧版 `~/.fintwind/<日期>/<slug>` 布局创建的
工作区迁移过去。

配置归属同样分离：Release 桌面端写 `~/.fintwind/app.json`，Debug 则隔离在
`temp/app.json`。daemon 的 provider 设置保存在
`~/.fintwind/settings.json`。

连接到桌面进程之外托管的 daemon 时，fintwind 绝不在客户机上解释 daemon
的路径。因此在协议增加 daemon 主机侧的目录选择器与终端流端点之前，
本地目录选择器和 PTY 不可用；文件、diff、Git、skills、用量、任务状态与
附件则已经走 daemon RPC。

Release 应用捆绑并签名 `fintwind-daemon`。开发时 daemon 位于
`target/debug/fintwind-debug-daemon`，这样只改 provider 相关代码时可以
单独重编译并替换 daemon，不必重启 fintwind Debug。

## 开发

开发环境要求 Windows 10 1809 或更新、MSVC 工具链、
[Rust 1.96 或更新](https://www.rust-lang.org/tools/install) 以及
[Bun](https://bun.sh/)。请先按
[CONTRIBUTING.md](CONTRIBUTING.md) 安装原生构建依赖。

```sh
bun install
bun run dev
```

右侧浏览器面板运行在 WebView2 上；agent 会话、项目、对话记录、skills、
用量、diff、文件编辑和终端均为原生实现。

开发流程与检查项见 [CONTRIBUTING.md](CONTRIBUTING.md)；
发布维护者还应阅读 [RELEASING.md](RELEASING.md)。

## 许可证

fintwind 基于 [GNU General Public License v3.0 only](LICENSE) 授权。
