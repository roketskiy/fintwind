# fintwind

English | [简体中文](README.zh-CN.md)

A native Windows desktop client for [OpenCode 2](https://opencode.ai/v2/docs).
Built with Rust and [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui),
not Electron. Manage local and SSH remote projects, work in multiple session tabs,
and follow the agent's replies, reasoning, and tool activity in one window.

[Downloads](https://github.com/roketskiy/fintwind/releases/latest) · [Changelog](CHANGELOG.md)

Session demo: switching models, a session in progress, and scrolling a long transcript.

https://github.com/user-attachments/assets/beb2dcb5-88dd-4f2b-9415-153af9c93bf9

## Install

### 1. Install OpenCode 2

fintwind requires the local `opencode` CLI; it is not bundled with the app.
Choose **one** of these commands in PowerShell:

With [Node.js and npm](https://nodejs.org/):

```powershell
npm install -g @opencode/cli
```

Or with [Bun](https://bun.sh/):

```powershell
bun install -g --trust @opencode/cli
```

The Bun `--trust` flag allows the package's required install script.
These use the V2 package, `@opencode/cli`; see the
[official installation guide](https://opencode.ai/v2/docs/) for other methods.
After installing fintwind, you can also install or update OpenCode from
**Settings → General → Runtime environment** with npm or Bun. In-app updates
are limited to installations managed by the current npm or Bun; update portable
and other installations yourself.

Open a new PowerShell window and verify the installation:

```powershell
opencode --version
opencode
```

In the OpenCode terminal interface, use `/connect` to connect a model provider,
or use your existing provider configuration. fintwind uses OpenCode's models,
provider credentials, and MCP configuration. You do not need to start
`opencode serve` yourself: fintwind starts its own private local server.

This release supports OpenCode up to 2.3.2. Version requirements for optional
features:

- Scheduled task MCP: OpenCode 2.1.4+. Enable it in
  **Settings → General → Features**; Fintwind manages the experimental MCP
  connection automatically. See [agent-created scheduled tasks](docs/scheduled-tasks-mcp.md).
- Image generation: OpenCode 2.2.1+ and a compatible connected image model.
- Native Goal: integration targets OpenCode 2.2.1; older versions without native
  Goal cannot use it. See [Goal](docs/goals.md).
- Background compaction: OpenCode 2.3.1+.

### 2. Install fintwind

- **System:** Windows 10 version 1809 or newer, or Windows 11; x86_64 or Arm64.
- **Graphics:** a Direct3D 11 driver supporting feature level 11_0 or newer.
- **Git features:** [Git for Windows](https://git-scm.com/download/win), available on `PATH`.

Download `fintwind-<version>-<arch>-Setup.exe` from the
[latest release](https://github.com/roketskiy/fintwind/releases/latest).
The installer runs per-user in `%LOCALAPPDATA%\Programs\fintwind` and does not
require administrator rights.

For the portable version, extract the release `.zip` and run `fintwind.exe`.
**Keep `fintwind.exe` and `fintwind-daemon.exe` in the same folder** — the app
launches the daemon from its own directory. Official release packages also include the backend used by
[SSH remote connections](#ssh-remote-connections); no separate download is needed.

When a newer release is available, the update card can install it in one
click: fintwind downloads the installer for your architecture, verifies it
against the sha256 published with the release, installs silently without
administrator rights, and restarts into the new version. Portable installs have
no install record, so their update entry still opens the release page. See
[Windows setup and troubleshooting](docs/windows.md) for SmartScreen, CLI
detection and data locations.

## Features

- **SSH remote workspaces.** Connect to a Linux server and use its projects and
  sessions like local ones, side by side with local tasks in one window. The
  backend installs automatically; switch hosts at the bottom of the sidebar.
  See [SSH remote connections](#ssh-remote-connections).
- **Phone remote control.** Enable it from the desktop sidebar, then scan the QR
  code or open the pairing link to use the mobile web client. Switch workspaces,
  view and create sessions, send messages and attachments, stop tasks, and handle
  approvals and questions over an end-to-end encrypted connection without an
  account. The desktop must stay running; access covers workspaces registered
  when remote control starts and sessions updated within the last 72 hours.
  Stopping remote control, closing the desktop, or pairing again invalidates the
  old pairing. See [remote control](docs/remote-control.md).
- **Projects and session tabs.** Group sessions by project and open multiple
  sessions in one window, with tab reordering and unread indicators. The
  add-project dialog offers existing folder, GitHub clone, and new folder; the
  project name is optional and defaults to the folder name. New sessions without
  a project share a fixed general workspace. Change its folder in
  **Settings → General → Data management**; existing sessions keep their paths.
- **Session controls.** Switch model, reasoning effort where supported, and
  access mode (Ask, Auto-accept edits, Full access). Queue or steer a follow-up
  while the agent works; queued messages live inside the composer card in a
  collapsible section and reorder by dragging. Choose the default send behavior
  in General settings. Question cards can be skipped, collapsed, or expanded.
  Use OpenCode's native revert and restore for eligible Git-backed turns. `/btw`
  asks a one-off side question from the session's settled context without
  disturbing the task in progress.
- **Turn recovery.** Recover missed replies and completion events from OpenCode's
  stored history. History reads accept responses up to 64 MiB and reduce page
  sizes when image or tool payloads make a page too large. Interrupted sessions
  repair stale Working projections instead of staying stuck, and dead runtimes
  are rebuilt on the next submit.
- **Context compaction.** Handled by OpenCode's native automatic and background
  compaction: context is compacted when input-window occupancy reaches the
  threshold, and background compaction prepares summaries ahead of the threshold
  (OpenCode 2.3.1+). The threshold is a percentage; workspace or custom OpenCode
  configuration takes precedence. Both are on by default.
- **Repeated-output protection.** A task is stopped automatically when model
  replies, or tool calls and their results, repeat identically 10 times in a row.
  On by default; can be turned off in General settings.
- **Readable agent activity.** View streaming replies, reasoning, tool calls,
  subagent activity, and nested Code Mode calls with OpenCode's tool names. The
  background work panel lists background processes and subagents with steering
  messages and running durations; inactive subagents fold away by default.
  Session errors appear as persistent activity cards, and activity groups show
  their elapsed time and step count. Transcripts and reasoning use virtualized
  lists to limit per-frame work.
- **Files and Git.** Browse workspace files and open multiple file tabs. Review
  staged, unstaged, untracked, and ignored files in a directory tree, load
  directories on demand, and inspect nested repositories and per-file diffs.
  Read Git history in a fixed-lane branch graph with branch and tag labels.
  A card-style commit box supports Ctrl+Enter, with commit, pull, push, and more
  actions in a split button; a failed branch switch keeps its state and offers
  retry. Attach files from the composer or by dropping them onto the
  conversation; images and PDFs are sent inline to OpenCode.
- **Terminal.** Work in a built-in terminal alongside the session.
- **Usage and subscriptions.** The usage page combines overview KPIs, a usage
  flow diagram (model shares feeding the total composition of cache reads, input,
  output, cache writes, and reasoning; click to highlight a flow), a subscription
  quota carousel, trend charts, model/provider/project rankings, and an activity
  heatmap, including subagent usage. Single-day statistics offer hourly charts
  and day-by-day navigation; ranges default to 7 days, with single day, 30 days,
  and all-time also available. The quota view covers connected, supported
  providers, including local Claude Code and Codex subscriptions. See
  [provider quotas](docs/provider-quota.md) for sources and limitations.
- **Model catalog.** A built-in, slimmed models.dev table keeps only official
  entries from recognized AI labs and models released within the last two years,
  with name, context window, input/output modalities, and reasoning efforts.
  Models and the provider list are read offline at runtime. Custom models can
  select multiple reasoning efforts (none/low/medium/high/xhigh/max); leaving
  the selection empty follows the catalog or the standard six levels.
- **Provider management.** Sign in and out of built-in providers, see at a
  glance which connections need re-login, and read the provider's own rejection
  reason (quota, key, or model) when a request fails. Custom provider API keys
  use OpenCode credentials; models can inherit the provider protocol or select
  their own, including OpenAI Images and Google Images.
- **Image generation.** OpenCode's experimental image tool is available with a
  supporting OpenCode version (2.2.1+) and connected image models. General
  settings choose the default image model; "Automatic" lets OpenCode pick an
  available model per request.
- **Native Goal.** Set a goal with `/goal <goal>`, then inspect, pause, resume,
  or clear it. OpenCode owns goal evaluation and continuation. Old Fintwind goals
  are not migrated automatically.
- **Session Todo.** Follow the agent's task list and completion progress in the
  session capsule, alongside pending added and removed line counts.
- **Keep awake.** Optionally keep the display and system awake while Fintwind runs.
- **OpenCode runtime.** Check the version and install or update an npm- or
  Bun-managed OpenCode in general settings. You can also choose a portable
  executable without adding it to PATH and restore automatic detection later.
- **Old session cleanup.** Preview and delete sessions inactive for 30, 90, or
  180 days (90 by default). Cleanup covers sessions visible in Fintwind and
  linked to OpenCode, removing the corresponding records from both.
- **[Context composition](docs/context-composition.md).** A treemap compares
  eight estimated categories, including compaction summaries, tool calls, and
  tool results. Reported occupancy stays separate, and unmeasurable content is
  marked explicitly.
- **App updates.** Stable builds offer the most recently published full release,
  even when a corrected version number is lower than the installed one. Drafts
  and prereleases are excluded, and the current release is not offered again.
- **Scheduled tasks.** Open the clock icon in the sidebar to run instructions
  on selected weekdays at a set time, at a fixed interval, or once at a specific
  date and time. Pick the date from a calendar; one-shot tasks deactivate after
  running but keep history. Tasks record their IANA time zone at creation and
  run against it. Create a session with a chosen project, model, and access
  mode, or reuse an existing session. Pause, resume, run immediately, and
  inspect results. Tasks do not run while Fintwind is fully closed, and missed
  runs are not replayed. Enable **Scheduled task MCP** in General settings to
  let agents create tasks. It is off by default and requires OpenCode 2.1.4+.
  Agents ask for missing task details before creating a task. See
  [the MCP reference](docs/scheduled-tasks-mcp.md).
- **MCP management.** Browse search, office, design, data, automation,
  operations, and business categories. Add remote servers, complete OAuth
  login, and check connection status. Listings use vendor-operated endpoints
  where available. MCP login for remote hosts completes in the local browser.
- **Native Mermaid diagrams.** Read flowcharts, sequence diagrams, class
  diagrams, state diagrams, and more directly in conversations. Click a diagram
  (or press Enter/Space on it) to open a fullscreen preview with zoom in, zoom out,
  and reset to 1:1. Diagrams use accent-tinted surfaces and a color series that adapts
  to light and dark themes. Switch to source or inspect compilation errors.
  Enable the Mermaid visualization prompt in General settings to give new
  sessions diagram guidance and a built-in validation tool. Existing sessions
  retain their original preference.
- **Native math.** Typeset inline and display formulas, including `\(…\)` and
  `\[…\]`, with layout work performed on background workers. Inline math renders
  at 1.21x and display math at 1.25x; overwide formulas keep natural size on their
  own horizontally scrollable row.
- **Appearance settings.** Choose Fintwind or one of 13 additional theme
  families, select light, dark, or automatic mode, and customize UI and code
  fonts and text sizes. Automatic mode switches to light at 07:00 and dark at
  19:00 local time; it does not follow the system appearance. Animations
  honor the system's reduce-motion preference.
- **Anonymous launch statistics.** Stable and beta builds report a randomly
  generated local installation ID to the project website on each launch to
  count daily active desktops. No sessions, project paths, or account data
  are sent.

## SSH remote connections

Connect to a Linux server and use its projects and sessions directly in
fintwind, side by side with local tasks in one window. fintwind installs and
updates the backend it needs on the server automatically — no manual setup.

### What you need

- A Linux server (x86_64 or ARM64) you can reach over SSH from this computer.
- A private key file for login, **without a passphrase**; password login is not
  supported.
- OpenCode on the server; if it is missing, fintwind can install it after you
  connect. Cloning projects requires Git on the server.

### Getting started

1. Open **Settings → SSH Connections → New connection**, fill in the name, host,
   port, and username, choose the private key file, then choose **Save and connect**.
2. Click **Add project** in the session area, pick the server, and choose one of
   three methods:
   - **Existing folder**: enter the project's path on the server.
   - **Clone from GitHub**: enter the repository URL. The clone runs on the
     server with live progress; private repositories require access already
     configured on the server.
   - **New folder**: create an empty project.

   An existing directory is never overwritten; if adding fails, your input is
   kept and you can retry.
3. Use it like any local project. The switcher at the bottom of the sidebar
   moves between **This computer** and each server; remote sessions are restored
   after reconnecting or upgrading the desktop.

### Good to know

- The first connection remembers the server's fingerprint; later connections
  are blocked if it changes.
- Everything travels over the encrypted SSH connection; no extra ports need to
  be opened on the server.
- Providers, MCP, skills, and general settings can be switched to the server
  with the picker at the bottom of the settings sidebar; changes apply to the
  server, and you can check, install, and update its OpenCode. While
  disconnected, server settings are read-only; appearance and SSH connection
  settings always belong to this computer.
- While tasks are running on the server, backend updates wait for them to finish.
- **Remove connection** deletes only the connection info saved on this computer;
  projects on the server are untouched.

## Local data and scope

fintwind has no account of its own and does not provide cloud synchronization.
App-managed projects, tasks, and attachments are stored in local SQLite and
files; OpenCode owns its native session data, and SSH remote session data is
owned by OpenCode on the server. **Local storage does not mean offline
inference:** prompts and relevant content are sent to the model providers and
tools you configure in OpenCode.

This is an independent [GPL-3.0-only](LICENSE) fork of
[waku](https://github.com/egoist/waku) by [EGOIST](https://github.com/egoist),
not the official OpenCode desktop app. fintwind ships for Windows and uses
OpenCode 2 as its only agent backend. Multiple model providers are available
through OpenCode; other agent backends are outside this fork's scope.

### Memory snapshot

One Task Manager snapshot shows the desktop at 69.4 MB and
`fintwind-daemon.exe` at 25.7 MB — about 95 MB combined. This is not a benchmark
or a total for the full agent stack: it excludes OpenCode and any other
processes, and actual usage varies by workload.

![Task Manager: fintwind at 69.4 MB, fintwind-daemon.exe at 25.7 MB](docs/media/runtime-memory.png)

## Architecture

The desktop uses [`fintwind-client`](crates/fintwind-client) to communicate with
a separate [`fintwind-daemon`](crates/fintwind-daemon) process over the
authenticated, versioned WebSocket contract in
[`fintwind-protocol`](crates/fintwind-protocol).
[`fintwind-core`](crates/fintwind-core) implements daemon-owned storage,
workspace and Git operations, and the OpenCode integration. The local daemon
listens on loopback and authenticates clients with a token generated at launch.
For SSH hosts, the desktop speaks the same protocol to the Linux daemon deployed
on the server through an SSH tunnel, reconnecting automatically when the tunnel
drops.

Release desktop preferences live in `~/.fintwind/app.json`; debug builds use
`temp/app.json`. Daemon provider settings live in `~/.fintwind/settings.json`.
SSH connection profiles live in `%LOCALAPPDATA%\Fintwind\ssh-profiles.json`.
New sessions without a project share `%LOCALAPPDATA%\Fintwind\projects` by
default. You can choose another folder in General settings. Existing sessions
keep their directories, including older dated workspaces under
`~/.fintwind/projects/<date>/<slug>`.

## Development

Requires Windows, the MSVC C++ toolchain and Windows SDK,
[Rust 1.97 or newer](https://www.rust-lang.org/tools/install), and
[Bun](https://bun.sh/). Follow [CONTRIBUTING.md](CONTRIBUTING.md) for setup and checks.

```sh
bun install
bun run dev
```

The development watcher builds the app and a separate
`target/debug/fintwind-debug-daemon.exe`; provider-only changes can replace the
daemon without restarting the debug app. Release builds place
`fintwind-daemon.exe` beside `fintwind.exe`.

See [RELEASING.md](RELEASING.md) for packaging and release maintenance.

## License

[GNU General Public License v3.0 only](LICENSE).
