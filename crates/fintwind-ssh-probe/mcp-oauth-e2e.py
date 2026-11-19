"""Verify the browser callback tunnel over SSH without a real account."""

import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import queue
import shlex
import socket
import subprocess
import sys
import threading
from urllib.request import ProxyHandler, build_opener
from urllib.parse import urlencode


def fixture():
    if len(sys.argv) != 4 or sys.argv[1:3] != ["mcp", "auth"] or not sys.argv[3].startswith("fixture-"):
        raise SystemExit("unexpected OpenCode arguments")
    port = int(sys.argv[3][len("fixture-"):])

    class Callback(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            self.server.received = self.path == "/callback?code=fixture&state=fixture"
            if self.server.received:
                Path("credential-fixture").write_text("saved by remote OAuth fixture", encoding="utf-8")
            self.send_response(200 if self.server.received else 400)
            self.end_headers()
            self.wfile.write(b"remote callback accepted")

    with HTTPServer(("127.0.0.1", port), Callback) as server:
        server.timeout = 20
        server.received = False
        query = urlencode({"redirect_uri": f"http://127.0.0.1:{port}/callback"})
        print(f"https://example.invalid/authorize?{query}", flush=True)
        server.handle_request()
        return 0 if server.received else 1


def local():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("host", "port", "user", "key", "known_hosts", "probe"):
        parser.add_argument(name)
    args = parser.parse_args()
    ssh = ["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
           "-o", f"UserKnownHostsFile={args.known_hosts}", "-o", "ConnectTimeout=8",
           "-i", args.key, "-p", args.port, f"{args.user}@{args.host}"]
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    remote_dir = subprocess.check_output(
        ssh + ["umask 077; mktemp -d \"$HOME/fintwind-mcp-oauth-e2e.XXXXXXXX\""],
        text=True, timeout=15,
    ).strip()
    remote_file = remote_dir + "/opencode fixture"
    try:
        source = Path(__file__).read_text(encoding="utf-8").split("\ndef local():", 1)[0]
        subprocess.run(ssh + [f"umask 077; cat > {shlex.quote(remote_file)} && chmod u+x {shlex.quote(remote_file)}"],
                       input=("#!/usr/bin/python3\n" + source + "\nraise SystemExit(fixture())\n").encode(),
                       check=True, timeout=15)
        command = [args.probe, "--mcp-oauth", args.host, args.port, args.user, args.key,
                   args.known_hosts, remote_file, remote_dir, f"fixture-{port}"]
        with subprocess.Popen(command, stdout=subprocess.PIPE, text=True) as child:
            try:
                output = queue.Queue()
                threading.Thread(target=lambda: output.put(child.stdout.readline()), daemon=True).start()
                query = urlencode({"redirect_uri": f"http://127.0.0.1:{port}/callback"})
                if output.get(timeout=20).strip() != f"https://example.invalid/authorize?{query}":
                    raise RuntimeError("the desktop-side client did not receive the authorization URL")
                browser = build_opener(ProxyHandler({}))
                with browser.open(f"http://127.0.0.1:{port}/callback?code=fixture&state=fixture", timeout=8) as response:
                    if response.read() != b"remote callback accepted":
                        raise RuntimeError("browser did not receive the remote callback response")
                if child.wait(timeout=8) != 0:
                    raise RuntimeError("MCP OAuth probe exited unsuccessfully")
            finally:
                if child.poll() is None:
                    child.kill()
        with socket.socket() as client:
            if client.connect_ex(("127.0.0.1", port)) == 0:
                raise RuntimeError("callback listener survived the probe")
        subprocess.run(ssh + [f"test -s {shlex.quote(remote_dir + '/credential-fixture')}"], check=True, timeout=15)
        print("PASS: authorization URL returned; simulated browser callback reached remote loopback; fixture credential saved remotely; listener closed")
    finally:
        subprocess.run(ssh + [f"rm -f {shlex.quote(remote_file)} {shlex.quote(remote_dir + '/credential-fixture')} && rmdir {shlex.quote(remote_dir)}"],
                       check=True, timeout=15)


if __name__ == "__main__":
    local()
