"""Remote half of the Windows provider OAuth test; stdout contains only fixture metadata and counts."""
import base64
from contextlib import ExitStack, contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import secrets
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener


RESULT = "Saved OAuth credential reached the fixture model."
HTTP = build_opener(ProxyHandler({}))


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, data, content_type="application/json"):
        body = json.dumps(data).encode() if content_type == "application/json" else data.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path != "/approve/" + self.server.browser_code or not self.server.starts:
            return self.reply(404, {})
        self.reply(200, '<title>SSH OAuth Fixture</title><form method="post"><button>Approve fixture</button></form>', "text/html")

    def do_POST(self):
        server = self.server
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.path == "/device" and not server.starts:
            server.starts += 1
            return self.reply(200, {"device": server.device_code, "url": server.base + "/approve/" + server.browser_code})
        if self.path == "/approve/" + server.browser_code and server.starts and not server.approvals:
            server.approvals += 1
            return self.reply(200, "<title>Fixture authorized</title>Fixture authorized", "text/html")
        if self.path == "/token" and body == server.device_code.encode() and not server.exchanges:
            if not server.approvals:
                return self.reply(202, {})
            server.exchanges += 1
            return self.reply(200, {"access": server.access, "refresh": secrets.token_urlsafe()})
        if self.path == "/v1/chat/completions":
            if self.headers.get("Authorization") != "Bearer " + server.access or not server.exchanges:
                server.rejected += 1
                return self.reply(401, {"error": "Fixture requires the OAuth token"})
            server.calls += 1
            common = {"id": "fixture", "model": "fixture-model", "created": 1}
            chunks = [{**common, "object": "chat.completion.chunk", "choices": [{"index": 0,
                       "delta": delta, "finish_reason": finish}]}
                      for delta, finish in [({"role": "assistant", "content": RESULT}, None), ({}, "stop")]]
            return self.reply(200, "".join("data: " + json.dumps(row) + "\n\n" for row in chunks)
                              + "data: [DONE]\n\n", "text/event-stream")
        self.reply(400, {})


@contextmanager
def daemon_fixture(root, daemon, binary, endpoint):
    root.mkdir()
    for name in ("config", "cache", "data", "state", "daemon"):
        (root / name).mkdir()
    config = root / "config/opencode"
    config.mkdir()
    plugin = config / "provider-oauth-fixture"
    plugin.mkdir()
    (plugin / "index.ts").write_bytes(Path("provider-oauth-fixture.ts").read_bytes())
    (config / "opencode.json").write_text(json.dumps({
        "update": "disable", "model": "fixture/fixture-model", "small_model": "fixture/fixture-model",
        "plugins": [{"package": str(plugin), "options": {"endpoint": endpoint}}],
        "providers": {"openai": {"settings": {"baseURL": endpoint + "/v1"}},
                      "fixture": {"name": "OAuth Fixture Model", "activation": "enabled",
            "package": "@opencode/ai/providers/openai-compatible", "settings": {"baseURL": endpoint + "/v1"},
            "models": {"fixture-model": {"name": "Fixture Model", "limit": {"context": 65536, "output": 2048}}}}},
    }))
    token = secrets.token_urlsafe()
    token_file = root / "daemon-token"
    token_file.write_text(token)
    server = root / "opencode-endpoint"
    launcher = root / "opencode-fixture-wrapper"
    launcher.write_text(f'''#!/bin/sh
if [ "$1" = serve ]; then
    previous=
    for arg in "$@"; do
        if [ "$previous" = --port ]; then port="$arg"; fi
        previous="$arg"
    done
    printf '%s\\n%s\\n' "$port" "$OPENCODE_SERVER_PASSWORD" > {shlex.quote(str(server))}
fi
exec {shlex.quote(str(binary))} "$@"
''')
    launcher.chmod(0o700)
    (root / "daemon/daemon.json").write_text(json.dumps({"opencode_binary_path": str(launcher),
                                                        "general_workspace_path": str(root)}))
    env = {"HOME": str(root), "PATH": "/usr/bin:/bin", "TMPDIR": str(root), "FINTWIND_DEBUG_DATA_DIR": str(root / "daemon"),
           "OPENCODE_DB": str(root / "fixture.db"), "FINTWIND_DAEMON_TOKEN": token,
           **{"XDG_" + name.upper() + "_HOME": str(root / name) for name in ("config", "cache", "data", "state")}}
    with subprocess.Popen([daemon, "--bind", "127.0.0.1:0", "--data-dir", str(root / "daemon")],
                          cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          text=True, start_new_session=True) as child:
        try:
            port = int(json.loads(child.stdout.readline())["address"].rsplit(":", 1)[1])
            yield {"port": port, "token_file": str(token_file), "workspace": str(root)}, server
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=5)


