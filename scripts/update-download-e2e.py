"""Run the update download black box in an isolated, owned Windows Job Object.

Build first: cargo build --locked -p fintwind-core --example update_download_blackbox
Then run: python scripts/update-download-e2e.py
"""

import importlib.util
import json
import os
from pathlib import Path
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent


def main():
    spec = importlib.util.spec_from_file_location("owned_goal_process", ROOT / "scripts/goal-opencode-e2e.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    output = ROOT / "target/update-download-e2e" / str(uuid.uuid4())
    output.mkdir(parents=True)
    environment = {key: value for key, value in os.environ.items()
                   if key.upper() in {"SYSTEMROOT", "WINDIR", "SYSTEMDRIVE", "COMSPEC", "PATHEXT"}}
    for key, relative in {"HOME": "home", "USERPROFILE": "home", "APPDATA": "roaming",
                          "LOCALAPPDATA": "local", "TEMP": "temp", "TMP": "temp",
                          "XDG_CONFIG_HOME": "config", "XDG_DATA_HOME": "data",
                          "XDG_CACHE_HOME": "cache"}.items():
        path = output / relative
        path.mkdir(exist_ok=True)
        environment[key] = str(path)
    environment["PATH"] = str(Path(environment["SYSTEMROOT"]) / "System32")
    environment["NO_PROXY"] = "*"
    executable = ROOT / "target/debug/examples/update_download_blackbox.exe"
    if not executable.is_file():
        raise RuntimeError("Build update_download_blackbox before running this script.")
    child = module.OwnedProcess(str(executable), [str(output)], output, environment)
    try:
        deadline = time.monotonic() + 45
        while child.alive():
            if time.monotonic() > deadline:
                raise TimeoutError("Owned download black box exceeded 45 seconds.")
            time.sleep(0.1)
        report = json.loads((output / "report.json").read_text(encoding="utf-8"))
        if not report.get("passed"):
            raise RuntimeError(report)
        print(f"{len(report['checks'])} download checks passed; {output / 'report.json'}")
    finally:
        child.close()


if __name__ == "__main__":
    main()
