"""Run the headless input-to-message reference E2E with isolated data and an owned job.

Usage: python scripts/mention-rendering-e2e.py [--target-dir PATH]
No real app, daemon, provider, screenshot or dev watcher is started.
"""

import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent


def test_command(target_dir):
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("cargo must be on PATH")
    return [cargo, "test", "--locked", "-p", "fintwind", "--lib", "--target-dir", target_dir,
            "mentions::e2e::reference_draft_send_edit_copy_e2e", "--", "--exact", "--nocapture"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-dir", default=str(ROOT / "target"))
    parser.add_argument("--worker", nargs=2, metavar=("LOG", "TARGET"), help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        log, target = args.worker
        with open(log, "w", encoding="utf-8") as output:
            result = subprocess.run(test_command(target), cwd=ROOT, stdout=output, stderr=subprocess.STDOUT)
        raise SystemExit(result.returncode)
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("owned_process", ROOT / "scripts/goal-opencode-e2e.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    root = ROOT / "target/mention-e2e"
    data = root / ("runtime-" + uuid.uuid4().hex)
    data.mkdir(parents=True)
    allowed = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "CARGO_HOME", "RUSTUP_HOME",
               "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS", "INCLUDE", "LIB", "LIBPATH"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP",
                "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        path = data / key.lower()
        path.mkdir()
        env[key] = str(path)
    before = set(root.glob("*/proof.json"))
    log = data / "test.log"
    child = helper.OwnedProcess(sys.executable, [str(Path(__file__).resolve()), "--worker",
        str(log), args.target_dir], ROOT, env)
    try:
        deadline = time.monotonic() + 900
        while child.alive():
            if time.monotonic() > deadline:
                raise TimeoutError("Mention E2E exceeded 15 minutes")
            time.sleep(0.1)
        status = ctypes.c_ulong()
        child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        child.close()
    artifacts = set(root.glob("*/proof.json")) - before
    if status.value != 0 or len(artifacts) != 1:
        raise RuntimeError(f"Mention E2E failed (exit {status.value}): {log}\n" + log.read_text(encoding="utf-8", errors="replace")[-12000:])
    proof = artifacts.pop()
    assert json.loads(proof.read_text(encoding="utf-8"))["passed"]
    print(f"[OK] Mention rendering E2E: {proof}")


if __name__ == "__main__":
    main()
