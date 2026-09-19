# fintwind

> 🇨🇳 [中文文档](README.md) | English

fintwind is a native Windows desktop app for [OpenCode](https://opencode.ai). It is
built in Rust with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
and keeps projects, sessions, transcripts on your machine.

> fintwind is a fork of [waku](https://github.com/egoist/waku) by
> [EGOIST](https://github.com/egoist), and stays under [GPL-3.0](LICENSE) like
> upstream. Unlike waku, this fork converges on a single OpenCode backend:
> all other agent providers and the macOS/Linux builds were removed in favor of
> Windows-first development. The upstream project remains the place for the
> multi-provider, cross-platform app.

## Install

Run `fintwind-<version>-<arch>-Setup.exe` from the
[latest release](https://github.com/roketskiy/fintwind/releases/latest). It installs
per-user and updates itself. A portable `.zip` is published alongside it. See
[docs/windows.md](docs/windows.md) for requirements and what is not available
there yet.

## Requirements

fintwind drives one agent backend: a local
[OpenCode](https://opencode.ai) server. Install and authenticate the
`opencode` CLI first; fintwind starts it automatically, and sessions, models,
providers, and MCP servers all come from that single backend.

## Highlights

- **Truly native, not Electron**: the UI is rendered directly on the GPU by
  Rust + GPUI; only the optional right-panel browser uses the built-in
  system WebView2.
- **Smooth with long conversations**: transcripts and reasoning are rendered
  with virtualized lists that build only what is visible each frame, keeping
  streaming output fluid even on high-refresh-rate displays.
- **Respects your system**: dark/light theme follows the OS appearance,
  animations honor the system reduce-motion setting, and key interactions
  are fully keyboard-operable.
- **Windows-native integration**: system tray, native menus, a built-in
  terminal, per-user install, and signed automatic updates behave the way
  Windows users expect.
- **Local-first**: projects, sessions, transcripts, and attachments all stay
  on your machine (SQLite + local files), with the daemon running locally.
  No account, no cloud dependency.
- **Zero telemetry by default**: telemetry is off by default; configuration
  is just local JSON files you can back up and migrate freely.
- **Session control**: switch models, reasoning effort, and access modes
  from one interface; queue or steer follow-up messages while an agent
  works; rewind Git-backed tasks with conversation-aware checkpoints.

## Architecture

The native desktop is an RPC client of the standalone `fintwind-daemon` process.
Provider sessions run in [`fintwind-core`](crates/fintwind-core), behind the
authenticated, versioned WebSocket contract in
[`fintwind-protocol`](crates/fintwind-protocol). fintwind Desktop depends on
[`fintwind-client`](crates/fintwind-client), not on the daemon implementation. The
daemon owns task SQLite data, uploaded attachments, provider-native session
forks, and all workspace filesystem and Git operations; paths returned by it
always refer to the daemon host. The desktop retains only presentation state
and a disposable preview cache.

Projectless task workspaces live on the daemon host under
`~/.fintwind/projects/<date>/<slug>`. The daemon moves workspaces created by the
older `~/.fintwind/<date>/<slug>` layout on first load.

Configuration ownership is separate too: the Release desktop writes
`~/.fintwind/app.json`, while Debug stays isolated at `temp/app.json`. Daemon
provider settings live in `~/.fintwind/settings.json`. The daemon listens on
loopback only and authenticates every client with a per-launch token.

When connected to a daemon managed outside the desktop process, fintwind never
interprets daemon paths on the client machine. The local folder picker and PTY
are therefore unavailable until the protocol gains daemon-host picker and
terminal-stream endpoints; files, diffs, Git, skills, usage, task state, and
attachments already use daemon RPC.

Release apps bundle and sign `fintwind-daemon`. Development keeps the daemon at
`target/debug/fintwind-debug-daemon`, allowing provider-only edits to rebuild and
replace the daemon without relaunching fintwind Debug.

## Development

Development requires Windows 10 1809 or newer, the MSVC toolchain,
[Rust 1.96 or newer](https://www.rust-lang.org/tools/install), and
[Bun](https://bun.sh/). Install the native build prerequisites listed in
[CONTRIBUTING.md](CONTRIBUTING.md) first.

```sh
bun install
bun run dev
```

The right-panel browser runs on WebView2. Agent sessions, projects,
transcripts, skills, usage, diffs, file editing, and the terminal also run
natively.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and checks.
Release maintainers should also read [RELEASING.md](RELEASING.md).

## License

fintwind is licensed under the [GNU General Public License v3.0 only](LICENSE).
