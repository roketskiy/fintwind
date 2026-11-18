"""Run on Linux with an explicit OpenCode V2 binary; uses only loopback fixtures."""
import base64
import contextlib
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
from urllib.error import URLError
from urllib.parse import parse_qs, quote, urlencode, urlsplit
from urllib.request import ProxyHandler, Request, build_opener


TOKEN = "fixture-access-token"
RESULT = "mcp-persistence-fixture-result"
NAME = "persist_fixture"
BROWSER = build_opener(ProxyHandler({}))


class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, value=None, status=200, **headers):
        self.send_response(status)
        for key, value_ in headers.items():
            self.send_header(key.replace("_", "-"), value_)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        if value is not None:
            self.wfile.write(json.dumps(value).encode())

    def do_GET(self):
        path = urlsplit(self.path)
        base = self.server.base
        if path.path == "/.well-known/oauth-protected-resource/mcp":
            self.reply({"resource": base + "/mcp", "authorization_servers": [base]})
        elif path.path == "/.well-known/oauth-authorization-server":
            self.reply({"issuer": base, "authorization_endpoint": getattr(self.server, "authorize_base", base) + "/authorize",
                        "token_endpoint": base + "/token", "response_types_supported": ["code"],
                        "grant_types_supported": ["authorization_code"],
                        "code_challenge_methods_supported": ["S256"],
                        "token_endpoint_auth_methods_supported": ["none"]})
        elif path.path == "/authorize":
            query = parse_qs(path.query)
            redirect = query["redirect_uri"][0]
            assert redirect == self.server.redirect
            assert query["client_id"] == ["fixture-client"]
            assert query["code_challenge_method"] == ["S256"]
            self.server.challenge = query["code_challenge"][0]
            self.server.authorizations += 1
            self.reply(status=302, Location=redirect + "?" + urlencode({"code": "fixture-code", "state": query["state"][0]}))
        else:
            self.reply(status=405)

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path == "/token":
            form = parse_qs(raw.decode())
            verifier = form.get("code_verifier", [""])[0]
            challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip("=")
            if (form.get("code") != ["fixture-code"] or challenge != self.server.challenge
                    or self.server.exchanges):
                self.reply({"error": "invalid_grant"}, 400)
                return
            self.server.exchanges += 1
            self.reply({"access_token": TOKEN, "token_type": "Bearer", "expires_in": 3600})
        elif self.path == "/mcp":
            if self.headers.get("Authorization") != "Bearer " + TOKEN:
                self.reply(status=401, WWW_Authenticate='Bearer resource_metadata="' + self.server.base + '/.well-known/oauth-protected-resource/mcp"')
                return
            body = json.loads(raw)
            method = body["method"]
            if "id" not in body:
                self.reply(status=202)
                return
            if method == "initialize":
                result = {"protocolVersion": body["params"]["protocolVersion"],
                          "capabilities": {"tools": {}}, "serverInfo": {"name": NAME, "version": "1"}}
            elif method == "tools/list":
                result = {"tools": [{"name": "ping", "description": "Return the fixture result",
                                     "inputSchema": {"type": "object", "properties": {}}}]}
            elif method == "tools/call" and body["params"]["name"] == "ping":
                self.server.calls += 1
                result = {"content": [{"type": "text", "text": RESULT}]}
            else:
                self.reply({"jsonrpc": "2.0", "id": body["id"], "error": {"code": -32601, "message": "Unknown method"}})
                return
            self.reply({"jsonrpc": "2.0", "id": body["id"], "result": result})
        elif self.path == "/v1/chat/completions":
            self.model(json.loads(raw))
        else:
            self.reply(status=404)

    def model(self, body):
        marker = any(m.get("role") == "tool" and RESULT in json.dumps(m) for m in body["messages"])
        tool = next((t["function"]["name"] for t in body.get("tools", [])
                     if t.get("function", {}).get("name", "").endswith("_ping")), None)
        if marker:
            delta, reason = {"role": "assistant", "content": RESULT}, "stop"
        elif tool:
            delta, reason = {"role": "assistant", "tool_calls": [{"index": 0, "id": "call_fixture",
                "type": "function", "function": {"name": tool, "arguments": "{}"}}]}, "tool_calls"
        else:
            delta, reason = {"role": "assistant", "content": "No fixture tool available"}, "stop"
        common = {"id": "chatcmpl-fixture", "created": 0, "model": "fixture-model"}
        if not body.get("stream"):
            self.reply({**common, "object": "chat.completion", "choices": [{"index": 0,
                        "message": delta, "finish_reason": reason}]})
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for chunk, finish in [(delta, None), ({}, reason)]:
            self.wfile.write(("data: " + json.dumps({**common, "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": chunk, "finish_reason": finish}]}) + "\n\n").encode())
        self.wfile.write(b"data: [DONE]\n\n")


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def poll(label, read, predicate, seconds=22):
    deadline = time.monotonic() + seconds
    while True:
        value = read()
        if predicate(value):
            return value
        if time.monotonic() >= deadline:
            raise RuntimeError(label + " did not reach the expected state")
        time.sleep(0.2)


