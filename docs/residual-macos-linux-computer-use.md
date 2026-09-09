# macOS、Linux 与 Computer Use 残留调查

> 调查日期：2026-09-09。Computer Use 已于同日按档 1 从仓库移除。本文后半仍记录尚未动手的 macOS / Linux 发行残留。

## 0. Computer Use（已移除）

已删除独立文件、协议命令、driver 接线、设置页、composer overlay、JS REPL MCP（`fintwind_js_repl`）、Swift helper、skill / pi-extension，以及相关 locale 与打包挂钩。

协议版本从 4 升到 5（去掉 `cancelComputerUse` / `runComputerTool` / `rejectComputerTool` / `probeComputerPermissions`、`DriverEvent::ComputerUseUpdated`、启动选项 `computer_use_enabled`）。旧 `settings.json` 里的 `computer_use_*` 键由 `DaemonSettings::discard_legacy_app_keys` 丢弃，不做迁移。

Windows 构建不再编译 `fintwind_js_repl`，也不再依赖 `rquickjs`。

## 1. 背景

`Cargo.toml` 的 package description 已写成：

> A native Windows desktop app for OpenCode 2

桌面端实际运行在 Windows 上：WebView2、自绘标题栏、Inno Setup 安装包、Windows 更新 feed。但源码、资源、脚本、CI 和文档仍大量保留上游多平台形态：

- macOS 发行链路（`.app`、Sparkle、DMG、公证）仍完整。
- Linux 发行链路（`tar.gz`、`install.sh`、`.desktop`、Wayland/X11 CI）仍完整。

## 2. macOS 支持

### 2.1 独立文件（发行与资源）

| 路径 | 作用 |
| --- | --- |
| `scripts/bundle.sh` | 打 `.app`、codesign、嵌入 Sparkle.framework |
| `scripts/release.ts` | DMG、公证、staple、Sparkle zip / delta |
| `scripts/appcast.ts` | 生成 macOS `appcast.xml` |
| `resources/Info.plist` | macOS bundle 元数据、`SUPublicEDKey` |
| `resources/AppIcon.icns` | Release 图标 |
| `resources/AppIconDev.icns` | Debug 图标 |
| `scripts/delete-debug-app.ts` | 清 `~/Library`、`Fintwind Debug.app`（几乎全是 macOS 路径） |

### 2.2 CI

- `.github/workflows/release.yml`：`macos` job（`create-dmg`、Developer ID、公证）
- `.github/workflows/test.yml`：矩阵含 `macos-latest`
- `.github/workflows/sync-release.yml`：仍同步 `appcast.xml`

### 2.3 `#[cfg(target_os = "macos")]` 实现（Windows 不编译）

| 位置 | 内容 |
| --- | --- |
| `src/updater.rs` | 整块 Sparkle FFI（`mod macos`，动态加载 `Sparkle.framework`） |
| `src/platform.rs` | `NSApplication` About、CoreText 字体注册、NSWorkspace、用户通知、reduce-motion |
| `src/browser.rs` | wry / WKWebView host、`focus_parent`、快照 |
| `src/input.rs` | macOS 快捷键 |
| `crates/fintwind-core/src/command_env.rs` | `SIGCHLD` / `pthread_sigmask`，避免 ACP 子进程变成僵尸 |
| `crates/fintwind-protocol/src/i18n.rs` | `NSLocale::preferredLanguages` |
| `Cargo.toml` | `objc2*`、`wry`、`block2`、`objc2-web-kit`、`objc2-user-notifications` |
| `crates/fintwind-core/Cargo.toml` | macOS `objc2-foundation` |
| `crates/fintwind-protocol/Cargo.toml` | 同上，给系统 locale 用 |

### 2.4 开发脚本

`scripts/dev.ts` 仍按 `process.platform === "darwin"` 分支：

- 产物路径：`target/debug/Fintwind Debug.app`
- 构建：调用 `scripts/bundle.sh debug`
- 启动：`open -n -W`
- 停止：`pkill -TERM -x "Fintwind Debug"`

当前 Windows 开发走 `cargo build` + 直接启动 `fintwind.exe`。`AGENTS.md` 里「持有当前的 `Fintwind Debug.app` 进程」也是这条 macOS 路径的残留表述。

### 2.5 共享文件里的 macOS 窗口选项

`src/lib.rs` 打开主窗口时仍有：

- `traffic_light_position`（红绿灯位置）
- `app_owns_titlebar_drag`
- `WindowBackgroundAppearance::Blurred`
- 注释里的 NSWindow / Dock 行为

这些在 Windows 上被 `cfg!` 折成另一支，属于死分支而不是独立模块。

## 3. Linux 支持

### 3.1 独立文件

