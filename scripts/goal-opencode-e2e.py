"""Black-box Goal checks against an isolated real OpenCode and a scripted model.

Run on Windows with Python 3.12: python scripts/goal-opencode-e2e.py
The native Fintwind UI is outside this runner's scope.
"""

import base64
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener
import uuid


ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "target" / "goal-opencode-e2e" / str(uuid.uuid4())
HTTP = build_opener(ProxyHandler({}))

SETTLE_TOOL = "fintwind_goal_settle"
JUDGE_MARKER = "goal completion judge"
PROTOCOL_MARKER = "This session has an active Goal set by the user"
INACTIVE_MARKER = "Goal mode is inactive for this request."
REMINDER_MARKER = "Call it now with verdict"
CONTINUATION_MARKER = "Continue working toward this goal"
GUIDANCE_MARKER = "Manual instruction takes priority."


class OwnedProcess:
    def __init__(self, executable, arguments, cwd, environment):
        if os.name != "nt":
            raise RuntimeError("This runner requires Windows Job Objects.")
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel = kernel

        class Startup(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD), ("reserved", wintypes.LPWSTR),
                        ("desktop", wintypes.LPWSTR), ("title", wintypes.LPWSTR),
                        ("x", wintypes.DWORD), ("y", wintypes.DWORD),
                        ("width", wintypes.DWORD), ("height", wintypes.DWORD),
                        ("chars_x", wintypes.DWORD), ("chars_y", wintypes.DWORD),
                        ("fill", wintypes.DWORD), ("flags", wintypes.DWORD),
                        ("show", wintypes.WORD), ("reserved_size", wintypes.WORD),
                        ("reserved_bytes", ctypes.c_void_p),
                        ("stdin", wintypes.HANDLE), ("stdout", wintypes.HANDLE),
                        ("stderr", wintypes.HANDLE)]

        class ProcessInfo(ctypes.Structure):
            _fields_ = [("process", wintypes.HANDLE), ("thread", wintypes.HANDLE),
                        ("pid", wintypes.DWORD), ("thread_id", wintypes.DWORD)]

        class BasicLimits(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_longlong), ("job_time", ctypes.c_longlong),
                        ("flags", wintypes.DWORD), ("minimum", ctypes.c_size_t),
                        ("maximum", ctypes.c_size_t), ("active", wintypes.DWORD),
                        ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD),
                        ("scheduling", wintypes.DWORD)]

        class Limits(ctypes.Structure):
            _fields_ = [("basic", BasicLimits), ("io", ctypes.c_ulonglong * 6),
                        ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                        ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]

        kernel.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        kernel.CreateJobObjectW.restype = wintypes.HANDLE
        kernel.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int,
                                                   ctypes.c_void_p, wintypes.DWORD]
        kernel.SetInformationJobObject.restype = wintypes.BOOL
        kernel.CreateProcessW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR,
                                          ctypes.c_void_p, ctypes.c_void_p, wintypes.BOOL,
                                          wintypes.DWORD, ctypes.c_void_p, wintypes.LPCWSTR,
                                          ctypes.POINTER(Startup), ctypes.POINTER(ProcessInfo)]
        kernel.CreateProcessW.restype = wintypes.BOOL
        kernel.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        kernel.AssignProcessToJobObject.restype = wintypes.BOOL
        kernel.ResumeThread.argtypes = [wintypes.HANDLE]
        kernel.ResumeThread.restype = wintypes.DWORD
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.CloseHandle.restype = wintypes.BOOL
        kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel.WaitForSingleObject.restype = wintypes.DWORD
        self.job = kernel.CreateJobObjectW(None, None)
        self.process = None
        if not self.job:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = Limits()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.
        if not kernel.SetInformationJobObject(self.job, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            kernel.CloseHandle(self.job)
            raise ctypes.WinError(ctypes.get_last_error())
        startup, info = Startup(), ProcessInfo()
        startup.cb = ctypes.sizeof(startup)
        command = ctypes.create_unicode_buffer(subprocess.list2cmdline([executable, *arguments]))
        env = ctypes.create_unicode_buffer("\0".join(f"{key}={value}" for key, value in
                                                     sorted(environment.items())) + "\0\0")
        flags = 0x4 | 0x400 | 0x08000000
        if not kernel.CreateProcessW(executable, command, None, None, False, flags,
                                     env, str(cwd), ctypes.byref(startup), ctypes.byref(info)):
            kernel.CloseHandle(self.job)
            raise ctypes.WinError(ctypes.get_last_error())
        self.process = info.process
        try:
            if not kernel.AssignProcessToJobObject(self.job, info.process):
                kernel.TerminateProcess(info.process, 1)
                raise ctypes.WinError(ctypes.get_last_error())
            if kernel.ResumeThread(info.thread) == 0xFFFFFFFF:
                raise ctypes.WinError(ctypes.get_last_error())
        except BaseException:
            self.close()
            raise
        finally:
            kernel.CloseHandle(info.thread)

    def alive(self):
        return self.kernel.WaitForSingleObject(self.process, 0) == 258

    def close(self):
        if self.job:
            self.kernel.CloseHandle(self.job)
            self.job = None
        if self.process:
            result = self.kernel.WaitForSingleObject(self.process, 5000)
            self.kernel.CloseHandle(self.process)
            self.process = None
            if result != 0:
                raise RuntimeError("The owned process did not exit after closing its Job Object.")


class Model:
    """Scripted provider brain.

    Each request is classified by the tool results already present in the
    current attempt (the walk back to the last user message), so the fixture
    replays fixture step, settlement and final reply without per-request
    bookkeeping. Every settlement tool call is counted, malformed or not.
    A retired independent judge request would carry the judge marker and is
    counted separately: the run must end with zero of them.
    """

    SETTLING_MODES = ("done", "continue", "hold", "blocked", "budget", "repeat", "wait")

    def __init__(self):
        self.lock = threading.Lock()
        self.mode = "done"
        self.hold_at = None
        self.settles = 0
        self.replies = 0
        self.judges = 0
        self.calls = []
        self.entered = threading.Event()
        self.release = threading.Event()
        self.release.set()

    def configure(self, mode, hold_at=None):
        with self.lock:
            self.mode, self.hold_at = mode, hold_at
            self.settles, self.replies = 0, 0
            self.entered.clear()
            self.release.set()
            if hold_at is not None:
                self.release.clear()

    def reply(self, payload):
        messages = payload.get("messages", [])
        serialized = json.dumps(payload)
        is_judge = JUDGE_MARKER in serialized.lower()
        has_settle = has_tool(payload, SETTLE_TOOL)
        has_fixture = has_tool(payload, "goal_fixture_step")
        system = json.dumps([message for message in messages if message.get("role") == "system"])
        inactive = INACTIVE_MARKER in system
        goal_history = SETTLE_TOOL in json.dumps([message for message in messages if message.get("role") != "system"])
        results = 0
        for message in reversed(messages):
            role = message.get("role")
            if role in ("user", "system"):
                break
            if role == "tool":
                results += 1
        last_user = ""
        for message in reversed(messages):
            if message.get("role") == "user":
                content = message.get("content")
                last_user = content if isinstance(content, str) else json.dumps(content)
                break
        reminder = REMINDER_MARKER in last_user
        guided = GUIDANCE_MARKER in last_user
        gate = None
        with self.lock:
            self.calls.append({"judge": is_judge, "settle": has_settle, "fixture": has_fixture,
                               "protocol": PROTOCOL_MARKER in serialized, "results": results,
                               "inactiveGoal": inactive, "goalHistory": goal_history,
                               "systemPrefix": hashlib.sha256(system.encode()).hexdigest(),
                               "toolPrefix": hashlib.sha256(json.dumps(payload.get("tools", []), sort_keys=True).encode()).hexdigest()})
            if is_judge:
                self.judges += 1
            elif self.hold_at is not None and results == self.hold_at:
                self.entered.set()
                gate = self.release
            mode = self.mode
        if gate is not None and not gate.wait(30):
            raise TimeoutError("Held request was never released.")
        if is_judge:
            return {"role": "assistant", "content": "Independent judge requests must never happen."}
        if not has_settle and not has_fixture:
            # Auxiliary OpenCode requests (title generation and friends) declare
            # no tools and stay outside every turn counter.
            return {"role": "assistant", "content": "Fixture session."}
        if not has_settle:
            # Replay the reported failure: historical tool use tempts a normal
            # follow-up into another settlement unless the current system scope forbids it.
            if mode == "stale-goal" and goal_history and not inactive and results == 0:
                return self.settle_call("done")
            return self.text("Plain session reply.")
        if mode == "no-tools":
            # Settle-only worker: the settlement call is bookkeeping and must
            # not count as tool progress, so the stalled guard can trip.
            if results == 0:
                return self.settle_call("continue")
            return self.text()
        if mode == "wait-stall":
            # Settle-only waiter: reports wait without any tool work,
            # replaying the orchestrator pattern that waits on background
            # children instead of calling tools in the goal session.
            if results == 0:
                return self.settle_call("wait")
            return self.text()
        if mode == "double-settle" and results == 2:
            return self.settle_call("continue")
        if results == 0 and has_tool(payload, "goal_fixture_step"):
            return self.tool_call("goal_fixture_step", {})
        if guided:
            # Guidance absorbed mid-turn keeps the worker busy but the turn
            # ends without a fresh settlement: the pre-guidance one was
            # invalidated and must not be consumed.
            return self.text()
        if mode == "malformed":
            if results == 1:
                return self.tool_call(SETTLE_TOOL, {"verdict": "maybe", "reason": ""}, settle=True)
            return self.text()
        if reminder and mode in self.SETTLING_MODES:
            # A settling mode reports continue on the reminder turn, so the
            # goal keeps running until the runner reconfigures the mode.
            if results == 1:
                return self.settle_call("continue")
            return self.text()
        if results == 1 and mode != "no-settle":
            with self.lock:
                index = self.settles + 1
            if mode in ("done", "double-settle"):
                verdict = "done"
            elif mode == "wait":
                verdict = "wait"
            elif mode in ("blocked", "stale"):
                verdict = "blocked"
            elif index >= 2 and mode in ("continue", "hold"):
                verdict = "done"
            else:
                verdict = "continue"
            return self.settle_call(verdict)
        return self.text()

    def text(self, content=None):
        with self.lock:
            self.replies += 1
            reply = content or ("No progress." if self.mode == "repeat" else f"Goal fixture completed step {self.replies}.")
        return {"role": "assistant", "content": reply}

    def settle_call(self, verdict):
        arguments = {"verdict": verdict, "reason": "Scripted verification."}
        if verdict == "done":
            arguments["summary"] = "Fixture work completed."
        return self.tool_call(SETTLE_TOOL, arguments, settle=True)

    def tool_call(self, name, arguments, settle=False):
        if settle:
            with self.lock:
                self.settles += 1
        return {"role": "assistant", "content": None, "tool_calls": [
            {"id": f"call_{uuid.uuid4().hex}", "type": "function",
             "function": {"name": name, "arguments": json.dumps(arguments)}}]}


def has_tool(payload, name):
    return any(tool.get("function", {}).get("name") == name for tool in payload.get("tools", []))


MODEL = Model()


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.send_json({"object": "list", "data": [{"id": "fixture-model", "object": "model"}]})

    def send_json(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        try:
            payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            message = MODEL.reply(payload)
            reason = "tool_calls" if message.get("tool_calls") else "stop"
            common = {"id": "chatcmpl-fixture", "created": 0, "model": "fixture-model"}
            usage = {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}
            if not payload.get("stream"):
                self.send_json({**common, "object": "chat.completion", "usage": usage,
                                "choices": [{"index": 0, "message": message, "finish_reason": reason}]})
                return
            delta = dict(message)
            if delta.get("tool_calls"):
                delta["tool_calls"] = [{**call, "index": index} for index, call in enumerate(delta["tool_calls"])]
            chunks = [{**common, "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                      {**common, "object": "chat.completion.chunk", "usage": usage,
                       "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}]
            data = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except OSError:
            pass


def wait_for(label, read, accept=bool, timeout=25):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        last = read()
        if accept(last):
            return last
        time.sleep(0.08)
    raise AssertionError(f"Timed out waiting for {label}: {last!r}")


def main():
    binary = shutil.which("opencode")
    if not binary:
        raise RuntimeError("OpenCode must be available on PATH.")
    OUTPUT.mkdir(parents=True)
    work = OUTPUT / "workspace"
    work.mkdir()
    plugin = OUTPUT / "goal-plugin"
    plugin.mkdir()
    shutil.copyfile(ROOT / "resources" / "opencode-goal-plugin.ts", plugin / "index.ts")
    fixture = OUTPUT / "fixture-plugin"
    fixture.mkdir()
    (fixture / "index.ts").write_text('''export default { id: "goal.fixture", async setup(ctx) {
      let step = 0;
      const registration = await ctx.tool.transform(editor => editor.add({
        name: "goal_fixture_step", description: "A harmless deterministic fixture step.",
        input: {type:"object",properties:{},additionalProperties:false}, options:{codemode:false},
        async execute() { return {content: `Fixture step ${++step}`}; }
      }));
      const rpc = await ctx.rpc.register({id: "goal.fixture", methods: {
        finish: {input: {type: "object", properties: {sessionID: {type: "string"}, childID: {type: "string"}}, required: ["sessionID"]}, output: {}}
      }, events: {}}, {async finish({sessionID, childID}) {
        const notification = {sessionID, text: "Background fixture completed. Verify the result.",
          description: "Background completion", metadata: {goalFixture: true}};
        if (childID) {
          notification.text = `<subagent sessionID="${childID}" state="completed" description="Fixture batch">\nFixture background batch completed.\n</subagent>`;
          notification.description = "Fixture batch completion";
          notification.metadata = {source: "subagent", childID, agent: "General", state: "completed"};
        }
        await ctx.session.synthetic(notification);
        return {};
      }});
      return async () => { await rpc.dispose(); await registration.dispose(); };
    }};''', encoding="utf-8")
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    config = {"model": "fixture/fixture-model", "small_model": "fixture/fixture-model",
              "plugins": [str(plugin), str(fixture)],
              "permission": {"goal_fixture_step": "allow", SETTLE_TOOL: "allow"},
              "provider": {"fixture": {"npm": "@ai-sdk/openai-compatible", "name": "Goal fixture",
                "options": {"apiKey": "fixture-only", "baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                "models": {"fixture-model": {"name": "Fixture", "tool_call": True,
                  "limit": {"context": 128000, "output": 8192}}}}}}
    env = {key: value for key, value in os.environ.items() if key.upper() in
           {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}}
    for key, subdirectory in {"HOME": "home", "USERPROFILE": "home", "APPDATA": "roaming",
                              "LOCALAPPDATA": "local", "TEMP": "tmp", "TMP": "tmp",
                              "OPENCODE_CONFIG_DIR": "config", "XDG_CONFIG_HOME": "xdg/config",
                              "XDG_DATA_HOME": "xdg/data", "XDG_CACHE_HOME": "xdg/cache",
                              "XDG_STATE_HOME": "xdg/state"}.items():
        directory = OUTPUT / subdirectory
        directory.mkdir(parents=True, exist_ok=True)
        env[key] = str(directory)
    password = uuid.uuid4().hex
    env.update({"OPENCODE_CONFIG_CONTENT": json.dumps(config), "OPENCODE_DB": str(OUTPUT / "opencode.db"),
                "OPENCODE_SERVER_USERNAME": "opencode", "OPENCODE_SERVER_PASSWORD": password,
                "OPENCODE_DISABLE_AUTOUPDATE": "1", "OPENCODE_DISABLE_FILEWATCHER": "1",
                "OPENCODE_DISABLE_PROJECT_CONFIG": "1", "OPENCODE_CONFIG_PROJECT_DISABLE": "1",
                "NO_PROXY": "127.0.0.1,localhost,::1"})
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    authorization = "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()
    base = f"http://127.0.0.1:{port}"
    query = "?" + urlencode({"location[directory]": str(work)})
    checks = []
    child = None

    def api(method, path, value=None, allow_error=False):
        data = None if value is None else json.dumps(value).encode()
        request = Request(base + path, data=data, method=method,
                          headers={"Authorization": authorization, "Content-Type": "application/json"})
        try:
            with HTTP.open(request, timeout=15) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except HTTPError as error:
            detail = error.read().decode(errors="replace")
            if allow_error:
                return {"status": error.code, "detail": detail}
            raise AssertionError(f"{method} {path} failed: {error.code} {detail}") from error

    def control(session, action, **kwargs):
        result = api("POST", "/api/rpc/fintwind.goal/control" + query,
                     {"input": {"sessionID": session, "action": {"type": action, **kwargs}}})
        return result["output"]

    def goal_elapsed(state):
        running = max(0, int(time.time()) - state["activeSince"]) if state["status"] == "active" else 0
        return state["elapsedSeconds"] + running

    ordinary_profiles = {}

    def plain_after_goal(session_id, expected, expect_history=True, force_call=False):
        MODEL.configure("stale-goal" if force_call else "done")
        calls_mark = len(MODEL.calls)
        history_before = context_messages(session_id)
        api("POST", f"/api/session/{session_id}/prompt" + query,
            {"text": "A new ordinary request. Describe the result without continuing the old Goal."})
        wait_for("ordinary follow-up reply", lambda: MODEL.replies, lambda replies: replies >= 1)
        time.sleep(0.5)
        main_calls = [call for call in MODEL.calls[calls_mark:] if call["fixture"]]
        assert main_calls and all(call["goalHistory"] == expect_history
                                  and not call["inactiveGoal"] and not call["settle"]
                                  and not call["protocol"] for call in main_calls), main_calls
        profiles = {(call["systemPrefix"], call["toolPrefix"]) for call in main_calls}
        if session_id not in ordinary_profiles:
            assert not expect_history and len(profiles) == 1, main_calls
            ordinary_profiles[session_id] = next(iter(profiles))
        assert profiles == {ordinary_profiles[session_id]}, "Goal scope changed ordinary system/tool cache prefixes"
        assert MODEL.settles == (1 if force_call else 0), MODEL.calls[calls_mark:]
        assert control(session_id, "get") == expected, "Ordinary conversation changed the previous Goal"
        assert context_messages(session_id)[:len(history_before)] == history_before, "Goal gating rewrote persisted history"

    def context_messages(session):
        return api("GET", f"/api/session/{session}/context" + query)["data"]

    def has_synthetic(session, marker):
        return any(message.get("type") == "synthetic" and marker in json.dumps(message)
                   for message in context_messages(session))

    def synthetic_count(session, marker):
        return sum(1 for message in context_messages(session)
                   if message.get("type") == "synthetic" and marker in json.dumps(message))

    settings_revision = 0

    def set_enabled(enabled):
        nonlocal settings_revision
        settings_revision += 1
        (plugin / "settings.json").write_text(json.dumps({"enabled": enabled, "revision": settings_revision}), encoding="utf-8")

    def goal_command_available():
        result = api("GET", "/api/command" + query)
        return any(command.get("name") == "goal" for command in result.get("data", []))

    def assert_disabled(session):
        for action in ("start", "resume"):
            try:
                control(session, action, **({"objective": "Disabled objective"} if action == "start" else {}))
            except AssertionError:
                pass
            else:
                raise AssertionError(f"Goal {action} accepted while disabled.")

    def session():
        result = api("POST", "/api/session", {"location": {"directory": str(work)}})
        return result["data"]["id"]

    def check(label):
        checks.append(label)
        print("PASS " + label, flush=True)

    try:
        child = OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)

        def ready():
            if not child.alive():
                raise AssertionError("Owned OpenCode process exited during startup.")
            try:
                return api("GET", "/api/info")
            except (URLError, TimeoutError):
                return None

        info = wait_for("OpenCode startup", ready, timeout=45)
        first, second = session(), session()
        plugins = wait_for("goal activation", lambda: api("GET", "/api/plugin" + query),
                           lambda result: any(p.get("id") == "fintwind.goal" and p.get("state", {}).get("status") == "active"
                                              for p in result.get("data", [])))
        assert control(first, "get") is None
        check("plugin loads on real OpenCode; untouched session has no goal")
        assert not goal_command_available()
        assert_disabled(first)
        assert MODEL.replies == 0
        check("missing settings default to disabled and expose no goal command")
        set_enabled(True)
        wait_for("goal enabled without restart", goal_command_available)
        check("enabling the private setting registers goal on the running server")
        MODEL.configure("done")
        calls_mark = len(MODEL.calls)
        first_input_id = f"msg_{(int(time.time() * 1000) * 0x1000) & 0xffffffffffff:012x}{uuid.uuid4().hex[:14]}"
        start = api("POST", "/api/rpc/fintwind.goal/control" + query,
                    {"input": {"sessionID": first, "inputID": first_input_id,
                               "action": {"type": "start", "objective": "Complete the first fixture step."}}},
                    allow_error=True)
        assert start and start.get("status") is None, start
        done = wait_for("first-turn settlement", lambda: control(first, "get"),
                        lambda state: state and state["status"] == "done")
        window = MODEL.calls[calls_mark:]
        goal_window = [entry for entry in window if entry["settle"]]
        aux_window = [entry for entry in window if not entry["settle"] and not entry["fixture"]]
        assert MODEL.judges == 0 and len(goal_window) == 3 and len(aux_window) == 1, window
        assert all(entry["settle"] and entry["protocol"] for entry in goal_window), window
        assert all(not entry["settle"] and not entry["protocol"] for entry in aux_window), window
        assert MODEL.settles == 1 and MODEL.replies == 1, (MODEL.settles, MODEL.replies)
        assert control(second, "get") is None
        check("the model settles the goal inside the main turn with no extra provider request")
        first_context = context_messages(first)
        first_users = [message for message in first_context if message.get("type") == "user"]
        assert len(first_users) == 1, first_context
        assert first_users[0]["id"] == first_input_id, first_users
        assert first_users[0].get("metadata", {}).get("fintwindGoal") is True, first_users
        assert "Complete the first fixture step." in json.dumps(first_users), first_users
        assert done["result"] == "Fixture work completed.", done
        assert done["model"] == "fixture/fixture-model", done
        assert done["tools"] == 1, done
        assert isinstance(done["completedAt"], int) and done["completedAt"] > 0, done
        check("the initial objective is durably stored as the real user message")
        MODEL.configure("continue")
        calls_mark = len(MODEL.calls)
        control(first, "start", objective="Complete two fixture steps.")
        continued = wait_for("continued settlement", lambda: control(first, "get"),
                             lambda state: state and state["status"] == "done")
        assert continued["turns"] == 2 and MODEL.settles == 2, (continued, MODEL.settles)
        assert len(MODEL.calls[calls_mark:]) == 6, MODEL.calls[calls_mark:]
        assert has_synthetic(first, CONTINUATION_MARKER), context_messages(first)
        assert not has_synthetic(first, REMINDER_MARKER), context_messages(first)
        check("a continue report schedules exactly one continuation then completes")
        messages = context_messages(first)
        assert any(message.get("type") == "synthetic" and message.get("metadata", {}).get("fintwindGoal")
                   for message in messages), messages
        users = [message for message in messages if message.get("type") == "user"]
        assert len(users) == 2, messages
        assert "Complete two fixture steps." in json.dumps(users[-1]), users
        check("automatic continuation is a synthetic input rather than a forged user message")
        retired_session = session()
        plain_after_goal(retired_session, None, expect_history=False)
        MODEL.configure("done")
        control(retired_session, "start", objective="Finish before a normal follow-up.")
        retired_goal = wait_for("retired Goal completion", lambda: control(retired_session, "get"),
                                lambda state: state and state["status"] == "done")
        plain_after_goal(retired_session, retired_goal)
        check("completed Goal follow-ups have no settlement tool or extra context and preserve cache prefixes")
        plain_after_goal(retired_session, retired_goal, force_call=True)
        check("a forced stale settlement is rejected without changing the completed Goal")
        control(retired_session, "clear")
        plain_after_goal(retired_session, None)
        check("clearing a Goal keeps historical settlement tools out of ordinary requests")
        MODEL.configure("wait")
        control(retired_session, "start", objective="Pause before a normal follow-up.")
        wait_for("retired Goal waits", lambda: control(retired_session, "get"),
                 lambda state: state and state["turns"] == 1)
        paused_goal = control(retired_session, "pause")
        plain_after_goal(retired_session, paused_goal)
        check("paused Goals do not authorize settlement in ordinary follow-ups")
        MODEL.configure("blocked")
        control(retired_session, "start", objective="Block before a normal follow-up.")
        blocked_goal = wait_for("retired Goal blocked", lambda: control(retired_session, "get"),
                               lambda state: state and state["status"] == "blocked")
        plain_after_goal(retired_session, blocked_goal)
        check("blocked Goals do not authorize settlement in ordinary follow-ups")
        MODEL.configure("done")
        control(retired_session, "start", objective="Explicitly create a new Goal after ordinary conversation.")
        retired_goal = wait_for("explicit new Goal completion", lambda: control(retired_session, "get"),
                                lambda state: state and state["status"] == "done")
        assert MODEL.settles == 1
        check("an explicit new Goal restores the settlement tool after inactive conversation")
        MODEL.configure("double-settle")
        control(retired_session, "start", objective="Accept only one settlement per execution.")
        retired_goal = wait_for("duplicate settlement rejected", lambda: control(retired_session, "get"),
                                lambda state: state and state["status"] == "done")
        assert retired_goal["turns"] == 1 and MODEL.settles == 2, (retired_goal, MODEL.settles)
        assert retired_goal["result"] == "Fixture work completed.", retired_goal
        check("a duplicate settlement cannot overwrite the first verdict or extend a completed Goal")
        waiting_session = session()
        MODEL.configure("wait")
        control(waiting_session, "start", objective="Wait for the background completion notification.")
        waiting = wait_for("waiting settlement", lambda: control(waiting_session, "get"),
                           lambda state: state and state["turns"] >= 1)
        time.sleep(1)
        waiting = control(waiting_session, "get")
        assert waiting["status"] == "active" and waiting["turns"] == 1, waiting
        assert MODEL.settles == 1 and MODEL.replies == 1, (MODEL.settles, MODEL.replies)
        assert not has_synthetic(waiting_session, CONTINUATION_MARKER), context_messages(waiting_session)
        assert not has_synthetic(waiting_session, REMINDER_MARKER), context_messages(waiting_session)
        check("a wait report keeps the goal active without forcing another turn")
        MODEL.configure("done")
        api("POST", "/api/rpc/goal.fixture/finish" + query, {"input": {"sessionID": waiting_session}})
        awakened = wait_for("background completion settles goal", lambda: control(waiting_session, "get"),
                            lambda state: state and state["status"] == "done")
        assert awakened["turns"] == 2 and awakened["tools"] == 2, awakened
        check("a background notification resumes the waiting goal and can complete it")
        waiting_session = session()
        MODEL.configure("wait-stall")
        control(waiting_session, "start", objective="Wait for staged background batches without stalling.")
        for index in range(3):
            staged = wait_for(f"staged wait settlement {index}", lambda: control(waiting_session, "get"),
                              lambda state: state and state["turns"] >= index + 1)
            assert staged["status"] == "active", staged
            api("POST", "/api/rpc/goal.fixture/finish" + query,
                {"input": {"sessionID": waiting_session, "childID": f"ses_fixture_batch_{index}"}})
        assert control(waiting_session, "get")["status"] == "active"
        check("wait settlements backed by fresh subagent completions never stall")
        wait_for("final staged settlement", lambda: control(waiting_session, "get"),
                 lambda state: state and state["turns"] >= 4 and state["status"] == "active")
        MODEL.configure("done")
        api("POST", "/api/rpc/goal.fixture/finish" + query, {"input": {"sessionID": waiting_session}})
        finished = wait_for("staged waiter completion", lambda: control(waiting_session, "get"),
                            lambda state: state and state["status"] == "done")
        assert finished["turns"] == 5, finished
        check("a staged waiter finishes normally once the model reports done")
        MODEL.configure("done")
        calls_mark = len(MODEL.calls)
        api("POST", f"/api/session/{second}/prompt" + query, {"text": "Hello without a goal."})
        plain = wait_for("plain session reply", lambda: MODEL.replies, lambda count: count >= 1)
        time.sleep(0.5)
        plain_window = MODEL.calls[calls_mark:]
        assert plain >= 1 and plain_window and all(not entry["settle"] and not entry["protocol"] for entry in plain_window), plain_window
        assert control(second, "get") is None
        check("the settlement tool and protocol appear only in goal sessions")
        MODEL.configure("stale", hold_at=2)
        control(first, "start", objective="Keep the objective while following manual guidance.")
        assert MODEL.entered.wait(20), MODEL.calls
        reminders_before = synthetic_count(first, REMINDER_MARKER)
        api("POST", f"/api/session/{first}/prompt" + query, {"text": GUIDANCE_MARKER})
        with MODEL.lock:
            MODEL.mode = "done"
        MODEL.release.set()
        wait_for("stale settlement reminder", lambda: synthetic_count(first, REMINDER_MARKER) == reminders_before + 1)
        assert control(first, "get")["status"] != "blocked"
        MODEL.configure("done")
        completed = wait_for("goal completes after manual guidance", lambda: control(first, "get"),
                             lambda state: state and state["status"] == "done")
        assert completed["objective"] == "Keep the objective while following manual guidance.", completed
        check("manual guidance preserves the active objective and invalidates its pending settlement")
        MODEL.configure("hold", hold_at=2)
        control(first, "start", objective="Preserve this objective when Goal is disabled.")
        assert MODEL.entered.wait(20)
        set_enabled(False)
        wait_for("goal command removed", goal_command_available, lambda available: not available)
        disabled = wait_for("disabled goal pauses", lambda: control(first, "get"), lambda state: state and state["status"] == "paused")
        assert disabled["objective"] == "Preserve this objective when Goal is disabled."
        assert_disabled(first)
        frozen = len(MODEL.calls)
        MODEL.release.set()
        time.sleep(0.5)
        assert len(MODEL.calls) == frozen, MODEL.calls[frozen - 3:]
        check("disabling removes the command and pauses the goal without losing its objective")
        set_enabled(True)
        wait_for("goal command restored", goal_command_available)
        time.sleep(0.5)
        assert control(first, "get")["status"] == "paused" and len(MODEL.calls) == frozen
        check("reenabling restores the command without resuming an old goal")
        MODEL.configure("hold", hold_at=2)
        control(first, "resume")
        assert MODEL.entered.wait(20)
        set_enabled(False)
        set_enabled(True)
        wait_for("quick disable-enable pauses", lambda: control(first, "get"), lambda state: state and state["status"] == "paused")
        frozen = len(MODEL.calls)
        MODEL.release.set()
        time.sleep(0.5)
        assert len(MODEL.calls) == frozen, MODEL.calls[frozen - 3:]
        check("a quick off-on cycle still invalidates a pending settlement")
        (plugin / "settings.json").write_text("invalid JSON", encoding="utf-8")
        wait_for("invalid settings hide command", goal_command_available, lambda available: not available)
        assert_disabled(first)
        check("invalid private settings fail closed")
        set_enabled(True)
        wait_for("valid settings restore command", goal_command_available)
        MODEL.configure("hold", hold_at=2)
        control(first, "start", objective="Wait with a pending settlement.")
        assert MODEL.entered.wait(20), MODEL.calls
        paused = control(first, "pause")
        assert paused["status"] == "paused"
        frozen = len(MODEL.calls)
        MODEL.release.set()
        time.sleep(1)
        assert control(first, "get")["status"] == "paused" and len(MODEL.calls) == frozen, MODEL.calls[frozen - 3:]
        check("pausing while a settlement is pending prevents stale continuation")
        MODEL.configure("hold", hold_at=2)
        timer_start = control(first, "start", objective="Accumulate active Goal time across pauses.")
        assert timer_start["elapsedSeconds"] == 0, timer_start
        assert MODEL.entered.wait(20)
        running_clock = wait_for("active Goal clock", lambda: control(first, "get"),
                                 lambda state: goal_elapsed(state) >= 2)
        time.sleep(1.2)
        assert control(first, "get") == running_clock, "Clock checkpoints must not publish new Goal revisions"
        timer_pause = control(first, "pause")
        assert timer_pause["startedAt"] == timer_start["startedAt"] and "activeSince" not in timer_pause
        time.sleep(1.2)
        assert control(first, "get") == timer_pause
        MODEL.release.set()
        time.sleep(1)
        MODEL.configure("hold", hold_at=2)
        timer_resume = control(first, "resume")
        assert timer_resume["startedAt"] == timer_start["startedAt"], (timer_start, timer_resume)
        assert timer_resume["elapsedSeconds"] == timer_pause["elapsedSeconds"], (timer_pause, timer_resume)
        assert MODEL.entered.wait(20)
        wait_for("resumed Goal clock", lambda: control(first, "get"),
                 lambda state: goal_elapsed(state) >= timer_pause["elapsedSeconds"] + 2)
        manual_done = control(first, "complete")
        assert manual_done["status"] == "done" and manual_done["completedAt"] >= timer_start["startedAt"]
        assert "activeSince" not in manual_done and manual_done["elapsedSeconds"] >= timer_pause["elapsedSeconds"] + 2
        time.sleep(1.2)
        assert control(first, "get") == manual_done
        MODEL.release.set()
        time.sleep(1)
        check("Goal clock freezes on pause, resumes cumulatively, and freezes with a manual completion timestamp")
        MODEL.configure("hold", hold_at=2)
        control(first, "start", objective="An old objective with a pending settlement.")
        assert MODEL.entered.wait(20)
        with MODEL.lock:
            MODEL.mode = "done"
            before = MODEL.replies
        control(first, "start", objective="The replacement objective.")
        MODEL.release.set()
        replacement = wait_for("replacement settlement", lambda: control(first, "get"),
                               lambda state: state and state["status"] == "done", timeout=40)
        assert replacement["objective"] == "The replacement objective."
        assert MODEL.replies > before
        check("a replacement goal settles on its own generation")
        control(first, "pause")
        MODEL.configure("done")
        resumed = control(first, "resume")
        assert resumed["startedAt"] == replacement["startedAt"] and resumed["elapsedSeconds"] == replacement["elapsedSeconds"]
        resumed_done = wait_for("resume completion", lambda: control(first, "get"), lambda state: state and state["status"] == "done")
        assert resumed_done["turns"] > replacement["turns"] and resumed_done["tools"] > replacement["tools"]
        assert resumed_done["elapsedSeconds"] >= replacement["elapsedSeconds"]
        check("explicit resume starts a new execution")
        MODEL.configure("blocked")
        control(first, "start", objective="Recognize an external blocker.")
        blocked_state = wait_for("blocked report", lambda: control(first, "get"), lambda state: state and state["status"] == "blocked")
        assert blocked_state["reason"] == "Scripted verification.", blocked_state
        check("a blocked report stops automatic continuation")
        time.sleep(1.2)
        assert control(first, "get") == blocked_state and "activeSince" not in blocked_state
        check("a blocked Goal freezes its active time")
        MODEL.configure("malformed")
        calls_mark = len(MODEL.calls)
        reminders_before = synthetic_count(first, REMINDER_MARKER)
        control(first, "start", objective="Reject malformed settlement input.")
        malformed = wait_for("malformed settlement pause", lambda: control(first, "get"),
                             lambda state: state and state["status"] == "paused")
        assert malformed["turns"] == 2 and MODEL.settles == 2, (malformed, MODEL.settles)
        assert len(MODEL.calls[calls_mark:]) == 6, MODEL.calls[calls_mark:]
        assert synthetic_count(first, REMINDER_MARKER) == reminders_before + 1
        check("a malformed report never settles and pauses after one reminder")
        MODEL.configure("no-settle")
        calls_mark = len(MODEL.calls)
        control(first, "start", objective="Finish without ever reporting.")
        unreported = wait_for("missing report pause", lambda: control(first, "get"),
                              lambda state: state and state["status"] == "paused")
        assert unreported["turns"] == 2 and MODEL.settles == 0, (unreported, MODEL.settles)
        assert len(MODEL.calls[calls_mark:]) == 4, MODEL.calls[calls_mark:]
        check("a missing report triggers exactly one reminder then pauses")
        MODEL.configure("no-tools")
        control(first, "start", objective="Stop a worker that only reports.")
        stalled = wait_for("no-tools stall", lambda: control(first, "get"), lambda state: state and state["status"] == "blocked")
        assert stalled["turns"] == 3 and MODEL.settles == 3, (stalled, MODEL.settles)
        check("the no-progress guard stops a settle-only worker")
        MODEL.configure("wait-stall")
        control(first, "start", objective="Stop a waiter that cites no background progress.")
        for index in range(2):
            wait_for(f"idle waiter turn {index}", lambda: control(first, "get"),
                     lambda state: state and state["turns"] >= index + 1)
            api("POST", "/api/rpc/goal.fixture/finish" + query, {"input": {"sessionID": first}})
        idle_blocked = wait_for("idle waiter stall", lambda: control(first, "get"),
                                lambda state: state and state["status"] == "blocked")
        assert idle_blocked["turns"] == 3 and MODEL.settles == 3, (idle_blocked, MODEL.settles)
        assert idle_blocked["reason"] == "No further progress detected", idle_blocked
        check("wait reports without background evidence still hit the stall guard")
        MODEL.configure("repeat")
        control(first, "start", objective="Stop repeated replies despite real tool calls.")
        repeat = wait_for("repeat guard", lambda: control(first, "get"), lambda state: state and state["status"] == "blocked")
        assert repeat["turns"] <= 4, repeat
        check("repeated replies stop even when the worker calls tools")
        MODEL.configure("budget")
        control(first, "start", objective="Respect the execution budget despite continued progress.")
        budget = wait_for("turn budget", lambda: control(first, "get"),
                          lambda state: state and state["status"] == "paused", timeout=90)
        assert budget["turns"] == 20 and MODEL.replies == 20, (budget, MODEL.replies)
        check("the fixed twenty-turn budget stops a continuously progressing worker")
        MODEL.configure("done")
        control(first, "resume")
        after_budget = wait_for("resume after budget", lambda: control(first, "get"),
                                lambda state: state and state["status"] == "done")
        assert after_budget["turns"] == budget["turns"] + 1 and after_budget["tools"] > budget["tools"]
        assert after_budget["startedAt"] == budget["startedAt"] and after_budget["elapsedSeconds"] >= budget["elapsedSeconds"]
        check("resume grants a new turn budget while preserving cumulative Goal metrics")
        saved = control(first, "pause")
        child.close()
        replacement_plugin = OUTPUT / "rematerialized-goal-plugin"
        replacement_plugin.mkdir()
        shutil.copyfile(plugin / "index.ts", replacement_plugin / "index.ts")
        shutil.copyfile(plugin / "settings.json", replacement_plugin / "settings.json")
        config["plugins"][0] = str(replacement_plugin)
        env["OPENCODE_CONFIG_CONTENT"] = json.dumps(config)
        child = OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)
        wait_for("restarted OpenCode", ready, timeout=45)
        wait_for("restarted goal plugin", lambda: api("GET", "/api/plugin" + query),
                 lambda result: any(p.get("id") == "fintwind.goal" and p.get("state", {}).get("status") == "active"
                                    for p in result.get("data", [])))
        assert control(first, "get") == saved
        assert control(second, "get") is None
        check("durable paused goal survives restart with a rematerialized plugin path")
        control(first, "clear")
        assert control(first, "get") is None
        assert control(second, "get") is None
        check("clear removes the target without touching another session")
        MODEL.configure("done")
        api("POST", f"/api/session/{first}/command" + query, {"name": "goal", "text": "Complete the slash command objective."})
        wait_for("slash goal completion", lambda: control(first, "get"), lambda state: state and state["status"] == "done")
        calls = len(MODEL.calls)
        api("POST", f"/api/session/{first}/command" + query, {"name": "goal", "text": "status"})
        time.sleep(0.5)
        assert len(MODEL.calls) == calls
        check("slash commands start a goal and query status without a model call")
        other = OUTPUT / "other-workspace"
        other.mkdir()
        other_query = "?" + urlencode({"location[directory]": str(other)})
        api("GET", "/api/plugin" + other_query)
        foreign = api("POST", "/api/rpc/fintwind.goal/control" + other_query,
            {"input": {"sessionID": first, "action": {"type": "start", "objective": "Wrong location"}}},
            allow_error=True)
        assert foreign and 400 <= foreign["status"] < 600, foreign
        assert control(first, "get")["objective"] == "Complete the slash command objective."
        check("a different location cannot replace this session goal")
        MODEL.configure("hold", hold_at=2)
        control(first, "start", objective="Do not restart active goals automatically.")
        assert MODEL.entered.wait(20)
        checkpoint = wait_for("durable Goal clock checkpoint", lambda: control(first, "get"),
                              lambda state: goal_elapsed(state) >= 2)
        checkpoint_elapsed = goal_elapsed(checkpoint)
        assert control(first, "get") == checkpoint, "Clock polling must not change the public revision or snapshot"
        child.close()
        time.sleep(1.2)
        MODEL.release.set()
        MODEL.configure("done")
        child = OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)
        wait_for("second restarted OpenCode", ready, timeout=45)
        wait_for("second restarted plugin", lambda: api("GET", "/api/plugin" + query),
                 lambda result: any(p.get("id") == "fintwind.goal" and p.get("state", {}).get("status") == "active"
                                    for p in result.get("data", [])))
        recovered = control(first, "get")
        assert recovered["status"] == "paused", recovered
        assert abs(recovered["elapsedSeconds"] - checkpoint_elapsed) <= 1 and "activeSince" not in recovered
        assert recovered["startedAt"] == checkpoint["startedAt"]
        check("restart retains the saved Goal clock without counting offline time")
        frozen = len(MODEL.calls)
        time.sleep(0.5)
        assert len(MODEL.calls) == frozen, MODEL.calls[frozen:]
        check("a previously active goal requires explicit resume after server restart")
        plain_after_goal(retired_session, retired_goal)
        check("a cold ordinary request keeps historical Goals inactive without changing the prompt prefix")
        child.close()
        legacy_goal = {**recovered, "startedAt": int(time.time()) - 120,
                       "lastEvent": "", "lastText": "", "stalled": 0, "repeats": 0}
        legacy_goal.pop("elapsedSeconds", None)
        legacy_goal.pop("activeSince", None)
        legacy_record = {"revision": recovered["revision"], "goal": legacy_goal}
        migration_source = (ROOT / "resources/opencode-goal-plugin.ts").read_text(encoding="utf-8")
        migration_source = migration_source.replace("export default {", "const goalPlugin = {", 1)
        migration_source += ("\nexport default {id: goalPlugin.id, async setup(ctx: Context) {\n"
                             + "await ctx.storage.set(" + json.dumps("session:" + first) + ", "
                             + json.dumps(legacy_record) + ");\nreturn goalPlugin.setup(ctx);\n}};\n")
        (replacement_plugin / "index.ts").write_text(migration_source, encoding="utf-8")
        child = OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)
        wait_for("legacy migration OpenCode", ready, timeout=45)
        api("GET", f"/api/session/{first}" + query)
        wait_for("legacy migration plugin", lambda: api("GET", "/api/plugin" + query),
                 lambda result: any(p.get("id") == "fintwind.goal" and p.get("state", {}).get("status") == "active"
                                    for p in result.get("data", [])))
        migrated = control(first, "get")
        assert migrated["status"] == "paused" and migrated["elapsedSeconds"] >= 120, migrated
        assert migrated["startedAt"] == legacy_goal["startedAt"] and "activeSince" not in migrated
        time.sleep(1.2)
        assert control(first, "get") == migrated
        check("legacy paused Goals migrate once and stop accumulating paused time")
        assert MODEL.judges == 0, MODEL.calls[-5:]
        assert len(checks) == 45, checks
        (OUTPUT / "report.json").write_text(json.dumps({"status": "passed", "surface": "real-opencode-scripted-provider",
              "nativeUI": "not exercised", "checks": checks, "info": info, "pluginState": plugins,
              "completedGoal": done}, indent=2), encoding="utf-8")
    except BaseException as error:
        (OUTPUT / "report.json").write_text(json.dumps({"status": "failed", "checks": checks,
              "error": repr(error), "providerCalls": MODEL.calls[-60:]}, indent=2), encoding="utf-8")
        raise
    finally:
        MODEL.release.set()
        if child:
            child.close()
        provider.shutdown()
        provider.server_close()
        print("Report: " + str(OUTPUT / "report.json"), flush=True)


if __name__ == "__main__":
    main()
