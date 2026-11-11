# Contributing to Fintwind

Thanks for helping improve Fintwind. Bug reports, focused fixes, tests, and
well-scoped features are welcome.

## Development setup

The debug app requires:

- Windows 10 1809 and newer
- Rust 1.97 or newer
- Bun
- A supported agent CLI when testing a provider integration
- The MSVC toolchain (Visual Studio Build Tools with the C++ workload and the
  Windows SDK) so Cargo can link and so the resource compiler is available for
  the executable's icon and version block

Install dependencies and start the development watcher from the repository
root:

```sh
bun install
bun run dev
```

The watcher builds `target/debug/fintwind.exe`. The provider daemon remains an
external `target/debug/fintwind-debug-daemon.exe`: provider-only edits rebuild and
hot-swap that process without relaunching the app, while desktop edits rebuild
and relaunch the app normally. Keep that watcher running while you work. Do
not start a second watcher or manually relaunch the debug app. Press `Ctrl-C`,
or quit the app, to stop it.

The right-panel browser runs on WebView2.

## MyGo 日用与开发入口

`fintwind-go/` 是连接 Rust daemon 的 MyGo 前端；`mygo-clone/` 仍是独立界面原型。
本地启动需要 PowerShell 7、Go（版本见 `fintwind-go/go.mod`）和上述 Rust/MSVC 工具链。
首次检出后先初始化固定版本的组件库子模块：

```sh
git submodule update --init --recursive -- vendor/CrychicUI
```

前端使用 MyGo（MIT）、Lobe MyGo（MIT）和 CrychicUI（MIT）。MyGo 和 Lobe MyGo
通过 Go 模块固定版本；CrychicUI 通过子模块固定提交，不跟随上游自动更新。
许可及图标来源见 [`docs/licenses/mygo-MIT.txt`](docs/licenses/mygo-MIT.txt)、
[`docs/licenses/lobe-mygo-MIT.txt`](docs/licenses/lobe-mygo-MIT.txt)、
[`docs/licenses/CrychicUI-MIT.txt`](docs/licenses/CrychicUI-MIT.txt) 和
[`docs/licenses/mygo-dependencies.txt`](docs/licenses/mygo-dependencies.txt)。

正式界面提供会话、源码管理、用量和设置；不提供定时任务管理或手机远程连接页面。
daemon 的定时任务能力保留，已有定时任务产生的会话事件仍会正常进入对话记录。
日常操作与隔离开发统一使用下面的入口，不使用复制真实用户数据的旧探针脚本。

在仓库根目录使用：

```powershell
.\MyGo.cmd
.\MyGo-Agent.cmd
.\MyGo-Agent.cmd --name task-layout
```

- `MyGo.cmd` 是日用入口，直接读写现有 `%LOCALAPPDATA%/Fintwind/app.db`，
  daemon 设置继续使用 `~/.fintwind/settings.json`，不是数据库副本。
  请先退出旧版 Fintwind。数据被另一实例占用时启动失败，不会关闭该实例。
- 日用启动直接打开原数据库，不自动备份或复制数据。
  不保证旧版能读取新版迁移后的数据库。
- MyGo 的界面偏好单独存于 `%LOCALAPPDATA%/Fintwind Go/app-settings.json`，
  不覆盖 Rust 界面偏好。
- `MyGo-Agent.cmd` 默认使用独立环境，数据、HOME、OpenCode 配置与数据库、
  缓存和工作目录都在 `%LOCALAPPDATA%/Fintwind Go/Agent/<name>`。
  不继承 provider 密钥、OpenCode 授权内容或旧 daemon 连接信息，不复制日用凭据。
  `--name` 可为每项开发任务分配单独环境。
- 独立环境不是文件系统或网络沙箱。手动添加真实项目、导入凭据或授权后，
  相关操作仍可能修改真实文件或产生费用。自动验证只使用专用测试项目。

两个入口均先编译 Go 前端，再按 Rust 源码、资源、工具链和构建配置指纹复用 daemon。
构建失败不会启动旧产物。二进制按环境和运行轮次分开保存，启动器核对协议后才打开窗口。
关闭窗口会关闭本轮启动器及其拥有的 daemon；关闭启动器控制台也会清理本轮进程。
目前不提供跨界面重启保留 daemon 的模式。

