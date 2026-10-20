"""Run scheduled-task dispatch checks against a real daemon and real OpenCode.

The run owns one Windows Job Object. Inside it a worker process starts a
loopback OpenAI-compatible fixture, a real `opencode` discovered on PATH, and an
isolated daemon. The fixture is the only provider: no user configuration,
credential, database, or provider endpoint is inherited. Every process launch,
HTTP read, and wait has a deadline.

The Rust example receives --data-root, --proof, --daemon-bin, --opencode-bin,
--fixture-url, and --fixture-log. It drives the initial daemon supplied through
FINTWIND_DAEMON_ADDRESS/TOKEN, then restarts its own daemon on the same data
directory. The fixture log is the evidence that a prompt reached a model.
"""

import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import threading
import time
import traceback
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parent.parent
EXAMPLE = "scheduled_tasks_e2e"
SOURCE = ROOT / "crates/fintwind-client/examples" / (EXAMPLE + ".rs")
DEFAULT_TARGET = ROOT / "target/scheduled-tasks-e2e-target"
FIXTURE_MODEL = "fixture/fixture-model"
ALLOWED = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "CARGO_HOME",
           "RUSTUP_HOME", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS",
           "INCLUDE", "LIB", "LIBPATH", "PROGRAMFILES", "PROGRAMDATA",
           "SYSTEMDRIVE", "USERNAME", "PUBLIC", "ALLUSERSPROFILE"}
REDIRECTED = ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP",
              "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME")
CONFIGURED = ("OPENCODE_CONFIG_DIR",)


def isolated_environment(run):
    env = {key: value for key, value in os.environ.items() if key.upper() in ALLOWED}
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    for key in REDIRECTED:
        directory = run / key.lower()
        directory.mkdir(parents=True, exist_ok=True)
        env[key] = str(directory)
    for key in CONFIGURED:
        directory = run / "config"
        directory.mkdir(parents=True, exist_ok=True)
        env[key] = str(directory)
    return env


def provider_config(fixture_origin):
    return {
        "$schema": "https://opencode.ai/config.json",
        "model": FIXTURE_MODEL,
        "update": "disable",
        "providers": {"fixture": {
            "name": "Scheduled task fixture",
            "env": ["FIXTURE_API_KEY"],
            "package": "@opencode/ai/providers/openai-compatible",
            "settings": {"baseURL": fixture_origin + "/v1", "apiKey": "fixture-only"},
            "models": {"fixture-model": {
                "name": "Fixture",
                "limit": {"context": 128000, "output": 8192},
            }},
        }},
    }


def write_provider_config(run, fixture_origin):
    data = json.dumps(provider_config(fixture_origin), indent=2)
    paths = (
        run / "xdg/config/opencode/opencode.json",
        run / "home/.config/opencode/opencode.json",
        run / "config/opencode.json",
    )
    for path in paths:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data, encoding="utf-8")


def last_user_text(payload):
    for message in reversed(payload.get("messages") or []):
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            return "\n".join(
                part.get("text", "") for part in content
                if isinstance(part, dict) and part.get("type") in ("text", "input_text"))
        return json.dumps(content)
    return ""


