#!/usr/bin/env python3
"""Re-exec the synthetic provider under a minimal whitelist environment.

The provider is spawned by the Fintwind driver as a plain child, so it
inherits whatever the desktop process carries. Deleting the obvious
`*_API_KEY` / `*_TOKEN` names is not a guarantee: a real `OPENCODE_CONFIG`,
a global config home, or a shared-server variable can name a real provider
context without either name. This launcher therefore never inherits
anything: it builds the child environment from a fixed whitelist of
operating-system essentials plus the two private credentials the driver
injects for the server it just started, and re-execs the provider with it.

The whitelist is deliberate about what it excludes: no `HOME`,
`USERPROFILE`, `APPDATA` or `LOCALAPPDATA` (no user profile context), no
`OPENCODE_CONFIG*` (no global configuration), no `*_API_KEY`, `*_TOKEN`,
`*_SECRET` or `*_CREDENTIAL*` (no real provider credentials), and nothing
else that was not named here. The process tree below the provider inherits
this same environment.

The provider never reaches the network, never starts a real model, and
never touches a real workspace; this launcher is the second half of that
guarantee, applied to the environment it runs under.

Usage: launch_isolated.py <script.py> [args...]
"""

import os
import subprocess
import sys

# Operating-system essentials only. Values are copied verbatim when present;
# nothing is invented.
ESSENTIALS = (
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "PATHEXT",
    "PATH",
    "TEMP",
    "TMP",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
)

# The two variables the Fintwind driver injects for the private server it
# just started. These authenticate against that one spawned server; they
# are not a shared-server context.
DRIVER_INJECTED = (
    "OPENCODE_SERVER_USERNAME",
    "OPENCODE_SERVER_PASSWORD",
)

# This fixture's own wiring.
FIXTURE_PREFIXES = ("FINTWIND_RC_",)


def build_whitelist():
    env = {}
    for name in ESSENTIALS + DRIVER_INJECTED:
        value = os.environ.get(name)
        if value is not None:
            env[name] = value
    for name, value in os.environ.items():
        if name.startswith(FIXTURE_PREFIXES):
            env[name] = value
    return env


def main():
    if len(sys.argv) < 2:
        print("launch_isolated.py: the provider script is required", file=sys.stderr)
        return 2
    script = sys.argv[1]
    env = build_whitelist()
    # Windows execve starts a replacement process and exits this process;
    # that invalidates the driver's private-server liveness handle while
    # the provider is still running. Keep this exact child alive and wait.
    if os.name == "nt":
        return subprocess.run([sys.executable, script] + sys.argv[2:], env=env, check=False).returncode
    os.execve(sys.executable, [sys.executable, script] + sys.argv[2:], env)


if __name__ == "__main__":
    sys.exit(main())