| 路径 | 作用 |
| --- | --- |
| `scripts/bundle-linux.sh` | 打 `fintwind-<ver>-<target>.tar.gz` |
| `scripts/install.sh` | `curl \| sh` 安装到 `~/.local`，注册 desktop entry |
| `docs/linux.md` | 依赖、手动安装、卸载、从源码构建、VM 软件光栅 |
| `resources/linux/sh.fintwind.desktop` | `.desktop` |
| `resources/linux/app-icon.png` | 256×256 图标 |

### 3.2 CI

- `.github/workflows/release.yml`：`linux-x86_64`、`linux-arm64`（Ubuntu 22.04，glibc 2.35；安装 Wayland / X11 / Vulkan 开发包）
- `.github/workflows/test.yml`：`ubuntu-24.04`
- `.github/workflows/sync-release.yml`：同步 `latest-linux.txt`

### 3.3 代码分支

| 位置 | 内容 |
| --- | --- |
| `src/platform.rs` | `linux_app_icon()`、`gsettings` 读 `enable-animations` |
| `src/app/window_chrome.rs` | Wayland 客户端装饰（**该文件 Windows 也在用**，见第 4 节） |
| `src/browser.rs` | Linux 无嵌入 WebView 的空 `WebviewHost`（wry 的 WebKitGTK 与 GPUI Linux 后端不兼容） |
| `src/updater.rs` | Linux stub：`Updater::init() -> None`，升级靠重跑 `install.sh` |
| `src/lib.rs` | `#[cfg(target_os = "linux")] icon: linux_app_icon()` |
| `src/terminal.rs` | Linux 终端剪贴板修饰键约定 |
| `crates/fintwind-core/src/command_env.rs` | `/bin/bash`、`/bin/sh` 候选与测试 |
| `crates/fintwind-client/src/command_env.rs` | unix shell 候选里的 linux 分支 |
| `Cargo.toml` | `[target.'cfg(target_os = "linux")'.dependencies] image`（给窗口图标解码） |

## 4. 不要误删

这些不是纯残留，Windows 运行时仍依赖：

| 路径 | 原因 |
| --- | --- |
| `src/app/window_chrome.rs` | `#[cfg(any(target_os = "linux", target_os = "windows"))]` 的自绘标题栏按钮 |
| `src/updater.rs` 的 Windows 实现 | 读 appcast、验 EdDSA、拉起 Inno Setup |
| `scripts/appcast-windows.ts` | 生成 `appcast-windows-*.xml` |
| `scripts/bundle-windows.ts`、`resources/windows/` | Windows 安装包 |
| `src/browser.rs` 的 WebView2 host | 右侧 Browser 面板 |
| 部分 `cfg!(not(target_os = "macos"))` | 关最后一扇窗即退出、终端 Ctrl+Shift+C/V 等，Windows 上是有效路径 |
| `build.rs` 从 `Info.plist` 导出 `SUPublicEDKey` | Windows 更新器用同一把 Sparkle 公钥验签；删 macOS 打包前要先把密钥迁走 |

`src/platform.rs` 里 `primary_shortcut(macos, other)` 这类跨平台 helper 可以收缩成 Windows-only，但不是整文件可删。

## 5. 文档与产品描述不一致

以下文档仍按「macOS + Linux + Windows」三平台产品来写：

| 文件 | 残留表述 |
| --- | --- |
| `README.md` | macOS `.dmg`、Linux `install.sh`、三平台开发 |
| `CONTRIBUTING.md` | macOS / Linux (Wayland or X11) / Windows；`./scripts/bundle-linux.sh` |
| `RELEASING.md` | 主体是 Sparkle、DMG、公证；Windows 只是附录 |
| `CHANGELOG.md` | 「Add Linux support (X11 and Wayland…)」 |
| `AGENTS.md` | 假定 `Fintwind Debug.app` 已被 dev watcher 持有 |

这与 `Cargo.toml` / README 第一句的 Windows-only 定位已经矛盾。

## 6. 建议的后续清理分档

档 1（Computer Use）已执行。若继续：

### 档 2：再砍 Linux 发行

删第 3.1 节文件，去掉 release/test/sync workflow 的 Linux job 与 `latest-linux.txt`。`window_chrome.rs` 只删 Linux `cfg`，保留 Windows 标题栏。

### 档 3：再砍 macOS 发行与 `cfg`

删第 2.1 节打包脚本和 icns/plist（**先**把 Sparkle 公钥从 `Info.plist` 迁到 Windows 构建能读的位置），去掉 Sparkle FFI、objc2/wry 依赖、macos CI job，并把文档改成 Windows-only。`scripts/dev.ts` 去掉 darwin 分支。

档 3 动到的共享文件最多（`platform.rs`、`browser.rs`、`updater.rs`、`lib.rs`、`command_env.rs`），需要单独审查 Windows 路径仍然编译并行为不变。
