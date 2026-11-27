"""Run the watchdog daemon E2E with an isolated environment and owned job.

Build first with: mbx test --locked -p fintwind-daemon --test opencode_recovery --no-run
"""
import importlib.util
import os
from pathlib import Path
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("owned_job", ROOT / "scripts/goal-opencode-e2e.py")
support = importlib.util.module_from_spec(spec)
spec.loader.exec_module(support)


def main():
    output = ROOT / "temp" / "watchdog-e2e" / uuid.uuid4().hex
    output.mkdir(parents=True)
    allowed = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "OPENCODE_CONFIG_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        directory = output / key
        directory.mkdir()
        env[key] = str(directory)
    env.update(OPENCODE_DISABLE_AUTOUPDATE="1", OPENCODE_DISABLE_FILEWATCHER="1", OPENCODE_DISABLE_PROJECT_CONFIG="1", NO_PROXY="127.0.0.1,localhost,::1")
    binaries = sorted((ROOT / "target/debug/deps").glob("opencode_recovery-*.exe"), key=lambda path: path.stat().st_mtime)
    if not binaries:
        raise RuntimeError("Build the opencode_recovery E2E binary first")
    log = output / "test.log"
    command = output / "run.cmd"
    command.write_text(f'@echo off\n"{binaries[-1]}" watchdog_daemon_e2e --ignored --nocapture > "{log}" 2>&1\n', encoding="utf-8")
    child = support.OwnedProcess(env["COMSPEC"], ["/d", "/c", str(command)], ROOT, env)
    try:
        deadline = time.monotonic() + 90
        while child.alive() and time.monotonic() < deadline:
            time.sleep(0.25)
        if child.alive():
            raise RuntimeError(f"Watchdog E2E timed out; log: {log}")
    finally:
        child.close()
    text = log.read_text(encoding="utf-8", errors="replace")
    print(text)
    print(f"Runner artifact: {output}")
    if "test result: ok. 1 passed" not in text:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
