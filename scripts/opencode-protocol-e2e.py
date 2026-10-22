"""Exercise the real daemon/client/OpenCode completion path with a local model.

Build first: cargo test --locked -p fintwind-core --test opencode_recovery --no-run
Run: python scripts/opencode-protocol-e2e.py
"""

import importlib.util
import argparse
import json
import os
import shutil
from pathlib import Path
import threading
import time
from http.server import ThreadingHTTPServer
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("goal_e2e", ROOT / "scripts/goal-opencode-e2e.py")
support = importlib.util.module_from_spec(spec)
spec.loader.exec_module(support)
support.MODEL.reply = lambda _payload: {"role": "assistant", "content": "Protocol completion verified."}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fault-matrix", action="store_true")
    parser.add_argument("--idle-matrix", action="store_true")
    parser.add_argument("--history-matrix", action="store_true")
    parser.add_argument("--history-page", type=Path)
    parser.add_argument("--history-input")
    args = parser.parse_args()
    binary = os.environ.get("FINTWIND_REAL_OPENCODE") or shutil.which("opencode2") or shutil.which("opencode")
    if not binary:
        raise RuntimeError("Set FINTWIND_REAL_OPENCODE to an OpenCode executable.")
    output = ROOT / "temp" / "protocol-e2e" / uuid.uuid4().hex
    output.mkdir(parents=True)
    environment = {key: value for key, value in os.environ.items() if key.upper() in {
        "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}}
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "OPENCODE_CONFIG_DIR",
                "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        directory = output / key
        directory.mkdir()
        environment[key] = str(directory)
    provider = ThreadingHTTPServer(("127.0.0.1", 0), support.Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    config = {"model": "fixture/fixture-model", "small_model": "fixture/fixture-model",
              "provider": {"fixture": {"npm": "@ai-sdk/openai-compatible", "name": "Fixture",
                  "options": {"apiKey": "fixture-only", "baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                  "models": {"fixture-model": {"name": "Fixture", "limit": {"context": 128000, "output": 8192}}}}}}
    (Path(environment["OPENCODE_CONFIG_DIR"]) / "opencode.json").write_text(json.dumps(config), encoding="utf-8")
    environment.update(OPENCODE_DB=str(output / "opencode.db"), OPENCODE_DISABLE_AUTOUPDATE="1",
                       OPENCODE_DISABLE_FILEWATCHER="1", OPENCODE_DISABLE_PROJECT_CONFIG="1",
                       OPENCODE_CONFIG_PROJECT_DISABLE="1", NO_PROXY="127.0.0.1,localhost,::1",
                       FINTWIND_REAL_OPENCODE=binary)
    if args.history_page:
        if not args.history_input:
            parser.error("--history-page requires --history-input")
        environment.update(FINTWIND_RECOVERY_PAGE=str(args.history_page.resolve()),
                           FINTWIND_RECOVERY_INPUT=args.history_input)
    tests = sorted((ROOT / "target/debug/deps").glob("opencode_recovery-*.exe"), key=lambda path: path.stat().st_mtime)
    if not tests:
        raise RuntimeError("Build opencode_recovery first.")
    log = output / "test.log"
    command = output / "run.cmd"
    name = "opencode_large_history_completion" if args.history_matrix else "opencode_recovery_fast_faults" if args.fault_matrix else (
        "opencode_recovery_idle_reconciliation" if args.idle_matrix else "opencode_real_turn_completion")
    ignored = "" if args.fault_matrix else "--ignored"
    command.write_text(f'@echo off\n"{tests[-1]}" {name} {ignored} --nocapture > "{log}" 2>&1\n', encoding="utf-8")
    child = support.OwnedProcess(environment["COMSPEC"], ["/d", "/c", str(command)], ROOT, environment)
    try:
        deadline = time.monotonic() + 120
        while child.alive() and time.monotonic() < deadline:
            time.sleep(.25)
        if child.alive():
            raise RuntimeError(f"Protocol test timed out; artifact: {output}")
    finally:
        child.close()
        provider.shutdown()
        provider.server_close()
    text = log.read_text(encoding="utf-8", errors="replace")
    print(text)
    print(f"Artifacts: {output}")
    if "test result: ok. 1 passed" not in text:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