@contextlib.contextmanager
def opencode(binary, root, env):
    port = free_port()
    basic = base64.b64encode(("opencode:" + secrets.token_urlsafe()).encode()).decode()
    password = base64.b64decode(basic).decode().split(":", 1)[1]
    child = subprocess.Popen([binary, "serve", "--hostname", "127.0.0.1", "--port", str(port)],
                             cwd=root, env={**env, "OPENCODE_SERVER_PASSWORD": password},
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    def api(path, body=None):
        path += ("&" if "?" in path else "?") + urlencode({"location[directory]": str(root)})
        request = Request(f"http://127.0.0.1:{port}" + path,
                          None if body is None else json.dumps(body).encode(),
                          {"Authorization": "Basic " + basic, "Content-Type": "application/json"})
        with BROWSER.open(request, timeout=5) as response:
            data = response.read()
            return json.loads(data) if data else None
    def ready():
        if child.poll() is not None:
            raise RuntimeError("isolated OpenCode exited during startup")
        try:
            return api("/api/info")
        except URLError:
            return None
    try:
        poll("OpenCode startup", ready, bool)
        yield api, child.pid
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)


@contextlib.contextmanager
def isolated_fixture(binary, callback_port=None):
    binary = str(Path(binary).resolve(strict=True))
    os.umask(0o077)
    with tempfile.TemporaryDirectory(prefix="mcp-oauth-data-", dir=os.getcwd()) as tmp, \
            ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as service:
        root = Path(tmp)
        service.base = f"http://127.0.0.1:{service.server_port}"
        service.redirect = f"http://127.0.0.1:{callback_port or free_port()}/callback"
        service.authorizations = service.exchanges = service.calls = 0
        service.challenge = None
        threading.Thread(target=service.serve_forever, daemon=True).start()
        env = {"HOME": tmp, "PATH": "/usr/bin:/bin", "TMPDIR": tmp, "SHELL": "/bin/sh",
               "FIXTURE_MODEL_KEY": "fixture-model-only", "OPENCODE_DB": str(root / "fixture.db"),
               **{"XDG_" + key.upper() + "_HOME": str(root / key) for key in ("config", "cache", "data", "state")}}
        (root / "opencode.json").write_text(json.dumps({"update": "disable", "model": "fixture/fixture-model",
            "small_model": "fixture/fixture-model", "permissions": [{"action": "*", "resource": "*", "effect": "allow"}],
            "providers": {"fixture": {"name": "Fixture", "env": ["FIXTURE_MODEL_KEY"],
                "package": "@opencode/ai/providers/openai-compatible", "settings": {"baseURL": service.base + "/v1"},
                "models": {"fixture-model": {"name": "Fixture", "limit": {"context": 65536, "output": 2048}}}}},
            "mcp": {"servers": {NAME: {"type": "remote", "url": service.base + "/mcp", "codemode": False,
                "oauth": {"client_id": "fixture-client", "redirect_uri": service.redirect}}}}}), encoding="utf-8")

        yield binary, root, env, service


def main(binary):
    with isolated_fixture(binary) as (binary, root, env, service):
        def data(reply):
            return reply.get("data", reply) if isinstance(reply, dict) else reply

        def mcp(api):
            return next((row for row in data(api("/api/mcp")) if row["name"] == NAME), {})

        def status(row):
            return row.get("status", {}).get("status")

        with opencode(binary, root, env) as (api, first_pid):
            before = poll("unauthenticated MCP", lambda: mcp(api), lambda row: status(row) == "needs_auth")
            integration = before["integrationID"]
            row = poll("MCP integration", lambda: next((row for row in data(api("/api/integration"))
                        if row["id"] == integration and row.get("methods")), None), bool)
            method = next(method for method in row["methods"] if method["type"] == "oauth")
            path = "/api/integration/" + quote(integration, safe="") + "/connect/oauth"
            attempt = data(api(path, {"methodID": method["id"]}))
            assert attempt["url"].startswith(service.base + "/authorize?")
            with BROWSER.open(attempt["url"], timeout=12) as response:
                assert response.status == 200
            poll("OAuth completion", lambda: data(api(path + "/" + quote(attempt["attemptID"], safe=""))),
                 lambda row: row["status"] == "complete")
            assert service.authorizations == service.exchanges == 1
            print("PASS real OpenCode exchanged one OAuth code", flush=True)

        with opencode(binary, root, env) as (api, second_pid):
            assert first_pid != second_pid
            poll("MCP connection after restart", lambda: mcp(api), lambda row: status(row) == "connected")
            session = data(api("/api/session", {"title": "MCP OAuth persistence fixture"}))["id"]
            path = "/api/session/" + quote(session, safe="")
            api(path + "/prompt", {"text": "Call the fixture ping tool."})
            poll("authenticated tools/call after restart", lambda: service.calls, lambda calls: calls == 1)
            poll("tool result in OpenCode transcript", lambda: json.dumps(api(path + "/message")), lambda text: RESULT in text)
            assert service.authorizations == service.exchanges == 1
            print(json.dumps({"passed": True, "initialMcpStatus": "needs_auth", "restarted": True,
                              "mcpStatusAfterRestart": "connected", "authorizationRequests": service.authorizations,
                              "tokenExchanges": service.exchanges, "authenticatedToolCalls": service.calls,
                              "toolResult": RESULT}), flush=True)


if __name__ == "__main__":
    main(sys.argv[1])
