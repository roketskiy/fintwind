"""Run the Windows probe against an isolated Linux daemon over SSH."""

import argparse
import contextlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import secrets
import shlex
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

MODEL = "fixture/fixture-model"
PROMPT = "Reply with a short greeting in plain text. Do not use tools."
PARTS = ("Hello from the ", "SSH stream fixture.")
LIVE_MODEL = "alibaba-token-plan-cn/qwen4-max-preview"


def prepare_live_provider(env):
    source_data = Path(os.getenv("XDG_DATA_HOME") or Path.home() / ".local/share")
    source = source_data / "opencode/opencode.db"
    target = Path(env["XDG_DATA_HOME"]) / "opencode/opencode.db"
    target.parent.mkdir(parents=True)
    with sqlite3.connect(source.as_uri() + "?mode=ro", uri=True, timeout=5) as existing:
        schema = existing.execute("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type != 'table'").fetchall()
        migrations = existing.execute("SELECT * FROM migration").fetchall()
        rows = existing.execute("SELECT * FROM credential WHERE integration_id = ? AND active = 1",
                                ("alibaba-token-plan-cn",)).fetchall()
    if len(rows) != 1:
        raise RuntimeError("The active Alibaba subscription is missing")
    with sqlite3.connect(target, timeout=5) as isolated:
        for (statement,) in schema:
            isolated.execute(statement)
        placeholders = ",".join("?" for _ in migrations[0])
        isolated.executemany(f"INSERT INTO migration VALUES ({placeholders})", migrations)
        isolated.executemany("INSERT INTO credential VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)", rows)


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/mcp":
            self.mcp(body)
            return
        if self.headers.get("Authorization") != "Bearer ssh-probe-fixture-key":
            self.send_error(401)
            return
        primary = body.get("stream") and PROMPT in json.dumps(body.get("messages", []))
        common = {"id": "chatcmpl-ssh", "created": 0, "model": "fixture-model"}
        if not body.get("stream"):
            data = json.dumps({**common, "object": "chat.completion", "choices": [{
                "index": 0, "message": {"role": "assistant", "content": "SSH fixture"},
                "finish_reason": "stop"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        chunks = PARTS if primary else ("SSH fixture",)
        for index, content in enumerate(chunks):
            delta = {"content": content}
            if index == 0:
                delta["role"] = "assistant"
            self.chunk(common, delta, None)
            if index + 1 < len(chunks):
                time.sleep(getattr(self.server, "stream_delay", 25 if os.getenv("FINTWIND_NATIVE_DESKTOP") else 3))
        self.chunk(common, {}, "stop")
        self.wfile.write(b"data: [DONE]\n\n")

    def do_GET(self):
        self.send_error(405)

    def mcp(self, body):
        if self.headers.get("Authorization") != "Bearer ssh-probe-mcp-key":
            self.send_error(401)
            return
        if "id" not in body:
            self.send_response(202)
            self.end_headers()
            return
        if body.get("method") == "initialize":
            result = {"protocolVersion": body["params"]["protocolVersion"],
                      "capabilities": {"tools": {}},
                      "serverInfo": {"name": "ssh-fixture", "version": "1.0.0"}}
        elif body.get("method") == "tools/list":
            result = {"tools": []}
        else:
            self.send_error(400)
            return
        data = json.dumps({"jsonrpc": "2.0", "id": body["id"], "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def chunk(self, common, delta, reason):
        data = {**common, "object": "chat.completion.chunk", "choices": [{
            "index": 0, "delta": delta, "finish_reason": reason}]}
        self.wfile.write(("data: " + json.dumps(data) + "\n\n").encode())
        self.wfile.flush()


def remote(daemon, opencode):
    token = sys.stdin.readline().strip()
    if not token:
        raise RuntimeError("isolated daemon token missing")
    daemon = str(Path(daemon).resolve(strict=True))
    opencode = str(Path(opencode).resolve(strict=True)) if opencode else None
    live = os.getenv("FINTWIND_SSH_LIVE_MODEL") == "1"
    os.umask(0o077)
    with tempfile.TemporaryDirectory(prefix="fintwind-ssh-e2e-", dir="/tmp") as directory:
        root = Path(directory)
        token_file = root / "daemon-token"
        token_file.write_text(token, encoding="utf-8")
        cwd = root / "workspace"
        cwd.mkdir()
        home = root / "home"
        home.mkdir()
        if os.getenv("FINTWIND_PROBE_SKILLS_REMOTE") == "1":
            skill = home / ".agents/skills/ssh-routing-fixture"
            skill.mkdir(parents=True)
            (skill / "SKILL.md").write_text("---\nname: ssh-routing-fixture\n---\nRemote fixture\n", encoding="utf-8")
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "SHELL": "/bin/sh",
               "TMPDIR": str(root), "GIT_CONFIG_NOSYSTEM": "1",
               **{"XDG_" + key.upper() + "_HOME": str(root / key)
                  for key in ("config", "cache", "data", "state")}}
        git = ["git", "-c", "user.name=SSH Probe", "-c", "user.email=probe@example.invalid"]
        for args in (("init", "-q"), ("symbolic-ref", "HEAD", "refs/heads/ssh-probe"),
                     ("commit", "-q", "--allow-empty", "-m", "Isolated fixture")):
            subprocess.run(git + list(args), cwd=cwd, env=env, check=True, timeout=5)

        with ThreadingHTTPServer(("127.0.0.1", 0), Provider) as provider:
            if live:
                prepare_live_provider(env)
                config = root / "config/opencode/opencode.json"
                config.parent.mkdir(parents=True, exist_ok=True)
                config.write_text(json.dumps({"model": LIVE_MODEL, "small_model": LIVE_MODEL,
                                              "update": "disable"}), encoding="utf-8")
            elif opencode:
                threading.Thread(target=provider.serve_forever, daemon=True).start()
                config = root / "config/opencode/opencode.json"
                config.parent.mkdir(parents=True)
                config.write_text(json.dumps({"model": MODEL, "small_model": MODEL,
                    "update": "disable", "mcp": {"servers": {"ssh-fixture": {
                        "type": "remote", "url": f"http://127.0.0.1:{provider.server_port}/mcp",
                        "oauth": False, "headers": {"Authorization": "Bearer {env:FIXTURE_MCP_KEY}"}}}},
                    "providers": {"fixture": {
                        "name": "SSH fixture", "env": ["FIXTURE_API_KEY"],
                        "package": "@opencode/ai/providers/openai-compatible",
                        "settings": {"baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                        "models": {"fixture-model": {"name": "SSH fixture",
                            "limit": {"context": 128000, "output": 8192}}}}}}), encoding="utf-8")
                if os.getenv("FINTWIND_PROVIDER_ISOLATION"):
                    document = json.loads(config.read_text(encoding="utf-8"))
                    document["providers"]["openai"] = {
                        "settings": {"baseURL": f"http://127.0.0.1:{provider.server_port}/v1"}}
                    config.write_text(json.dumps(document), encoding="utf-8")
            if os.getenv("FINTWIND_NATIVE_DESKTOP"):
                env["FIXTURE_API_KEY"] = "ssh-probe-fixture-key"
            if opencode:
                data = root / "daemon"
                data.mkdir(exist_ok=True)
                (data / "daemon.json").write_text(json.dumps({"opencode_binary_path": opencode}), encoding="utf-8")
            with subprocess.Popen([daemon, "--bind", "127.0.0.1:0", "--data-dir",
                                   str(root / "daemon"), "--parent-pid", str(os.getpid())],
                                  env={**env, "FINTWIND_DAEMON_TOKEN": token,
                                       "FIXTURE_MCP_KEY": "ssh-probe-mcp-key"}, cwd=cwd,
                                  stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                  text=True, encoding="utf-8", start_new_session=True) as child:
                try:
                    ready = json.loads(child.stdout.readline())
                    response = {"port": int(ready["address"].rsplit(":", 1)[1]),
                                      "workspace": str(cwd), "opencode": opencode,
                                      "token_file": str(token_file)}
                    with provider_peer(daemon, opencode, token) as peer:
                        if peer:
                            response["peer"] = peer
                        print(json.dumps(response), flush=True)
                        sys.stdin.read(1)
                finally:
                    # Keep the session leader unreaped until its group is stopped.
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    child.wait(timeout=5)


@contextlib.contextmanager
def provider_peer(daemon, opencode, token):
    if os.getenv("FINTWIND_PROVIDER_ISOLATION") != "1":
        yield None
        return
    env = dict(os.environ)
    env["FINTWIND_PROVIDER_ISOLATION"] = "peer"
    with subprocess.Popen([sys.executable, "-u", __file__, "--remote", daemon, opencode],
                          env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          text=True, encoding="utf-8") as child:
        try:
            child.stdin.write(token + "\n")
            child.stdin.flush()
            response = queue.Queue()
            threading.Thread(target=lambda: response.put(child.stdout.readline()), daemon=True).start()
            yield json.loads(response.get(timeout=15))
        finally:
            child.stdin.close()
            child.wait(timeout=15)


def settings_probe(binary, args, env, skills=False, timeout=100, local_model=False):
    import importlib.util
    import socket

    spec = importlib.util.spec_from_file_location(
        "fintwind_fixture", Path(__file__).resolve().parents[2] / "scripts/goal-opencode-e2e.py")
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    with tempfile.TemporaryDirectory(prefix="ssh-settings-") as root:
        if skills:
            skill = Path(root) / ".agents/skills/ssh-routing-fixture"
            skill.mkdir(parents=True)
            (skill / "SKILL.md").write_text("---\nname: ssh-routing-fixture\n---\nWindows fixture\n", encoding="utf-8")
            env = {**env, "FINTWIND_PROBE_SKILLS_LOCAL": root}
        token = secrets.token_urlsafe(32)
        daemon_env = {k: v for k, v in os.environ.items()
                      if k.upper() in {"SYSTEMROOT", "WINDIR", "PATH", "COMSPEC", "PATHEXT"}}
        daemon_env.update({k: root for k in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP")})
        daemon_env["FINTWIND_DAEMON_TOKEN"] = token
        with contextlib.ExitStack() as services, socket.socket() as listener:
            if local_model:
                opencode = __import__("shutil").which("opencode")
                if not opencode:
                    raise RuntimeError("Windows OpenCode is required")
                provider = services.enter_context(ThreadingHTTPServer(("127.0.0.1", 0), Provider))
                provider.stream_delay = 25
                threading.Thread(target=provider.serve_forever, daemon=True).start()
                services.callback(provider.shutdown)
                daemon_env.update(OPENCODE_CONFIG_CONTENT=json.dumps({"model": MODEL,
                    "providers": {"fixture": {"name": "Windows fixture", "env": ["FIXTURE_API_KEY"],
                        "package": "@opencode/ai/providers/openai-compatible",
                        "settings": {"baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                        "models": {"fixture-model": {"name": "Windows fixture",
                            "limit": {"context": 128000, "output": 8192}}}}}}),
                    FIXTURE_API_KEY="ssh-probe-fixture-key", OPENCODE_DISABLE_AUTOUPDATE="1")
                Path(root, "daemon.json").write_text(json.dumps({"opencode_binary_path": opencode}), encoding="utf-8")
                workspace = Path(root, "workspace")
                workspace.mkdir()
                env = {**env, "FINTWIND_E2E_LOCAL_WORKSPACE": str(workspace),
                       "FINTWIND_E2E_LOCAL_OPENCODE": opencode}
            listener.bind(("127.0.0.1", 0))
            address = listener.getsockname()
            listener.close()
            return run_local_daemon(fixture, binary, args, env, root, daemon_env, address, token, timeout)


def run_local_daemon(fixture, binary, args, env, root, daemon_env, address, token, timeout):
    import socket
    endpoint = f"127.0.0.1:{address[1]}"
    daemon = fixture.OwnedProcess(str(binary.resolve(strict=True)),
                                  ["--bind", endpoint, "--data-dir", root], root, daemon_env)
    try:
        deadline = time.monotonic() + 15
        while daemon.alive():
            try:
                with socket.create_connection(address, timeout=0.5):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise RuntimeError("Windows daemon fixture startup timed out")
                time.sleep(0.05)
        else:
            raise RuntimeError("Windows daemon fixture exited during startup")
        return subprocess.run(args, env={**env, "FINTWIND_PROBE_LOCAL_SETTINGS_ADDRESS": endpoint,
                              "FINTWIND_PROBE_LOCAL_SETTINGS_TOKEN": token}, timeout=timeout)
    finally:
        daemon.close()


def native_desktop(ns, ready, fingerprint):
    import importlib.util
    import uuid
    spec = importlib.util.spec_from_file_location(
        "fintwind_fixture", Path(__file__).resolve().parents[2] / "scripts/goal-opencode-e2e.py")
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    root = ns.report.resolve()
    root.mkdir(parents=True, exist_ok=False)
    home = root / "home"
    home.mkdir()
    config = root / "opencode-config"
    config.mkdir()
    profiles = ready.get("profiles", {"target": ready})
    rows = [{"id": str(uuid.uuid4()),
             "name": f"OAuth {name.title()}" if ns.native_provider_oauth else "SSH Native Fixture",
             "host": ns.host, "port": ns.port, "user": ns.user, "key": str(ns.key.resolve()),
             "fingerprint": fingerprint, "daemon_port": endpoint["port"], "token_file": endpoint["token_file"]}
            for name, endpoint in profiles.items()]
    (root / "ssh-profiles.json").write_text(json.dumps(rows), encoding="utf-8")
    allowed = {"SYSTEMROOT", "WINDIR", "PATH", "PATHEXT", "COMSPEC"}
    env = {k: v for k, v in os.environ.items() if k.upper() in allowed}
    env.update({k: str(home) for k in ("USERPROFILE", "HOME", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP")})
    env.update(FINTWIND_DEBUG_DATA_DIR=str(root), OPENCODE_CONFIG_DIR=str(config))
    app = fixture.OwnedProcess(str(ns.native_app.resolve(strict=True)), [], home, env)
    try:
        import ctypes
        from ctypes import wintypes
        app.kernel.GetProcessId.argtypes = [wintypes.HANDLE]
        app.kernel.GetProcessId.restype = wintypes.DWORD
        pid = app.kernel.GetProcessId(app.process)
        if not pid:
            raise ctypes.WinError(ctypes.get_last_error())
        (root / "fixture.json").write_text(json.dumps({"workspace": profiles["target"]["workspace"],
            "profile": rows[0]["id"], "desktopPid": pid, "callbackPort": ready.get("callback_port"),
            "browserPort": ready.get("browser_port"), "browserSpki": ready.get("browser_spki")}, indent=2), encoding="utf-8")
        print(f"Native desktop fixture ready: {root}", flush=True)
        deadline = time.monotonic() + (360 if ns.native_provider_oauth else 240 if ns.native_mcp_oauth else 145)
        while (app.alive() and time.monotonic() < deadline
               and not (root / "stop").exists() and not (root / "abort").exists()):
            if ns.native_provider_oauth:
                for stage in ("baseline", "authorized", "verify"):
                    output = root / f"provider-{stage}.json"
                    if (root / stage).exists() and not output.exists():
                        result = ns.provider_stage(stage)
                        output.write_text(json.dumps(result, indent=2), encoding="utf-8")
                        if not result.get("passed"):
                            raise RuntimeError(f"Provider OAuth {stage} failed; see {output.name}")
            time.sleep(0.2)
    finally:
        app.close()
    return (root / "stop").exists()


def local():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("port", type=int)
    parser.add_argument("user")
    parser.add_argument("key", type=Path)
    parser.add_argument("known_hosts", type=Path)
    parser.add_argument("probe", type=Path)
    parser.add_argument("report", type=Path)
    parser.add_argument("remote_dir", help="existing isolated remote build directory")
    parser.add_argument("--opencode", help="remote OpenCode V2 binary relative to remote_dir")
    parser.add_argument("--desktop-test", type=Path, help="run the desktop Rust test binary instead of the probe")
    parser.add_argument("--live-model", action="store_true", help="send one real Alibaba Qwen4 Max task from an isolated desktop and daemon")
    parser.add_argument("--quit-during-turn", action="store_true", help="quit the desktop during streaming and verify remote completion")
    parser.add_argument("--native-app", type=Path, help="open a debug desktop executable with isolated data; report is a new artifact directory")
    parser.add_argument("--native-mcp-oauth", action="store_true", help="verify MCP after the isolated native browser sign-in")
    parser.add_argument("--native-provider-oauth", action="store_true", help="run the dual-daemon provider OAuth fixture")
    parser.add_argument("--settings-local-daemon", type=Path, help="verify settings against a separate local daemon")
    parser.add_argument("--skills-local-daemon", type=Path, help="verify skill isolation against a separate local daemon")
    parser.add_argument("--provider-isolation", action="store_true")
    ns = parser.parse_args()
    if ns.native_provider_oauth and (not ns.native_app or ns.native_mcp_oauth or ns.provider_isolation):
        parser.error("--native-provider-oauth requires --native-app and is a separate test mode")
    if ns.quit_during_turn and not ns.desktop_test:
        parser.error("--quit-during-turn requires --desktop-test")
    ssh = ["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
           "-o", f"UserKnownHostsFile={ns.known_hosts.as_posix()}", "-o", "ConnectTimeout=8",
           "-i", str(ns.key), "-p", str(ns.port), f"{ns.user}@{ns.host}"]
    command = ["timeout", "-k", "10s", "200s", "python3", "-u", "daemon-e2e.py",
               "--remote", "target/debug/fintwind-daemon", ns.opencode or ""]
    if ns.live_model:
        if not ns.desktop_test or not ns.opencode:
            parser.error("--live-model requires --desktop-test and --opencode")
        command[3] = "390s" if ns.quit_during_turn else "335s"
        command = ["env", "FINTWIND_SSH_LIVE_MODEL=1", *command]
    elif ns.quit_during_turn:
        command[3] = "250s"
    if ns.native_app:
        if not ns.opencode:
            parser.error("--native-app requires --opencode")
        command = ["env", "FINTWIND_NATIVE_DESKTOP=1", *command]
    if ns.native_mcp_oauth or ns.native_provider_oauth:
        import socket
        if not ns.native_app:
            parser.error("--native-mcp-oauth requires --native-app")
        with socket.socket() as oauth, socket.socket() as callback:
            oauth.bind(("127.0.0.1", 0))
            callback.bind(("127.0.0.1", 0))
            port = oauth.getsockname()[1]
            callback_port = callback.getsockname()[1]
        ssh[1:1] = ["-o", "ExitOnForwardFailure=yes", "-L", f"127.0.0.1:{port}:127.0.0.1:{port}"]
        command = ["timeout", "-k", "10s", "300s", "python3", "-Bu", "mcp-oauth-desktop-e2e.py",
                   "target/debug/fintwind-daemon", ns.opencode, str(callback_port), str(port)]
        if ns.native_provider_oauth:
            command = ["timeout", "-k", "10s", "400s", "python3", "-Bu", "provider-oauth-desktop-e2e.py",
                       "target/debug/fintwind-daemon", ns.opencode, str(port)]
    if ns.skills_local_daemon:
        command = ["env", "FINTWIND_PROBE_SKILLS_REMOTE=1", *command]
    if ns.provider_isolation:
        if not ns.opencode:
            parser.error("--provider-isolation requires --opencode")
        command = ["env", "FINTWIND_PROVIDER_ISOLATION=1", *command]
    token = secrets.token_urlsafe(32)
    with subprocess.Popen(ssh + ["cd " + shlex.quote(ns.remote_dir) + " && exec "
                                 + shlex.join(command)], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, text=True, encoding="utf-8") as ssh_process:
        try:
            def receive(timeout):
                lines = queue.Queue()
                threading.Thread(target=lambda: lines.put(ssh_process.stdout.readline()), daemon=True).start()
                line = lines.get(timeout=timeout)
                if not line:
                    raise RuntimeError("Remote fixture closed without a report")
                return json.loads(line)

            def provider_stage(stage):
                ssh_process.stdin.write(stage + "\n")
                ssh_process.stdin.flush()
                result = receive(40)
                print(json.dumps(result), flush=True)
                return result

            if not ns.native_provider_oauth:
                ssh_process.stdin.write(token + "\n")
                ssh_process.stdin.flush()
            else:
                ns.provider_stage = provider_stage
            ready = receive(40 if ns.live_model else 25)
            if ns.native_provider_oauth:
                ready["browser_port"] = port
            if ns.desktop_test or ns.native_app:
                if not (ns.native_mcp_oauth or ns.native_provider_oauth) and not ready["opencode"]:
                    parser.error("--desktop-test requires --opencode")
                known = subprocess.check_output(["ssh-keygen", "-F", f"[{ns.host}]:{ns.port}",
                                                 "-f", str(ns.known_hosts)], text=True, timeout=5)
                keys = [line.split()[1:3] for line in known.splitlines() if not line.startswith("#")]
                ed25519 = next(key for key in keys if key[0] == "ssh-ed25519")
                fingerprint = subprocess.run(["ssh-keygen", "-lf", "-"], input=" ".join(ed25519) + "\n",
                                             capture_output=True, text=True, timeout=5, check=True).stdout.split()[1]
                if ns.native_app:
                    finished = native_desktop(ns, ready, fingerprint)
                    if ns.native_provider_oauth and (not finished or not (ns.report / "provider-verify.json").is_file()):
                        raise RuntimeError("Provider OAuth desktop verification did not finish")
                    if ns.native_mcp_oauth:
                        if not finished:
                            raise RuntimeError("desktop sign-in was not confirmed before the fixture closed")
                        ssh_process.stdin.write("verify\n")
                        ssh_process.stdin.flush()
                        result = queue.Queue()
                        threading.Thread(target=lambda: result.put(ssh_process.stdout.readline()), daemon=True).start()
                        line = result.get(timeout=40)
                        if not line:
                            raise RuntimeError("remote OAuth fixture exited without a restart report")
                        proof = json.loads(line)
                        if not proof.get("passed"):
                            raise RuntimeError("OpenCode restart verification failed")
                        (ns.report / "oauth-proof.json").write_text(json.dumps(proof, indent=2), encoding="utf-8")
                        print(json.dumps(proof), flush=True)
                    return
                proof = ns.report.resolve()
                proof.parent.mkdir(parents=True, exist_ok=True)
                with tempfile.TemporaryDirectory(prefix="desktop-", dir=proof.parent) as data:
                    root = Path(data)
                    home = root / "home"
                    home.mkdir()
                    fixture = {"profile": {"id": str(__import__("uuid").uuid4()), "name": "SSH fixture",
                                            "host": ns.host, "port": ns.port, "user": ns.user,
                                            "key": str(ns.key.resolve()), "fingerprint": fingerprint,
                                            "daemon_port": ready["port"], "token_file": ready["token_file"]},
                               "workspace": ready["workspace"], "binary": ready["opencode"],
                               "state_dir": str(root / "state"), "proof": str(proof)}
                    if ns.provider_isolation:
                        fixture["peer_profile"] = {**fixture["profile"],
                            "id": str(__import__("uuid").uuid4()), "name": "SSH peer",
                            "daemon_port": ready["peer"]["port"],
                            "token_file": ready["peer"]["token_file"]}
                        fixture["peer_workspace"] = ready["peer"]["workspace"]
                    if ns.live_model:
                        fixture["live_model"] = LIVE_MODEL
                    allowed = {"SYSTEMROOT", "WINDIR", "PATH", "PATHEXT", "COMSPEC"}
                    env = {k: v for k, v in os.environ.items() if k.upper() in allowed}
                    env.update({k: str(home) for k in ("USERPROFILE", "HOME", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP")})
                    env["FINTWIND_DEBUG_DATA_DIR"] = str(root / "desktop-data")
                    env["FINTWIND_SSH_DESKTOP_FIXTURE"] = json.dumps(fixture)
                    if ns.quit_during_turn:
                        env["FINTWIND_SSH_QUIT_DURING_TURN"] = "1"
                    desktop_command = [str(ns.desktop_test.resolve()),
                                       "app::ssh_page::e2e::desktop_ssh_project_streams", "--exact", "--ignored", "--nocapture"]
                    if ns.settings_local_daemon and ns.live_model and not ns.quit_during_turn:
                        result = settings_probe(ns.settings_local_daemon, desktop_command, env, timeout=270, local_model=True)
                    else:
                        result = subprocess.run(desktop_command,
                                            env=env, cwd=home,
                                            timeout=270 if ns.live_model else 150 if ns.quit_during_turn else 100)
                    if result.returncode == 0 and ns.quit_during_turn:
                        result = subprocess.run(desktop_command,
                                                env={**env, "FINTWIND_SSH_RESTART_PHASE": "1"},
                                                cwd=home, timeout=55)
                if result.returncode == 0 and not proof.is_file():
                    raise RuntimeError("desktop test did not write a proof report")
                if result.returncode == 0 and ns.provider_isolation and not ns.quit_during_turn:
                    if json.loads(proof.read_text(encoding="utf-8")).get("switchedBetweenRemoteDaemons") is not True:
                        raise RuntimeError("desktop test did not verify switching between remote daemons")
                if result.returncode == 0 and ns.quit_during_turn:
                    restart_proof = json.loads(proof.read_text(encoding="utf-8"))
                    if not all(restart_proof.get(field) is True for field in (
                        "remoteTurnActiveBeforeQuit",
                        "remoteTurnCompletedAfterQuit",
                        "updateBlockedWhileBusy",
                        "updateAllowedWhenIdle",
                        "restoredInNewProcess",
                        "restoredCompletedTurnWithoutDuplicateMessage",
                    )):
                        raise RuntimeError("desktop restart report is missing required evidence")
                sys.exit(result.returncode)
            args = [str(ns.probe), ns.host, str(ns.port), ns.user, str(ns.key), str(ns.known_hosts),
                    str(ns.report), str(ready["port"]), ready["workspace"]]
            if ready["opencode"]:
                args += [ready["opencode"], MODEL, "".join(PARTS)]
            env = {**os.environ, "FINTWIND_PROBE_REMOTE_TOKEN_FILE": ready["token_file"]}
            if ns.provider_isolation:
                env.update(FINTWIND_PROBE_PROVIDER_PEER_PORT=str(ready["peer"]["port"]),
                           FINTWIND_PROBE_PROVIDER_PEER_TOKEN_FILE=ready["peer"]["token_file"])
            result = (settings_probe(ns.skills_local_daemon, args, env, skills=True) if ns.skills_local_daemon
                      else settings_probe(ns.settings_local_daemon, args, env) if ns.settings_local_daemon
                      else subprocess.run(args, env=env, timeout=155))
            sys.exit(result.returncode)
        finally:
            ssh_process.stdin.close()
            try:
                ssh_process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                ssh_process.kill()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--remote"]:
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(124))
        remote(*sys.argv[2:])
    else:
        local()
