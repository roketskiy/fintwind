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
- **A Direct3D 11 driver at feature level 11_0 or newer.**
- **x86_64 or aarch64.**

SmartScreen may warn about an unrecognized publisher on first launch when the
release was not code-signed. Choose **More info → Run anyway**.

## Updating

Download the new installer or portable archive from the
[GitHub releases page](https://github.com/roketskiy/fintwind/releases).
Close the running application before replacing the executables. The installer
and portable archive do not contain your application data.

## Where fintwind keeps its data

| What | Path |
| --- | --- |
| Tasks, sessions, transcripts | `%LOCALAPPDATA%\Fintwind\app.db` |
| Attachments and blobs | `%LOCALAPPDATA%\Fintwind\blobs` |
| Desktop settings | `%LOCALAPPDATA%\Fintwind Go\app-settings.json` |
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
The same section can check the version and install OpenCode using npm or Bun.
In-app updates apply only to installations managed by the current npm or Bun;
portable and other installations must be updated separately.

If the CLI is installed but not detected, make sure `opencode --version` works
in a new PowerShell window, or select its executable in settings.

## Terminal

The built-in terminal opens PowerShell 7 (`pwsh.exe`) when it is installed,
then Windows PowerShell, then whatever `COMSPEC` names. Ctrl+Shift+C and
Ctrl+Shift+V copy and paste so Ctrl+C stays available to the shell.

Web links and login authorization open in the default browser. Fintwind does not embed a browser or provide browser automation tools.

## Troubleshooting

**The window opens black, or the app exits at startup.** fintwind needs a working
Direct3D 11 device. Update the GPU driver; in a VM, enable 3D acceleration.

**OpenCode is listed as not installed.** Open a new PowerShell window and run
`opencode --version`. If the shell cannot find it either, the install did not
put a shim on `PATH`. If the shell finds it but fintwind does not, file an
issue with the install method.

**Git-backed features do nothing.** fintwind shells out to `git`. Install Git for
Windows and make sure `git --version` works in a new terminal.
