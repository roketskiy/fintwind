"""One bounded, isolated credential check; no production process or data access."""
import importlib.util
import json
import os
import socket
from pathlib import Path
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import uuid

ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("owned_process", ROOT / "scripts/goal-opencode-e2e.py")
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        valid = self.headers.get("Authorization") in ("Bearer fixture-old", "Bearer fixture-new")
        body = json.dumps({"data": [{"id": "fixture-model"}]} if valid else {"error": "unauthorized"}).encode()
        self.send_response(200 if valid else 401)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: provider-credentials-check.py <example.exe> <daemon.exe> <opencode.exe>")
    binaries = [str(Path(path).resolve(strict=True)) for path in sys.argv[1:]]
    output = ROOT / "target/provider-credentials-check" / str(uuid.uuid4())
    output.mkdir(parents=True)
    allowed = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    for key, name in {"HOME": "home", "USERPROFILE": "home", "APPDATA": "roaming", "LOCALAPPDATA": "local",
                      "TEMP": "tmp", "TMP": "tmp", "OPENCODE_CONFIG_DIR": "config", "XDG_CONFIG_HOME": "xdg/config",
                      "XDG_DATA_HOME": "xdg/data", "XDG_CACHE_HOME": "xdg/cache", "XDG_STATE_HOME": "xdg/state"}.items():
        directory = output / name
        directory.mkdir(parents=True, exist_ok=True)
        env[key] = str(directory)
    env.update({"OPENCODE_DB": str(output / "opencode.db"), "FIXTURE_KEY": "fixture-env",
                "OPENCODE_DISABLE_AUTOUPDATE": "1", "OPENCODE_DISABLE_PROJECT_CONFIG": "1",
                "OPENCODE_CONFIG_PROJECT_DISABLE": "1", "HTTP_PROXY": "http://127.0.0.1:7897",
                "HTTPS_PROXY": "http://127.0.0.1:7897", "NO_PROXY": "localhost,127.0.0.1,::1"})
    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=fixture.serve_forever, daemon=True).start()
    child = daemon = None
    try:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{listener.getsockname()[1]}"
        token = uuid.uuid4().hex
        daemon_env = dict(env, FINTWIND_DAEMON_TOKEN=token)
        daemon = runner.OwnedProcess(binaries[1], ["--bind", address, "--parent-pid", str(os.getpid()),
            "--data-dir", str(output / "daemon")], output, daemon_env)
        env["FINTWIND_TEST_DAEMON_TOKEN"] = token
        child = runner.OwnedProcess(binaries[0], [str(output), address, binaries[2],
            f"http://127.0.0.1:{fixture.server_port}/v1"], output, env)
        deadline = time.monotonic() + 150
        while child.alive() and time.monotonic() < deadline:
            time.sleep(0.1)
        if child.alive():
            raise RuntimeError("Credential check exceeded 150 seconds")
        report = json.loads((output / "result.json").read_text(encoding="utf-8"))
        print(json.dumps(report, indent=2))
        print(f"Artifact: {output / 'result.json'}")
        if not report.get("passed"):
            raise SystemExit(1)
    finally:
        if child:
            child.close()
        if daemon:
            daemon.close()
        fixture.shutdown()
        fixture.server_close()


if __name__ == "__main__":
    main()