class Fixture:
    """A deterministic OpenAI-compatible endpoint that can hold main requests."""

    MAX_BODY = 4 * 1024 * 1024
    READ_TIMEOUT = 20
    HOLD_TIMEOUT = 150

    def __init__(self, log_path):
        self.log_path = log_path
        self.lock = threading.Lock()
        self.released = threading.Event()
        self.released.set()
        self.markers = ()
        self.sequence = 0
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self.handler())
        self.server.daemon_threads = True
        self.thread = threading.Thread(
            target=self.server.serve_forever, name="scheduled-fixture", daemon=True)

    @property
    def origin(self):
        return "http://127.0.0.1:%d" % self.server.server_port

    def handler(self):
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def setup(self):
                super().setup()
                self.connection.settimeout(fixture.READ_TIMEOUT)

            def do_GET(self):
                if self.path.startswith("/__fixture/state"):
                    fixture.control_state(self)
                    return
                fixture.send_json(self, {"object": "list", "data": [
                    {"id": "fixture-model", "object": "model"}]})

            def do_POST(self):
                if self.path.startswith("/__fixture/"):
                    fixture.control(self)
                    return
                fixture.provider(self)

        return Handler

    def start(self):
        self.thread.start()

    def hold(self, markers):
        with self.lock:
            self.markers = tuple(markers)
        self.released.clear()

    def release(self):
        self.released.set()

    def stop(self):
        self.server.shutdown()
        self.server.server_close()

    def next_sequence(self):
        with self.lock:
            self.sequence += 1
            return self.sequence

    def log(self, entry):
        entry["seq"] = self.next_sequence()
        entry["at"] = time.time()
        line = json.dumps(entry, ensure_ascii=False) + "\n"
        with self.lock:
            with self.log_path.open("a", encoding="utf-8") as log:
                log.write(line)
                log.flush()

    def read_body(self, handler):
        length = int(handler.headers.get("Content-Length") or 0)
        if length <= 0 or length > self.MAX_BODY:
            raise ValueError("invalid request body length")
        return json.loads(handler.rfile.read(length))

    def send_json(self, handler, value, status=200):
        data = json.dumps(value).encode()
        handler.close_connection = True
        handler.send_response(status)
        handler.send_header("Content-Type", "application/json")
        handler.send_header("Content-Length", str(len(data)))
        handler.end_headers()
        handler.wfile.write(data)

    def control(self, handler):
        try:
            command = handler.path.rsplit("/", 1)[-1]
            if command == "release":
                self.released.set()
                body = {"released": True}
            elif command == "hold":
                payload = self.read_body(handler)
                markers = payload.get("markers")
                if not isinstance(markers, list) or not markers or not all(
                        isinstance(marker, str) and marker for marker in markers):
                    raise ValueError("hold needs a non-empty marker list")
                with self.lock:
                    self.markers = tuple(markers)
                self.released.clear()
                body = {"released": False, "markers": list(self.markers)}
            else:
                raise ValueError("unknown fixture control command")
            self.send_json(handler, body)
        except (OSError, ValueError, json.JSONDecodeError) as error:
            self.send_json(handler, {"error": str(error)}, 400)

    def control_state(self, handler):
        self.send_json(handler, {"released": self.released.is_set(),
                                 "markers": list(self.markers)})

    def provider(self, handler):
        try:
            payload = self.read_body(handler)
        except (OSError, ValueError, json.JSONDecodeError) as error:
            self.send_json(handler, {"error": str(error)}, 400)
            return
        prompt = last_user_text(payload)
        tools = len(payload.get("tools") or [])
        with self.lock:
            markers = self.markers
        held = tools > 0 and any(marker in prompt for marker in markers)
        if held:
            self.log({"kind": "request", "path": handler.path,
                      "prompt": prompt, "tools": tools,
                      "stream": bool(payload.get("stream")), "held": True})
            if not self.released.wait(self.HOLD_TIMEOUT):
                self.send_json(handler, {"error": "fixture hold expired"}, 504)
                return
        else:
            self.log({"kind": "request", "path": handler.path,
                      "prompt": prompt, "tools": tools,
                      "stream": bool(payload.get("stream")), "held": False})
        reply = "fixture reply %d" % self.next_sequence()
        self.log({"kind": "response", "prompt": prompt, "reply": reply,
                  "tools": tools, "held": held})
        message = {"role": "assistant", "content": reply}
        common = {"id": "chatcmpl-fixture", "created": 0, "model": "fixture-model"}
        usage = {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
        try:
            if not payload.get("stream"):
                self.send_json(handler, {**common, "object": "chat.completion",
                                         "usage": usage, "choices": [{
                                             "index": 0, "message": message,
                                             "finish_reason": "stop"}]})
                return
            chunks = [
                {**common, "object": "chat.completion.chunk",
                 "choices": [{"index": 0, "delta": message, "finish_reason": None}]},
                {**common, "object": "chat.completion.chunk", "usage": usage,
                 "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
            data = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks)
                    + "data: [DONE]\n\n").encode()
            handler.close_connection = True
            handler.send_response(200)
            handler.send_header("Content-Type", "text/event-stream")
            handler.send_header("Content-Length", str(len(data)))
            handler.end_headers()
            handler.wfile.write(data)
        except OSError:
            pass


def read_ready(process, timeout):
    lines = queue.Queue()

    def read():
        try:
            lines.put(process.stdout.readline())
        except BaseException as error:
            lines.put(error)

    threading.Thread(target=read, daemon=True).start()
    try:
        line = lines.get(timeout=timeout)
    except queue.Empty as error:
        raise TimeoutError("Daemon readiness deadline expired") from error
    if isinstance(line, BaseException):
        raise line
    if not line:
        raise RuntimeError("Daemon closed stdout before announcing readiness")
    ready = json.loads(line)
    if not isinstance(ready, dict) or not isinstance(ready.get("address"), str):
        raise RuntimeError("Daemon returned an invalid ready line")
    if process.poll() is not None:
        raise RuntimeError("Daemon exited after announcing readiness")
    return ready


def stop_owned(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)


def base_environment(run):
    env = isolated_environment(run)
    for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "OPENCODE_CONFIG_CONTENT"):
        env.pop(key, None)
    env.update({
        "OPENCODE_DISABLE_AUTOUPDATE": "1",
        "OPENCODE_DISABLE_FILEWATCHER": "1",
        "OPENCODE_DISABLE_PROJECT_CONFIG": "1",
        "OPENCODE_CONFIG_PROJECT_DISABLE": "1",
        "OPENCODE_DB": str(run / "opencode.db"),
        "FIXTURE_API_KEY": "fixture-only",
        "NO_PROXY": "127.0.0.1,localhost,::1",
    })
    return env


