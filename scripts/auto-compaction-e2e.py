"""Exercise automatic compaction through the actual headless Fintwind event pump.

Run with python scripts/auto-compaction-e2e.py. All children belong to one owned
Windows job. The daemon and desktop use isolated data and an environment allowlist.
"""

import argparse
import ctypes
import contextlib
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


def worker(data, target):
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("cargo must be on PATH")
    with (data / "test.log").open("w", encoding="utf-8") as log:
        subprocess.run([cargo, "build", "--locked", "-p", "fintwind-daemon",
                        "--target-dir", str(target)], cwd=ROOT, stdout=log,
                       stderr=subprocess.STDOUT, check=True)
        daemon_env = dict(os.environ)
        token = uuid.uuid4().hex
        daemon_env["FINTWIND_DAEMON_TOKEN"] = token
        daemon = subprocess.Popen([str(target / "debug/fintwind-daemon.exe"),
                                   "--bind", "127.0.0.1:0", "--data-dir", str(data / "daemon"),
                                   "--parent-pid", str(os.getpid())],
                                  cwd=data, env=daemon_env, stdout=subprocess.PIPE,
                                  stderr=log, text=True, encoding="utf-8")
        ready = json.loads(daemon.stdout.readline())
        if daemon.poll() is not None:
            raise RuntimeError("The owned daemon exited after announcing readiness")
        test_env = dict(os.environ)
        test_env["FINTWIND_DAEMON_ADDRESS"] = ready["address"]
        test_env["FINTWIND_DAEMON_TOKEN"] = token
        test_env["FINTWIND_E2E_PROOF"] = str(data / "proof.json")
        result = subprocess.run([cargo, "test", "--locked", "-p", "fintwind", "--lib",
                                 "--target-dir", str(target),
                                 "app::auto_compaction_e2e::automatic_compaction_event_pump_e2e",
                                 "--", "--ignored", "--exact", "--nocapture"], cwd=ROOT, env=test_env,
                                stdout=log, stderr=subprocess.STDOUT)
    raise SystemExit(result.returncode)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-dir", default=str(ROOT / "target"))
    parser.add_argument("--worker", nargs=2, metavar=("DATA", "TARGET"), help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        with (Path(args.worker[0]) / "worker.log").open("w", encoding="utf-8") as log:
            with contextlib.redirect_stderr(log):
                try:
                    worker(Path(args.worker[0]), Path(args.worker[1]))
                except Exception:
                    import traceback
                    traceback.print_exc()
                    raise SystemExit(1)
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("owned_process", ROOT / "scripts/goal-opencode-e2e.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    data = ROOT / "target/auto-compaction-e2e" / uuid.uuid4().hex
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
    child = helper.OwnedProcess(sys.executable, [str(Path(__file__).resolve()), "--worker",
        str(data), str(Path(args.target_dir).resolve())], data, env)
    try:
        deadline = time.monotonic() + 1200
        while child.alive():
            if time.monotonic() > deadline:
                raise TimeoutError(f"Compaction E2E exceeded 20 minutes. See {data / 'test.log'}")
            time.sleep(0.1)
        status = ctypes.c_ulong()
        child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        child.close()
    proof = data / "proof.json"
    if status.value != 0 or not proof.is_file():
        log = data / "test.log"
        details = "\n".join(path.read_text(encoding="utf-8", errors="replace")[-5000:]
                            for path in (log, data / "worker.log") if path.is_file())
        raise RuntimeError(f"Compaction E2E failed (exit {status.value}). See {log}\n{details}")
    assert json.loads(proof.read_text(encoding="utf-8"))["passed"]
    print(f"[OK] Automatic compaction E2E. {proof}")


if __name__ == "__main__":
    main()
