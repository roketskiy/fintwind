# macOS、Linux 与 Computer Use 残留调查

> 调查日期：2026-09-09。Computer Use 已于同日按档 1 从仓库移除。
> 档 2（Linux 发行）与档 3（macOS 发行与 `cfg`）已于 2026-09-09 执行。

## 0. Computer Use（已移除）

已删除独立文件、协议命令、driver 接线、设置页、composer overlay、JS REPL MCP（`fintwind_js_repl`）、Swift helper、skill / pi-extension，以及相关 locale 与打包挂钩。

协议版本从 4 升到 5（去掉 `cancelComputerUse` / `runComputerTool` / `rejectComputerTool` / `probeComputerPermissions`、`DriverEvent::ComputerUseUpdated`、启动选项 `computer_use_enabled`）。旧 `settings.json` 里的 `computer_use_*` 键由 `DaemonSettings::discard_legacy_app_keys` 丢弃，不做迁移。

Windows 构建不再编译 `fintwind_js_repl`，也不再依赖 `rquickjs`。

## 1. 档 2 / 档 3（已执行）

Linux 发行文件、macOS 打包脚本与 icns/plist、Sparkle FFI、objc2/wry 依赖、macos/linux CI job 已删除。Sparkle 公钥迁到 `resources/sparkle-public-ed-key.txt`，由 `build.rs` 与 `scripts/appcast-windows.ts` 读取。`scripts/dev.ts` 只走 Windows `fintwind.exe`。文档改为 Windows-only。

`src/app/window_chrome.rs` 保留 Windows 自绘标题栏。Windows 更新器、`scripts/appcast-windows.ts`、`scripts/bundle-windows.ts` 与 WebView2 host 未动行为。