def worker(run, target, timeout, daemon_bin, no_build):
    deadline = time.monotonic() + timeout

    def remaining(limit=None):
        seconds = deadline - time.monotonic()
        if seconds <= 0:
            raise TimeoutError("Scheduled-task verification deadline expired")
        return min(seconds, limit) if limit else seconds

    opencode = shutil.which("opencode")
    if not opencode:
        raise RuntimeError("opencode is not available on PATH")
    fixture = Fixture(run / "fixture.jsonl")
    fixture.start()
    env = base_environment(run)
    forbidden = {"ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY",
                 "OPENROUTER_API_KEY", "DEEPSEEK_API_KEY", "XAI_API_KEY",
                 "AZURE_OPENAI_API_KEY", "OPENCODE_CONFIG_CONTENT",
                 "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"}
    leaked = sorted(key for key in forbidden if key in env)
    if leaked:
        raise RuntimeError("provider environment leaked into the run: " + ", ".join(leaked))
    system_config = Path(os.environ.get("ProgramData", "C:/ProgramData")) / "opencode"
    if any((system_config / name).is_file()
           for name in ("opencode.json", "opencode.jsonc")):
        raise RuntimeError(
            "system OpenCode configuration cannot be isolated: " + str(system_config))
    write_provider_config(run, fixture.origin)
    daemon = Path(daemon_bin) if daemon_bin else target / "debug/fintwind-daemon.exe"
    example = target / "debug/examples" / (EXAMPLE + ".exe")
    process = None
    try:
        with (run / "worker.log").open("w", encoding="utf-8") as log:
            if not no_build:
                cargo = shutil.which("cargo")
                if not cargo:
                    raise RuntimeError("Cargo is not available on PATH")
                commands = []
                if not daemon_bin:
                    commands.append(
                        [cargo, "build", "--locked", "--offline", "-p", "fintwind-daemon",
                         "--bin", "fintwind-daemon", "--target-dir", str(target)])
                commands.append(
                    [cargo, "build", "--locked", "--offline", "-p", "fintwind-client",
                     "--example", EXAMPLE, "--target-dir", str(target)])
                for command in commands:
                    subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                                   check=True, timeout=remaining(1500))
            if not daemon.is_file() or not example.is_file():
                raise RuntimeError("Build the daemon and scheduled_tasks_e2e example first")
            data = run / "data"
            data.mkdir()
            token = uuid.uuid4().hex
            daemon_env = dict(env, FINTWIND_DAEMON_TOKEN=token)
            with (run / "daemon.log").open("w", encoding="utf-8") as daemon_log:
                process = subprocess.Popen(
                    [str(daemon), "--bind", "127.0.0.1:0", "--data-dir", str(data),
                     "--parent-pid", str(os.getpid())], cwd=run, env=daemon_env,
                    stdout=subprocess.PIPE, stderr=daemon_log, text=True, encoding="utf-8")
                ready = read_ready(process, remaining(60))
                example_env = dict(env, FINTWIND_DAEMON_ADDRESS=ready["address"],
                                   FINTWIND_DAEMON_TOKEN=token)
                subprocess.run(
                    [str(example), "--data-root", str(data),
                     "--proof", str(run / "proof.json"), "--daemon-bin", str(daemon),
                     "--opencode-bin", opencode, "--fixture-url", fixture.origin,
                     "--fixture-log", str(run / "fixture.jsonl")],
                    cwd=run, env=example_env, stdout=log, stderr=subprocess.STDOUT,
                    check=True, timeout=remaining())
    finally:
        stop_owned(process)
        fixture.stop()
    proof = run / "proof.json"
    if not proof.is_file():
        raise RuntimeError("The example wrote no proof")
    report = json.loads(proof.read_text(encoding="utf-8"))
    if report.get("passed") is not True:
        raise RuntimeError("The example did not record passed=true")