仅查看路径或仅构建而不打开应用：

```powershell
.\MyGo.cmd --plan
.\MyGo-Agent.cmd --plan --name task-layout
.\MyGo-Agent.cmd --build-only --name task-layout
```

`--plan` 不打开或修改应用数据，但入口仍会编译启动器。
默认不启动 watcher，不改变 stable 的安装、更新或发布流程。

无窗口启动器 E2E 使用新建的隔离目录、真实 daemon 和受控子进程，不调用 provider：

```powershell
.\MyGo-Agent.cmd --verify --daemon E:/absolute/path/to/fintwind-daemon.exe
```

检查结果、二进制哈希和日志保留在命令打印的 `result.json` 所在目录。

## Windows bundle

To produce the portable archive and the installer:

```sh
bun scripts/bundle-windows.ts
```

Both land under `target/release`. The zip holds the two executables side by
side beneath one versioned directory — the layout Fintwind needs to find its
daemon — and the installer is built from
[`resources/windows/fintwind.iss`](resources/windows/fintwind.iss), so Inno Setup 6.3
or newer must be installed (`choco install innosetup`) — the architecture
gate uses identifiers added in 6.3. Set `WINDOWS_CERTIFICATE`
(base64 `.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD` to Authenticode-sign them;
without those the script packages unsigned binaries and says so.
[docs/windows.md](docs/windows.md) documents installing for users, and
[RELEASING.md](RELEASING.md) the GitHub release process.

The default package is the stable channel. The personal beta channel uses the
same production application identity and data directory, but skips automatic
update checks:

```sh
bun scripts/bundle-windows.ts --channel=beta
```

Debug builds are the `fintwind Dev` channel for agent development and use a
separate data directory. Beta and stable must remain data-compatible because
they intentionally share the production directory — which also means beta and
stable must not run at the same time, and data a newer beta migrated is not
guaranteed to reopen in an older stable. Both channels build into the same
`target/release`, so building one after the other replaces those executables
in place; each bundle run packages right after its own build. Builds outside
the stable channel skip the in-app update check, so a beta is never replaced
by a published stable behind your back; GitHub CI builds the stable channel
only (`--channel=stable` is explicit in the Release workflow).

## Making changes

- Before starting work on anything larger than a bug fix, open an issue and
  discuss the proposal first.
- Keep changes focused and follow the existing Rust and GPUI conventions.
- Keep filesystem, process, network, and other blocking work off the UI thread.
  Rendering and row-building paths must read data already held in memory.
- Keep long collections virtualized and per-frame work proportional to visible
  content.
- Make every mouse control keyboard-operable, preserve visible focus, honor
  reduce-motion settings, and do not communicate state with color alone.
- Prefer provider-neutral behavior when a change applies to every agent, while
  preserving provider-native event order and session semantics.
- Add or update tests for behavior that can be verified without the UI.

## Checks

Run the focused checks relevant to your change, then run the full baseline
before opening a pull request:

```sh
cargo fmt --package fintwind --package fintwind-protocol --package fintwind-client --package fintwind-core --package fintwind-daemon -- --check
cargo check
cargo test
```

GitHub runs Tests on pushes to `main` and on pull requests, not on every
feature-branch push. Use **Actions → Tests → Run workflow** to check a branch
without opening a PR. The CI Rust version is maintained in
`.github/rust-version`.

For workflow or release-automation changes, also run the isolated black-box
check (Python 3 and Bun; no application, provider credentials, or GitHub access):

```sh
python scripts/ci-workflows-blackbox.py --output temp/workflow-tools/result.json
```

For user-visible changes, wait for the watcher to report a successful rebuild
and validate the freshly relaunched app. Include screenshots or a short
recording in the pull request when they make the result easier to review.

## Pull requests

In the pull request description:

- Explain the problem and the chosen solution.
- List the checks you ran.
- Call out known limitations or follow-up work.
- Link the related issue, if one exists.

By contributing, you agree that your contribution will be licensed under the
[GNU General Public License v3.0 only](LICENSE).
