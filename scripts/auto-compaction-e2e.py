"""Verify Fintwind compaction settings against a running OpenCode server.

Run with python scripts/auto-compaction-e2e.py. All children belong to one owned
Windows job. The daemon and desktop use isolated data and an environment allowlist.
"""

import argparse
import base64
import ctypes
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
import uuid
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent


def worker(data, target, opencode):
    cargo = shutil.which("mbx") or shutil.which("cargo")
    if not cargo:
        raise RuntimeError("mbx or cargo must be on PATH")
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
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        password = uuid.uuid4().hex
        opencode_env = dict(os.environ, OPENCODE_CONFIG=str(data / "daemon/fintwind-opencode.json"),
                            OPENCODE_DB=str(data / "opencode-fixture.db"),
                            OPENCODE_SERVER_USERNAME="opencode", OPENCODE_SERVER_PASSWORD=password,
                            OPENCODE_DISABLE_AUTOUPDATE="1")
        provider = subprocess.Popen([opencode, "serve", "--hostname", "127.0.0.1", "--port", str(port)],
                                    cwd=data, env=opencode_env, stdout=log, stderr=subprocess.STDOUT)

        def read_config(expected):
            request = Request(f"http://127.0.0.1:{port}/api/config", headers={
                "Authorization": "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()})
            deadline = time.monotonic() + 20
            seen = []
            while provider.poll() is None and time.monotonic() < deadline:
                try:
                    with urlopen(request, timeout=2) as response:
                        entries = json.load(response)
                    seen = [entry["info"]["compaction"] for entry in entries
                            if "compaction" in entry.get("info", {})]
                    if seen and seen[-1] == expected:
                        return seen[-1]
                except OSError:
                    pass
                time.sleep(0.1)
            raise AssertionError(f"OpenCode did not load {expected}; got {seen}")

        before = read_config({"auto": True, "background": True, "buffer": "15%"})
        test_env = dict(os.environ)
        test_env["FINTWIND_DAEMON_ADDRESS"] = ready["address"]
        test_env["FINTWIND_DAEMON_TOKEN"] = token
        test_env["FINTWIND_E2E_DIRECTORY"] = str(data)
        result = subprocess.run([cargo, "test", "--locked", "-p", "fintwind", "--lib",
                                 "--target-dir", str(target),
                                 "app::auto_compaction_e2e::compaction_settings_e2e",
                                 "--", "--ignored", "--exact", "--nocapture"], cwd=ROOT, env=test_env,
                                stdout=log, stderr=subprocess.STDOUT)
        if result.returncode:
            raise SystemExit(result.returncode)
        after = read_config({"auto": False, "background": False, "buffer": "28%"})
        (data / "proof.json").write_text(json.dumps({"passed": True, "before": before, "after": after},
                                                     indent=2), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-dir", default=str(ROOT / "target"))
    parser.add_argument("--opencode", default=shutil.which("opencode"))
    parser.add_argument("--worker", nargs=3, metavar=("DATA", "TARGET", "OPENCODE"), help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        with (Path(args.worker[0]) / "worker.log").open("w", encoding="utf-8") as log:
            with contextlib.redirect_stderr(log):
                try:
                    worker(Path(args.worker[0]), Path(args.worker[1]), args.worker[2])
                    return
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
               "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS", "INCLUDE", "LIB", "LIBPATH",
               "HTTP_PROXY", "HTTPS_PROXY", "MBX_CACHE_DIR"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    env.update(MBX_TARGET_VIEWS="0", MBX_GC_AUTO="0", NO_PROXY="127.0.0.1,localhost,::1")
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP",
                "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        path = data / key.lower()
        path.mkdir()
        env[key] = str(path)
    if not args.opencode:
        raise RuntimeError("OpenCode 2.3.1 or later is required")
    child = helper.OwnedProcess(sys.executable, [str(Path(__file__).resolve()), "--worker",
        str(data), str(Path(args.target_dir).resolve()), str(Path(args.opencode).resolve(strict=True))], data, env)
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
