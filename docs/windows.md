# fintwind on Windows

## Install

Download `fintwind-<version>-x86_64-Setup.exe` (or the `aarch64` installer on an
Arm device) from the
[GitHub release](https://github.com/roketskiy/fintwind/releases) and run it. It
installs per-user into `%LOCALAPPDATA%\Programs\fintwind`, so it never asks for
administrator rights.

### Portable

`fintwind-<version>-<target>.zip` is the same build without an installer. Unpack it
anywhere and run `fintwind.exe`.

**Keep the two executables together.** fintwind launches `fintwind-daemon.exe` from its
own directory, so moving `fintwind.exe` out on its own leaves it unable to start
the daemon. A shortcut is fine.

fintwind expects:

- **Windows 10 version 1809 or newer**, or Windows 11.
- **A Direct3D 11 driver at feature level 11_0 or newer.** GPUI renders
  through DirectX and falls back to the Microsoft Basic Render Driver, so it
  can run in a VM — see Troubleshooting if the window comes up black.
- **x86_64 or aarch64.**

Nothing else: fintwind links the C runtime statically, so there is no Visual C++
redistributable to install first. That matters most on Arm devices, which
rarely have the arm64 redistributable already.

SmartScreen may warn about an unrecognized publisher on first launch when the
release was not code-signed. Choose **More info → Run anyway**.

## Updating

An installed copy updates itself. When fintwind starts it asks the update
gateway (`fintwind.roketskiy.top`, which caches the
[GitHub release](https://github.com/roketskiy/fintwind/releases) list so the
check never hits the GitHub API rate limit) whether a newer release exists,
and the notice it shows carries **Update now**: fintwind downloads the installer for
your architecture, checks it against the digest published with the release,
and installs it silently. No UAC prompt, because the installer is per-user.
fintwind closes while it installs and comes back by itself. **Go to Release to
download** stays on the card if you would rather do it by hand.

GitHub resolves to several addresses, and some of them are unreachable from
some networks — so before downloading, fintwind measures the publisher's own
host and two public relays of the release asset, and uses whichever answers
fastest. The card shows that measurement as **Choosing a download source…**.
During the download, the card shows progress, percentage, and downloaded size.
When the total size is unknown, it shows the downloaded size.
This costs nothing in trust: every download is still checked against the
digest the release published, so a relay that served anything else is refused
before the installer is ever launched.

The update installs through the same installer a manual download runs, so it
only applies to an installed copy. A copy unpacked from the portable zip has no
installer record to update, and pointing one at the installer would install a
second copy in `%LOCALAPPDATA%\Programs` instead of replacing it — so a portable
copy is only ever offered the release page.

Either way your data is untouched: installing over an existing copy replaces the
executables in place — see the next section.

## Where fintwind keeps its data

| What | Path |
| --- | --- |
| Tasks, sessions, transcripts | `%LOCALAPPDATA%\Fintwind\app.db` |
| Attachments and blobs | `%LOCALAPPDATA%\Fintwind\blobs` |
| Settings | `%USERPROFILE%\.fintwind\app.json` |
| Daemon settings | `%USERPROFILE%\.fintwind\settings.json` |
| Default general workspace for new sessions without a project | `%LOCALAPPDATA%\Fintwind\projects` |

Unpacking a new release over the old directory leaves all of it untouched.
Choose a different general workspace in **Settings → General → Data management**.
Changing or resetting that folder affects new sessions; existing sessions keep
their directories and files, including older dated workspaces under
`%USERPROFILE%\.fintwind\projects`.

## The OpenCode CLI

fintwind drives one agent backend: the `opencode` CLI. It detects the binary on
`PATH` and, because a fresh `PATH` may predate an install, also looks in the
usual per-user prefixes: `%APPDATA%\npm`, `%USERPROFILE%\.bun\bin`,
`%USERPROFILE%\.cargo\bin`, `%USERPROFILE%\scoop\shims`, and
`%LOCALAPPDATA%\Microsoft\WindowsApps`.

Bare names resolve through `PATHEXT`, so the `opencode.cmd` shim npm installs
is found the same way `opencode` would be in a shell. Nothing is spawned with a
console window attached.

You can also choose an executable, including a portable `opencode-cli.exe`, in
**Settings → General → Runtime environment**. The selected path applies to
subsequent session starts and background operations. Running sessions keep
their existing process. Restore automatic detection from the same setting.

If the CLI is installed but not detected, make sure `opencode --version` works
in a new PowerShell window, or select its executable in settings.

### Scheduled task MCP

Agent-created scheduled tasks require OpenCode 2.1.4 or newer. Enable
**Scheduled task MCP** in **Settings → General → Features**. It is off by
default. Enabling it automatically connects OpenCode's experimental client-hosted
MCP interface, including for existing sessions. No separate experimental flag
or MCP configuration entry is needed. Disabling it removes the tool; saved tasks
continue to follow their schedules. See [the MCP reference](scheduled-tasks-mcp.md).

## Terminal

The built-in terminal opens PowerShell 7 (`pwsh.exe`) when it is installed,
then Windows PowerShell, then whatever `COMSPEC` names. Ctrl+Shift+C and
Ctrl+Shift+V copy and paste so Ctrl+C stays available to the shell.

## Browser

The right panel's Browser tab runs on WebView2, which is in-box on Windows 11
and evergreen-installed on Windows 10. Navigation, devtools, downloads, and
pop-up handling are native WebView2.

fintwind hosts it in *visual* mode rather than as a child window: the page renders
into a DirectComposition visual that GPUI hands out between its own content
and its overlay plane, so menus, tooltips and dialogs composite above a live
page instead of hiding it. That is also why the browser needs a working
composition path — see the black-window note under Troubleshooting.

Differences worth knowing:

- **No load progress in the toolbar.** WebView2 reports no equivalent of
  WebKit's `estimatedProgress`, so the bar stays empty while a page loads.
- **Devtools open but do not toggle.** WebView2 offers no way to ask whether
  its devtools window is open, or to close it, so the shortcut only opens and
  refocuses it.
- **Pen, touch and dragging files into the page are not wired up.** Visual
  hosting delivers no input of its own; fintwind forwards mouse, wheel, cursor and
  focus, and leaves `SendPointerInput` and the external drop target for later.
  Keyboard and IME are unaffected — those still reach the page directly once
  it holds focus.

Developer-only same-page Playwright validation is documented in
[browser-automation.md](browser-automation.md). It uses a separate build and
test profile; ordinary builds do not enable a CDP debugging endpoint.

## Troubleshooting

**The window opens black, or the app exits at startup.** fintwind needs a working
Direct3D 11 device. Update the GPU driver; in a VM, enable 3D acceleration.

**OpenCode is listed as not installed.** Open a new PowerShell window and run
`opencode --version`. If the shell cannot find it either, the install did not
put a shim on `PATH`. If the shell finds it but fintwind does not, file an
issue with the install method.

**Git-backed features do nothing.** fintwind shells out to `git`. Install Git for
Windows and make sure `git --version` works in a new terminal.
