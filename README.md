# fintwind

English | [简体中文](README.zh-CN.md)

A native Windows desktop client for [OpenCode 2](https://opencode.ai/v2/docs).
Built with Go and [MyGo](https://github.com/egoist/mygo),
not Electron. Manage projects, work in multiple session tabs, and follow the
agent's replies, reasoning, and tool activity in one window.

[Downloads](https://github.com/roketskiy/fintwind/releases/latest) · [Changelog](CHANGELOG.md)

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
launches the daemon from its own directory.

For updates, download a newer installer or portable archive from the release page.
See [Windows setup and troubleshooting](docs/windows.md) for data locations.

## Features

The Go edition supports projects, conversation tabs, provider settings, MCP server
settings, source control, and a built-in terminal. It uses the local OpenCode
installation for agent interactions. Some features of the former Rust edition
are not yet available in the Go edition.

Web links and login authorization open in the system browser. Fintwind does not
embed a browser or expose browser automation tools.

## Local data and scope

fintwind has no account of its own and does not provide cloud synchronization.
App-managed projects, tasks, and attachments are stored in local SQLite and
files; OpenCode owns its native session data. **Local storage does not mean
offline inference:** prompts and relevant content are sent to the model
providers and tools you configure in OpenCode.

This is an independent [GPL-3.0-only](LICENSE) fork of
[waku](https://github.com/egoist/waku) by [EGOIST](https://github.com/egoist),
not the official OpenCode desktop app. fintwind ships for Windows and uses
OpenCode 2 as its only agent backend. Multiple model providers are available
through OpenCode; other agent backends are outside this fork's scope.

## Architecture

The Go desktop and daemon live in [`fintwind-go/`](fintwind-go). The desktop
starts `fintwind-daemon.exe` from its own directory. The daemon listens on a
loopback address and authenticates clients with a token generated at launch.
Application data is stored under `%LOCALAPPDATA%\Fintwind`; daemon provider
settings are stored in `%USERPROFILE%\.fintwind\settings.json`.

The former Rust implementation remains in the repository as source. It is no
longer the maintained or released application.

## Development

Use Windows, PowerShell 7, and the Go version specified in
[`fintwind-go/go.mod`](fintwind-go/go.mod). Run `MyGo.cmd` from the repository
root to build and start the application with your existing Fintwind data. See
[CONTRIBUTING.md](CONTRIBUTING.md) for development and build instructions and
[RELEASING.md](RELEASING.md) for packaging.

## License

[GNU General Public License v3.0 only](LICENSE).
