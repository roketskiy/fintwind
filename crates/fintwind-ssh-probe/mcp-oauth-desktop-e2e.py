"""Linux side of the Windows MCP sign-in test. The Windows app owns the callback."""
import http.client
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import shlex
import signal
import ssl
import subprocess
import sys
import threading
from urllib.parse import quote, urlsplit


spec = importlib.util.spec_from_file_location(
    "fixture", Path(__file__).with_name("mcp-oauth-persistence-e2e.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def authorize_server(root, service, port):
    class BrowserEntry(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            if urlsplit(self.path).path != "/authorize":
                self.send_error(404)
                return
            connection = http.client.HTTPConnection("127.0.0.1", service.server_port, timeout=5)
            try:
                connection.request("GET", self.path)
                response = connection.getresponse()
                self.send_response(response.status)
                if response.getheader("Location"):
                    self.send_header("Location", response.getheader("Location"))
                self.end_headers()
            finally:
                connection.close()

    key, cert = root / "browser.key", root / "browser.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                    "-subj", "/CN=127.0.0.1", "-addext", "subjectAltName=IP:127.0.0.1",
                    "-keyout", key, "-out", cert], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   check=True, timeout=10)
    server = ThreadingHTTPServer(("127.0.0.1", port), BrowserEntry)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    public_key = subprocess.check_output(["openssl", "x509", "-in", cert, "-pubkey", "-noout"], timeout=5)
    spki = subprocess.run(["openssl", "pkey", "-pubin", "-outform", "DER"], input=public_key,
                          capture_output=True, check=True, timeout=5).stdout
    server.spki = base64.b64encode(hashlib.sha256(spki).digest()).decode()
    server.socket = tls.wrap_socket(server.socket, server_side=True)
    return server


def main(daemon, binary, callback_port, browser_port):
    daemon = str(Path(daemon).resolve(strict=True))
    token = sys.stdin.readline().strip()
    assert token
    with fixture.isolated_fixture(binary, int(callback_port)) as (binary, root, env, service), \
            authorize_server(root, service, int(browser_port)) as browser:
        service.authorize_base = f"https://127.0.0.1:{browser.server_port}"
        threading.Thread(target=browser.serve_forever, daemon=True).start()
        global_config = Path(env["XDG_CONFIG_HOME"]) / "opencode" / "opencode.json"
        global_config.parent.mkdir(parents=True)
        (root / "opencode.json").rename(global_config)
        data_dir = root / "daemon"
        data_dir.mkdir()
        wrapper = root / "opencode-fixture-wrapper"
        token_file = root / "daemon-token"
        token_file.write_text(token, encoding="utf-8")
        endpoint = root / "server-endpoint"
        password = root / "server-password"
        wrapper.write_text(f'''#!/bin/bash
set -eu
case "${{1-}}" in
  serve)
    exec 2>>{shlex.quote(str(root / "opencode-startup.log"))}
    previous=
    for arg in "$@"; do
      if [ "$previous" = --port ]; then
        printf 'http://127.0.0.1:%s' "$arg" > {shlex.quote(str(endpoint))}
      fi
      previous="$arg"
    done
    printf '%s' "${{OPENCODE_SERVER_PASSWORD:-${{OPENCODE_PASSWORD:-}}}}" > {shlex.quote(str(password))}
    ;;
  mcp)
    export OPENCODE_SERVER_PASSWORD="$(cat {shlex.quote(str(password))})"
    set -- --server "$(cat {shlex.quote(str(endpoint))})" "$@"
    exec > >(tee {shlex.quote(str(root / "oauth-cli-output"))}) 2> >(tee {shlex.quote(str(root / "oauth-cli-errors"))} >&2)
    ;;
esac
cd {shlex.quote(str(root))}
exec {shlex.quote(binary)} "$@"
''', encoding="utf-8")
        wrapper.chmod(0o700)
        (data_dir / "daemon.json").write_text(json.dumps({"opencode_binary_path": str(wrapper),
            "general_workspace_path": str(root)}), encoding="utf-8")
        with subprocess.Popen([daemon, "--bind", "127.0.0.1:0", "--data-dir", str(data_dir)],
                              cwd=root, env={**env, "FINTWIND_DAEMON_TOKEN": token}, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, text=True, start_new_session=True) as child:
            try:
                ready = json.loads(child.stdout.readline())
                print(json.dumps({"port": int(ready["address"].rsplit(":", 1)[1]), "workspace": str(root),
                    "token_file": str(token_file), "browser_port": browser.server_port,
                    "browser_spki": browser.spki, "callback_port": int(callback_port)}), flush=True)
                if sys.stdin.readline().strip() != "verify":
                    return
                assert service.authorizations == service.exchanges == 1, "expected one browser authorization and one OpenCode token exchange"
            finally:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)
                log = root / "opencode-startup.log"
                if service.exchanges == 0 and log.exists():
                    text = log.read_text(encoding="utf-8", errors="replace")[-2000:]
                    if password.is_file() and password.read_text():
                        text = text.replace(password.read_text(), "[redacted]")
                    print(text, file=sys.stderr, flush=True)
        with fixture.opencode(binary, root, env) as (api, pid):
            def data(reply):
                return reply.get("data", reply) if isinstance(reply, dict) else reply

            def connected():
                return any(row["name"] == fixture.NAME and row["status"]["status"] == "connected"
                           for row in data(api("/api/mcp")))

            fixture.poll("MCP connected in restarted OpenCode", connected, bool)
            session = data(api("/api/session", {"title": "Desktop OAuth persistence"}))["id"]
            path = "/api/session/" + quote(session, safe="")
            api(path + "/prompt", {"text": "Call the fixture ping tool."})
            fixture.poll("authenticated tools/call after restart", lambda: service.calls, lambda calls: calls == 1)
            fixture.poll("tool result in transcript", lambda: json.dumps(api(path + "/message")),
                         lambda transcript: fixture.RESULT in transcript)
            assert service.authorizations == service.exchanges == 1
            print(json.dumps({"passed": True, "restartedOpenCodePid": pid,
                "authorizationRequests": service.authorizations, "tokenExchanges": service.exchanges,
                "authenticatedToolCalls": service.calls, "toolResult": fixture.RESULT}), flush=True)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(124))
    main(*sys.argv[1:])
