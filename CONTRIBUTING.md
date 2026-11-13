# Contributing to Fintwind

Thanks for helping improve Fintwind. Bug reports, focused fixes, tests, and
well-scoped features are welcome.

## Development setup

开发正式版需要 Windows、PowerShell 7 和 `fintwind-go/go.mod` 指定的 Go 版本。
打包安装程序还需要 Bun 和 Inno Setup。Go 版位于 `fintwind-go/`；原 Rust
源码保留供参考，不参与正式构建和发布。

## 源码启动

`fintwind-go/` 包含 MyGo 前端和 Go daemon。
本地启动需要 PowerShell 7 和 Go（版本见 `fintwind-go/go.mod`）。
前端使用 MyGo（MIT）、Lobe MyGo（MIT）和 CrychicUI（MIT），均通过 Go 模块
固定版本，由 Go 自动下载并按 `go.sum` 校验，不需要初始化子模块或准备本地组件库。
许可及图标来源见 [`docs/licenses/mygo-MIT.txt`](docs/licenses/mygo-MIT.txt)、
[`docs/licenses/lobe-mygo-MIT.txt`](docs/licenses/lobe-mygo-MIT.txt)、
[`docs/licenses/CrychicUI-MIT.txt`](docs/licenses/CrychicUI-MIT.txt) 和
[`docs/licenses/mygo-dependencies.txt`](docs/licenses/mygo-dependencies.txt)。

正式界面提供会话、源码管理、用量和设置。部分原 Rust 版功能尚未在 Go 版提供。
从源码启动应用或使用独立开发环境时，使用下面的入口。

在仓库根目录使用：

```powershell
.\MyGo.cmd
.\MyGo-Agent.cmd
.\MyGo-Agent.cmd --name task-layout
```

- `MyGo.cmd` 使用正式数据，直接读写现有 `%LOCALAPPDATA%/Fintwind/app.db`，
  daemon 设置继续使用 `~/.fintwind/settings.json`，不是数据库副本。
  请先退出旧版 Fintwind。数据被另一实例占用时启动失败，不会关闭该实例。
- `MyGo.cmd` 直接打开原数据库，不自动备份或复制数据。
  不保证旧版能读取新版迁移后的数据库。
- MyGo 的界面偏好单独存于 `%LOCALAPPDATA%/Fintwind Go/app-settings.json`，
  不覆盖 Rust 界面偏好。
- `MyGo-Agent.cmd` 默认使用独立环境，数据、HOME、OpenCode 配置与数据库、
  缓存和工作目录都在 `%LOCALAPPDATA%/Fintwind Go/Agent/<name>`。
  不继承 provider 密钥、OpenCode 授权内容或旧 daemon 连接信息，不复制正式数据的凭据。
  `--name` 可为每项开发任务分配单独环境。
- 独立环境不是文件系统或网络沙箱。手动添加真实项目、导入凭据或授权后，
  相关操作仍可能修改真实文件或产生费用。自动验证只使用专用测试项目。

两个入口均构建 Go 前端和 Go daemon。
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
默认不启动 watcher。

`MyGo.cmd` 使用现有 Fintwind 数据、用户配置和项目。以下命令分别查看开发环境路径
和构建 Go 后端，都不会打开应用窗口：

```powershell
.\MyGo-Agent.cmd --backend go --plan --name go-backend
.\MyGo-Agent.cmd --backend go --build-only --name go-backend
```

Go 后端使用 modernc.org/sqlite（BSD-3-Clause）访问 SQLite，许可见
[`docs/licenses/modernc-sqlite.txt`](docs/licenses/modernc-sqlite.txt)。MyGo 前端使用
limu（MIT，包含 Pele 的 MIT 许可）渲染 Mermaid，许可见
[`docs/licenses/limu.txt`](docs/licenses/limu.txt)。

独立开发环境的 Git 操作只作用于其工作区。本地远端也必须在该根内；网络传输使用独立
Git 配置，不继承用户的 credential helper、SSH agent 或私钥。SSH 主机信任只读取
隔离 profile 的 `home/.ssh/known_hosts`，没有可信记录时保持失败，不自动接受主机。
当前不支持多 URL、自定义 fetch refspec 或需要外部 filter 的快进检出；请求会明确拒绝。

独立开发环境的快进检出后若引用或索引发布失败，错误会给出 Git metadata 下的
`fintwind-recovery/<id>` 路径，其中保留前后索引、提交身份和更新阶段。
不要直接重试或覆盖现有恢复记录，应先检查工作树、索引和分支引用。恢复记录用于
操作或进程失败后的人工核对，不保证断电耐久性；保留记录达到上限时会拒绝新的检出。

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

The default package is stable. To build a local beta package:

```sh
bun scripts/bundle-windows.ts --channel=beta
```

Stable and beta use `%LOCALAPPDATA%\Fintwind` and must not run at the same
time. A database migrated by a newer beta may not reopen in an older stable.
Both builds write the executables to `target/release`.

To build the executables without packaging, run these commands in
`fintwind-go/`:

```powershell
New-Item -ItemType Directory -Force ../target/release | Out-Null
go build -ldflags="-H=windowsgui" -o ../target/release/fintwind.exe ./cmd/app
go build -o ../target/release/fintwind-daemon.exe ./cmd/daemon
```

Keep both executables in the same directory. `fintwind.exe` starts the daemon
beside it and uses the production data directory.

## Making changes

- Before starting work on anything larger than a bug fix, open an issue and
  discuss the proposal first.
- Keep changes focused and follow the existing Go and MyGo conventions.
- Keep filesystem, process, network, and other blocking work off the UI thread.
  Rendering and row-building paths must read data already held in memory.
- Keep long collections virtualized and per-frame work proportional to visible
  content.
- Make every mouse control keyboard-operable, preserve visible focus, honor
  reduce-motion settings, and do not communicate state with color alone.
- Prefer provider-neutral behavior when a change applies to every agent, while
  preserving provider-native event order and session semantics.
- Verify behavior with end-to-end checks appropriate to the change.

## Checks

Run the relevant checks from `fintwind-go/`:

```powershell
go build ./...
go vet ./internal/... ./cmd/daemon ./cmd/app ./cmd/mygo
go run github.com/egoist/mygo/cmd/mygo vet ./cmd/app
```

GitHub runs Tests on pushes to `main` and on pull requests. Use **Actions →
Tests → Run workflow** to check a branch without opening a PR.

## Pull requests

In the pull request description:

- Explain the problem and the chosen solution.
- List the checks you ran.
- Call out known limitations or follow-up work.
- Link the related issue, if one exists.

By contributing, you agree that your contribution will be licensed under the
[GNU General Public License v3.0 only](LICENSE).