def api(profile, path, body=None):
    ready, server = profile
    port, password = server.read_text().splitlines()
    auth = base64.b64encode(("opencode:" + password).encode()).decode()
    url = f"http://127.0.0.1:{port}{path}?" + urlencode({"location[directory]": ready["workspace"]})
    request = Request(url, None if body is None else json.dumps(body).encode(),
                      {"Authorization": "Basic " + auth, "Content-Type": "application/json"})
    with HTTP.open(request, timeout=10) as response:
        payload = json.load(response)
        return payload.get("data", payload) if isinstance(payload, dict) else payload


def snapshot(profiles):
    states = {}
    for name, profile in profiles.items():
        deadline = time.monotonic() + 12
        while True:
            rows = api(profile, "/api/integration")
            row = next((row for row in rows if row.get("id") == "openai"), {})
            methods = [method.get("id") for method in row.get("methods", [])]
            if "fintwind-fixture" in methods:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{name}: fixture OAuth method missing; integration count={len(rows)}, "
                                   f"OpenAI methods={methods}")
            time.sleep(.25)
        states[name] = len(row.get("connections", []))
    return states


def main(daemon, binary, port):
    daemon, binary = (Path(path).resolve(strict=True) for path in (daemon, binary))
    os.umask(0o077)
    with tempfile.TemporaryDirectory(prefix="provider-oauth-", dir=os.getcwd()) as tmp, \
            HTTPServer(("127.0.0.1", int(port)), Provider) as service, ExitStack() as stack:
        service.base = f"http://127.0.0.1:{service.server_port}"
        service.browser_code, service.device_code, service.access = (secrets.token_urlsafe() for _ in range(3))
        service.starts = service.approvals = service.exchanges = service.calls = service.rejected = 0
        threading.Thread(target=service.serve_forever, daemon=True).start()
        profiles = {name: stack.enter_context(daemon_fixture(Path(tmp) / name, daemon, binary, service.base))
                    for name in ("target", "peer")}
        print(json.dumps({"profiles": {name: value[0] for name, value in profiles.items()}}), flush=True)
        before = after = None
        for command in sys.stdin:
            command = command.strip()
            if command == "baseline":
                before = snapshot(profiles)
                assert before == {"target": 0, "peer": 0} and service.starts == 0
                print(json.dumps({"passed": True, "before": before}), flush=True)
            elif command == "authorized":
                deadline = time.monotonic() + 15
                while True:
                    after = snapshot(profiles)
                    if after == {"target": 1, "peer": 0}:
                        break
                    if time.monotonic() >= deadline or after["peer"]:
                        break
                    time.sleep(.25)
                evidence = {"after": after, "browserApprovals": service.approvals, "tokenExchanges": service.exchanges}
                valid = (before == {"target": 0, "peer": 0} and after == {"target": 1, "peer": 0}
                         and service.starts == service.approvals == service.exchanges == 1)
                print(json.dumps({**evidence, "passed": valid}), flush=True)
                if not valid:
                    return
            elif command == "verify":
                assert after == {"target": 1, "peer": 0}
                reconnected = snapshot(profiles)
                assert reconnected == after and service.calls == 0
                target = profiles["target"]
                session = api(target, "/api/session", {"title": "SSH OAuth reconnect fixture"})["id"]
                path = "/api/session/" + session
                api(target, path + "/prompt", {"text": "Reply briefly without calling tools."})
                deadline = time.monotonic() + 20
                while RESULT not in json.dumps(api(target, path + "/message")):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"Fixture response absent: accepted={service.calls}, rejected={service.rejected}")
                    time.sleep(.2)
                assert service.calls > 0 and service.exchanges == 1 and snapshot(profiles) == after
                print(json.dumps({"passed": True, "before": before, "after": after, "afterReconnect": reconnected,
                                  "browserApprovals": service.approvals, "tokenExchanges": service.exchanges,
                                  "authenticatedModelRequests": service.calls, "modelResult": RESULT}), flush=True)
            else:
                raise ValueError("Unknown fixture command")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(124))
    main(*sys.argv[1:])
