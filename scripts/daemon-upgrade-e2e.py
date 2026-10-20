"""Black-box daemon startup checks for fresh and retired scheduler databases.

Runs only the supplied build, with isolated profiles inside an owned Windows
Job Object. No installed process or user database is modified.
"""

import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import queue
import sqlite3
import subprocess
import sys
import threading
import time
import traceback
import uuid

ROOT = Path(__file__).resolve().parent.parent


def isolated_environment(run):
    allowed = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT",
               "PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMDATA", "SYSTEMDRIVE",
               "NUMBER_OF_PROCESSORS", "PROCESSOR_ARCHITECTURE"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP",
                "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME",
                "OPENCODE_CONFIG_DIR"):
        directory = run / key.lower()
        directory.mkdir()
        env[key] = str(directory)
    return env


def load_helper(filename):
    spec = importlib.util.spec_from_file_location(filename, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def start_daemon(binary, data, run):
    token = uuid.uuid4().hex
    with (run / (data.name + ".log")).open("a", encoding="utf-8") as log:
        process = subprocess.Popen(
            [str(binary), "--data-dir", str(data), "--bind", "127.0.0.1:0",
             "--parent-pid", str(os.getpid())],
            cwd=run, env=dict(os.environ, FINTWIND_DAEMON_TOKEN=token),
            stdout=subprocess.PIPE, stderr=log, text=True, encoding="utf-8")
        try:
            lines = queue.Queue()
            threading.Thread(target=lambda: lines.put(process.stdout.readline()),
                             daemon=True).start()
            ready = json.loads(lines.get(timeout=20))
            if ready.get("pid") != process.pid or not ready.get("address"):
                raise AssertionError("Invalid daemon readiness: " + str(ready))
            if process.poll() is not None:
                raise AssertionError("Daemon exited after readiness")
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            process.stdout.close()


def worker(binary, run):
    fresh = run / "fresh"
    start_daemon(binary, fresh, run)
    with sqlite3.connect(fresh / "app.db") as db:
        assert db.execute("SELECT count(*) FROM scheduled_tasks").fetchone() == (0,)
        assert db.execute("SELECT count(*) FROM scheduled_occurrences").fetchone() == (0,)
        assert db.execute("SELECT count(*) FROM migrations WHERE tag='0009_big_starbolt'").fetchone() == (1,)
    start_daemon(binary, fresh, run)

    legacy = run / "legacy"
    legacy.mkdir()
    # Model the previous local release, including run-history references.
    with sqlite3.connect(legacy / "app.db") as db:
        db.executescript("""
            CREATE TABLE migrations(tag TEXT PRIMARY KEY, applied_at INTEGER NOT NULL);
            INSERT INTO migrations VALUES('0009_true_lockheed', 1);
            CREATE TABLE scheduled_tasks(
                id TEXT PRIMARY KEY, title TEXT NOT NULL, prompt TEXT NOT NULL,
                schedule_json TEXT NOT NULL, enabled INTEGER NOT NULL);
            CREATE TABLE scheduled_task_runs(
                id TEXT PRIMARY KEY, task_id TEXT REFERENCES scheduled_tasks(id),
                status TEXT NOT NULL);
            INSERT INTO scheduled_tasks VALUES('legacy-task', 'Saved task', 'Saved prompt', '{}', 0);
            INSERT INTO scheduled_task_runs VALUES('legacy-run', 'legacy-task', 'completed');
        """)
        old_tasks = db.execute("SELECT * FROM scheduled_tasks").fetchall()
        old_runs = db.execute("SELECT * FROM scheduled_task_runs").fetchall()
    for _ in range(2):
        start_daemon(binary, legacy, run)
        with sqlite3.connect(legacy / "app.db") as db:
            assert db.execute("SELECT * FROM retired_scheduled_tasks").fetchall() == old_tasks
            assert db.execute("SELECT * FROM scheduled_task_runs").fetchall() == old_runs
            assert db.execute("PRAGMA foreign_key_check").fetchall() == []
            assert db.execute("SELECT count(*) FROM scheduled_tasks").fetchone() == (0,)
            assert db.execute("SELECT count(*) FROM scheduled_occurrences").fetchone() == (0,)
            assert db.execute("SELECT count(*) FROM migrations WHERE tag='0009_big_starbolt'").fetchone() == (1,)
    (run / "proof.json").write_text(json.dumps({
        "passed": True, "freshAndRestart": True, "legacyUpgradeAndRestart": True,
        "retiredTasksAndHistoryPreserved": True, "foreignKeysValid": True,
    }, indent=2), encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon-bin", default=str(ROOT / "target/release/fintwind-daemon.exe"))
    parser.add_argument("--worker", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    binary = Path(args.daemon_bin).resolve()
    if args.worker:
        try:
            worker(binary, args.worker)
        except BaseException:
            (args.worker / "failure.log").write_text(traceback.format_exc(), encoding="utf-8")
            raise
        return
    if os.name != "nt" or not binary.is_file():
        raise RuntimeError("Requires Windows and a built daemon")
    run = ROOT / "target/daemon-upgrade-e2e" / uuid.uuid4().hex
    run.mkdir(parents=True)
    # Assign the suspended worker to our Job Object before it can spawn children.
    owned = load_helper("goal-opencode-e2e.py")
    child = owned.OwnedProcess(sys.executable,
        [str(Path(__file__).resolve()), "--daemon-bin", str(binary), "--worker", str(run)],
        run, isolated_environment(run))
    try:
        deadline = time.monotonic() + 100
        while child.alive():
            if time.monotonic() >= deadline:
                raise TimeoutError("Daemon startup check exceeded deadline: " + str(run))
            time.sleep(.1)
        status = ctypes.c_ulong()
        child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
            raise ctypes.WinError(ctypes.get_last_error())
        if status.value != 0:
            raise RuntimeError("Daemon startup check failed: " + str(run))
    finally:
        child.close()
    proof = run / "proof.json"
    assert json.loads(proof.read_text(encoding="utf-8"))["passed"] is True
    print("[OK] " + str(proof))


if __name__ == "__main__":
    main()
