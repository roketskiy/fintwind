"""Headless math E2E, isolated data, whitelisted environment and owned Windows job.

No installed application, daemon, provider or dev watcher is started or stopped.
Usage: python scripts/math-layout-e2e.py [--target-dir PATH] [--timeout 1200]
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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-dir", default=str(ROOT / "target"))
    parser.add_argument("--timeout", type=int, default=1200)
    parser.add_argument("--worker", nargs=2, metavar=("LOG", "TARGET"), help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        cargo = shutil.which("cargo")
        if not cargo:
            raise RuntimeError("cargo must be on PATH")
        log, target = args.worker
        command = [cargo, "test", "--locked", "--offline", "-p", "fintwind", "--lib",
                   "--target-dir", target, "md::render::math_text::", "--", "--nocapture",
                   "--test-threads=1"]
        with open(log, "w", encoding="utf-8") as output:
            result = subprocess.run(command, cwd=ROOT, stdout=output, stderr=subprocess.STDOUT,
                                    timeout=args.timeout)
        raise SystemExit(result.returncode)

    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("owned_process", ROOT / "scripts/goal-opencode-e2e.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    data = ROOT / "target/math-layout-e2e" / uuid.uuid4().hex
    data.mkdir(parents=True)
    allowed = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "CARGO_HOME", "RUSTUP_HOME",
               "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS", "INCLUDE", "LIB", "LIBPATH"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    # Resolve toolchain homes before isolating the runtime's home directories.
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP",
                "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        path = data / key.lower()
        path.mkdir()
        env[key] = str(path)
    proof, log = data / "proof.json", data / "test.log"
    env["FINTWIND_MATH_E2E_PROOF"] = str(proof)
    child = helper.OwnedProcess(sys.executable,
        [str(Path(__file__).resolve()), "--timeout", str(args.timeout), "--worker", str(log), args.target_dir],
        ROOT, env)
    try:
        deadline = time.monotonic() + args.timeout
        while child.alive():
            if time.monotonic() > deadline:
                raise TimeoutError(f"Math E2E exceeded {args.timeout}s: {log}")
            time.sleep(0.1)
        status = ctypes.c_ulong()
        child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        child.close()
    if status.value != 0 or not proof.exists():
        raise RuntimeError(f"Math E2E failed (exit {status.value}): {log}\n" +
                           log.read_text(encoding="utf-8", errors="replace")[-16000:])
    assert json.loads(proof.read_text(encoding="utf-8"))["passed"]
    print(f"[OK] Math layout E2E: {proof}")


if __name__ == "__main__":
    main()