def shared_target_lock(target):
    """Return the name of a running image in the shared target, if any."""
    for name in ("debug/fintwind-daemon.exe", "debug/fintwind.exe"):
        path = target / name
        if not path.is_file():
            continue
        try:
            with path.open("r+b"):
                pass
        except OSError:
            return name
    return None


def parent(args):
    if os.name != "nt":
        raise RuntimeError("This runner requires Windows Job Objects")
    if not SOURCE.is_file():
        print("[X] Missing example: " + str(SOURCE))
        print("[X] No build or process was started")
        return 2
    target = Path(args.target_dir).resolve()
    if target == (ROOT / "target").resolve():
        locked = shared_target_lock(target)
        if locked:
            print("[X] Refusing the shared target: " + locked + " is running.")
            print("    Use the default dedicated target or --target-dir.")
            return 2
    run = ROOT / "target/scheduled-tasks-e2e" / uuid.uuid4().hex
    run.mkdir(parents=True)
    print("[i] build target: " + str(target))
    print("[i] run directory: " + str(run))
    daemon = str(Path(args.daemon_bin).resolve()) if args.daemon_bin else ""
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("owned_process", ROOT / "scripts/goal-opencode-e2e.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    deadline = time.monotonic() + args.timeout
    child = helper.OwnedProcess(
        sys.executable, [str(Path(__file__).resolve()), "--worker", str(run), str(target),
                         str(args.timeout), daemon, str(int(args.no_build))],
        run, isolated_environment(run))
    status = None
    try:
        while child.alive():
            if time.monotonic() >= deadline:
                raise TimeoutError("Scheduled-task E2E exceeded its deadline. See " + str(run))
            time.sleep(0.1)
        status = ctypes.c_ulong()
        child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        child.close()
    proof = run / "proof.json"
    if status.value != 0 or not proof.is_file():
        raise RuntimeError("Scheduled-task E2E failed. See " + str(run))
    report = json.loads(proof.read_text(encoding="utf-8"))
    if not isinstance(report, dict) or report.get("passed") is not True:
        raise RuntimeError("Proof must explicitly record passed=true: " + str(proof))
    print("[OK] Scheduled-task E2E. " + str(proof))
    return 0


def main():
    parser = argparse.ArgumentParser(description="Run isolated scheduled-task black-box verification")
    parser.add_argument("--target-dir", default=str(DEFAULT_TARGET),
                        help="build cache; the shared target is refused while its images run")
    parser.add_argument("--timeout", type=int, default=2400,
                        help="overall deadline in seconds, including a cold build")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--daemon-bin", help="use this daemon instead of building one")
    parser.add_argument("--worker", nargs=5, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        run, target, timeout, daemon, no_build = args.worker
        try:
            worker(Path(run), Path(target), float(timeout), daemon, no_build == "1")
            return 0
        except BaseException:
            with (Path(run) / "failure.log").open("w", encoding="utf-8") as log:
                traceback.print_exc(file=log)
            return 1
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    return parent(args)



if __name__ == "__main__":
    sys.exit(main())
