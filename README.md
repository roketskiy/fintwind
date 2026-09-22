# fintwind

English | [简体中文](README.zh-CN.md)

fintwind is a native Windows desktop for [OpenCode](https://opencode.ai). Rust and
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) draw the
interface on the GPU — it is not an Electron shell. Projects, sessions, and
transcripts stay on your machine.

54 seconds: switching models, a session in progress, and scrolling a long transcript.

https://github.com/user-attachments/assets/8ca3cd89-3044-4e90-81cd-ebd9f55818ef

## Install

Download `fintwind-<version>-<arch>-Setup.exe` from the
[latest release](https://github.com/roketskiy/fintwind/releases/latest) and run
it. The installer is per-user (`%LOCALAPPDATA%\Programs\fintwind`) and does not
ask for administrator rights. A portable `.zip` is published beside it.

Keep `fintwind.exe` and `fintwind-daemon.exe` in the same folder. The app
starts the daemon from its own directory; moving one without the other leaves
it unable to launch.

Windows 10 version 1809 or newer, or Windows 11, on x86_64 or Arm64. SmartScreen,
data locations, and what is not supported yet are in
[docs/windows.md](docs/windows.md).

When a newer release exists, an **Update** button opens that release page.
fintwind does not download or install the update itself.

## Requirements

fintwind drives one agent backend: a local [OpenCode](https://opencode.ai)
server. Install and authenticate the `opencode` CLI first. fintwind starts it,
and sessions, models, providers, and MCP servers all come from that backend.

## Highlights

- **Native rendering.** The session UI is Rust + GPUI. Only the optional
  browser panel on the right uses the system WebView2.
- **Long transcripts stay responsive.** Transcripts and reasoning are
  virtualized, so each frame builds only the rows on screen. Streaming holds
  up on a high-refresh display.
- **Follows the system.** Light and dark track the Windows theme. Animations
  honor reduce-motion. The main controls are keyboard-operable.
- **A Windows shell.** System tray, native menus, a built-in terminal, and a
  per-user install.
- **Local-first.** Projects, sessions, transcripts, and attachments live on
  the machine (SQLite and local files). The daemon listens on loopback only
  and authenticates clients with a token minted at launch. fintwind has no
  account of its own and does not sync to a cloud.
- **Telemetry off by default.** Settings are a local JSON file you can back
  up or copy.
- **The session stays in your hands.** Switch model, reasoning effort, and
  access mode (Ask, Auto-accept edits, Full access) from the composer. Queue
  or steer a follow-up while the agent is working. Revert a Git-backed turn
  through OpenCode's own revert, in the same session.
- **Tool activity you can read.** Thinking, tool calls, and nested Code Mode
  calls stay in order, with the raw OpenCode tool name on the card.
- **Usage from your own history.** Token and cost stats read from local
  OpenCode sessions: an activity heatmap, daily bars, and a per-model ranking.
- **MCP marketplace.** Browse and add remote MCP servers, including OAuth
  login, and see whether they are connected.
- **Math as math.** Inline and display formulas are typeset natively,
  including the `\[…\]` and `\(…\)` delimiters models actually emit, without
  blocking the UI thread.

## Memory

One Task Manager reading with the window open. The desktop process is at
69.4 MB and `fintwind-daemon.exe` is at 25.7 MB, about 95 MB together.

![Task Manager: fintwind at 69.4 MB, fintwind-daemon.exe at 25.7 MB](docs/media/runtime-memory.png)

## Scope

fintwind is a [GPL-3.0](LICENSE) fork of [waku](https://github.com/egoist/waku)
by [EGOIST](https://github.com/egoist). This fork keeps a single backend —
local OpenCode — and ships Windows only. Multi-provider, macOS, and Linux
stay on the upstream project.

It is not the official OpenCode desktop. It is a Windows client for people who
already run OpenCode and want the transcript in a native window.

## Architecture

The desktop is an RPC client of a separate `fintwind-daemon` process. Provider
sessions run in [`fintwind-core`](crates/fintwind-core), behind the
authenticated, versioned WebSocket contract in
[`fintwind-protocol`](crates/fintwind-protocol). The desktop depends on
[`fintwind-client`](crates/fintwind-client), not on the daemon implementation.

The daemon owns the task SQLite database, uploaded attachments, provider-native
session forks, and every workspace filesystem and Git operation. Paths it
returns always refer to the daemon's machine. The desktop keeps presentation
state and a disposable preview cache. The daemon listens on loopback only, and
authenticates each client with a token minted for that launch.

Workspaces for tasks that are not tied to a project live under
`~/.fintwind/projects/<date>/<slug>` on the daemon machine. On first load, the
daemon moves workspaces created by the older `~/.fintwind/<date>/<slug>` layout.

Configuration is split the same way. Release builds write
`~/.fintwind/app.json`. Debug builds stay in `temp/app.json`. Daemon provider
settings live in `~/.fintwind/settings.json`.

Connected to a daemon hosted outside the desktop process, fintwind does not
interpret that daemon's paths on the client machine. The local folder picker
and PTY stay unavailable until the protocol has daemon-host picker and
terminal-stream endpoints. Files, diffs, Git, skills, usage, task state, and
attachments already go through daemon RPC.

## Development

Development requires Windows 10 1809 or newer, the MSVC toolchain,
[Rust 1.96 or newer](https://www.rust-lang.org/tools/install), and
[Bun](https://bun.sh/). Install the native build prerequisites listed in
[CONTRIBUTING.md](CONTRIBUTING.md) first.

```sh
bun install
bun run dev
```

Release builds ship `fintwind-daemon` beside the app. In development the daemon
is `target/debug/fintwind-debug-daemon`, so provider-only changes can replace
the daemon without restarting the debug app.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and checks.
Release maintainers should also read [RELEASING.md](RELEASING.md).

## License

fintwind is licensed under the [GNU General Public License v3.0 only](LICENSE).
