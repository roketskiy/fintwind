"""Black-box E2E for the frozen Mermaid prompt version.

The real ``resources/opencode-context-plugin.ts`` runs inside an isolated,
privately spawned OpenCode V2 server. A loopback OpenAI-compatible provider
records the outgoing chat requests and answers with short deterministic text;
no real model, credential, public OpenCode service, or user data is used.

The script only proves OpenCode request behavior for sessions created through
the V2 session-create metadata. It does not exercise the Fintwind desktop UI or
the Rust settings snapshot.

Usage (Windows, Python 3.12):

    python scripts/mermaid-prompt-e2e.py --opencode C:\\path\\to\\opencode.exe

A run writes target/mermaid-prompt-e2e/<uuid>/report.json and exits non-zero on
failure. The report contains checks, capture classification, and prompt hashes,
never complete provider payloads or environment dumps.
"""

import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener


ROOT = Path(__file__).resolve().parent.parent
HTTP = build_opener(ProxyHandler({}))

MARKER = "Fintwind Mermaid visualization (v1)."
# Frozen contract. A wording change needs a new version, so this runner owns a
# byte-for-byte copy instead of parsing the TypeScript source.
MERMAID_PROMPT_V1 = "\n".join([
    "Fintwind Mermaid visualization (v1).",
    "Proactively include a concise fenced mermaid code block when explaining nontrivial architecture, relationships, data flow, or a multi-step process that benefits from visualization, even when a diagram was not explicitly requested. Do not explain such structure only in prose.",
    "Prefer simple flowchart or sequenceDiagram syntax, short quoted labels, and the user's language. Fintwind renders Mermaid blocks directly in the conversation.",
    "Use plain Mermaid without HTML labels, external images, clickable links, icons, custom CSS, or theme directives. Output the diagram source directly; no rendering tools or generated files are needed unless requested.",
    "Do not force diagrams into simple answers. Follow explicit user requests for a different format or workflow.",
])
TOOL_NAME = "mermaid_fixture_probe"
PLUGIN_SOURCE = ROOT / "resources" / "opencode-context-plugin.ts"


class Budget:
    """One wall-clock budget for every HTTP call, wait, and fixture step."""

    def __init__(self, seconds):
        self.end = time.monotonic() + seconds

    def left(self):
        return max(0.0, self.end - time.monotonic())

    def cap(self, timeout):
        remaining = self.left()
        if remaining <= 0:
            raise TimeoutError("Total E2E deadline exceeded")
        return max(0.05, min(float(timeout), remaining))

    def sleep(self, seconds):
        time.sleep(self.cap(seconds))


BUDGET = None


def sha256(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def system_text(payload):
    parts = []
    messages = payload.get("messages")
    if isinstance(messages, list):
        for message in messages:
            if isinstance(message, dict) and message.get("role") == "system":
                content = message.get("content")
                parts.append(content if isinstance(content, str) else canonical(content))
    if "system" in payload:
        content = payload.get("system")
        parts.append(content if isinstance(content, str) else canonical(content))
    return "\n".join(parts)


def tools_hash(tools):
    if not isinstance(tools, list):
        return sha256(canonical(tools))
    def name(tool):
        function = tool.get("function") if isinstance(tool, dict) else None
        return function.get("name", "") if isinstance(function, dict) else ""
    return sha256(canonical(sorted(tools, key=name)))


def tool_result_since_last_user(payload):
    messages = payload.get("messages") or []
    for message in reversed(messages):
        if not isinstance(message, dict):
            continue
        role = message.get("role")
        if role == "user":
            return False
        if role == "tool":
            return True
    return False


class Brain:
    """Scripted model: classify every request, then answer deterministically."""

    def __init__(self):
        self.lock = threading.Lock()
        self.records = []
        self.mode = "text"
        self.text_index = 0

    @property
    def primary_count(self):
        with self.lock:
            return sum(1 for record in self.records if record["class"] == "primary")

    def snapshot(self):
        with self.lock:
            return list(self.records)

    def reply(self, payload):
        prompt = system_text(payload)
        tools = payload.get("tools")
        primary = isinstance(tools, list) and len(tools) > 0
        with self.lock:
            mode = self.mode
            tool_call = primary and mode == "probe" and not tool_result_since_last_user(payload)
            if tool_call:
                message = self.tool_call()
            elif primary:
                message = {
                    "role": "assistant",
                    "content": f"Mermaid fixture reply {self.text_index + 1}.",
                }
                self.text_index += 1
            else:
                message = {"role": "assistant", "content": "Mermaid fixture auxiliary reply."}
            record = {
                "class": "primary" if primary else "auxiliary",
                "systemSha256": sha256(prompt),
                "toolsSha256": tools_hash(tools),
                "instructionCount": prompt.count(MERMAID_PROMPT_V1),
                "markerCount": prompt.count(MARKER),
                "messageCount": len(payload.get("messages") or []),
                "toolCall": tool_call,
                "payload": payload,
            }
            self.records.append(record)
        return message

    def tool_call(self):
        return {
            "role": "assistant",
            "content": None,
            "tool_calls": [{
                "id": "call_" + uuid.uuid4().hex,
                "type": "function",
                "function": {"name": TOOL_NAME, "arguments": "{}"},
            }],
        }


BRAIN = Brain()


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def send_json(self, value, status=200):
        data = json.dumps(value).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        path = self.path.split("?", 1)[0].rstrip("/")
        if path.endswith("/models"):
            self.send_json({"object": "list", "data": [{"id": "fixture-model", "object": "model"}]})
        else:
            self.send_json({"id": "fixture-model", "object": "model"})

    def do_POST(self):
        try:
            length = int(self.headers.get("Content-Length", "0"))
            payload = json.loads(self.rfile.read(length) or b"{}")
            message = BRAIN.reply(payload)
            reason = "tool_calls" if message.get("tool_calls") else "stop"
            common = {"id": "chatcmpl-mermaid", "created": 0, "model": "fixture-model"}
            usage = {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}
            if not payload.get("stream"):
                self.send_json({**common, "object": "chat.completion", "usage": usage,
                                "choices": [{"index": 0, "message": message, "finish_reason": reason}]})
                return
            delta = dict(message)
            if delta.get("tool_calls"):
                delta["tool_calls"] = [{**call, "index": index}
                                       for index, call in enumerate(delta["tool_calls"])]
            chunks = [
                {**common, "object": "chat.completion.chunk",
                 "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                {**common, "object": "chat.completion.chunk", "usage": usage,
                 "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]},
            ]
            data = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks)
                    + "data: [DONE]\n\n").encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (OSError, ValueError):
            pass


FIXTURE_PLUGIN = '''export default { id: "mermaid.fixture", async setup(ctx) {
  const tool = await ctx.tool.transform(editor => editor.add({
    name: "''' + TOOL_NAME + '''",
    description: "A deterministic fixture probe.",
    input: {type: "object", properties: {}, additionalProperties: false},
    options: {codemode: false},
    async execute() { return {content: "Fixture probe complete."}; }
  }));
  return async () => { await tool.dispose(); };
}};'''

CHECKS = []
CLOSE_ERRORS = []
ACTIVE_PROCESS = None
PROVIDER_PORT = None


def check(check_id, condition, detail):
    CHECKS.append({"id": check_id, "status": "passed" if condition else "failed", "detail": detail})
    if not condition:
        raise AssertionError(f"{check_id}: {detail}")
    print("[OK] " + check_id, flush=True)


def untested(check_id, reason):
    CHECKS.append({"id": check_id, "status": "untested", "detail": reason})
    print("[SKIP] " + check_id + " - " + reason, flush=True)


def poll(label, read, accept=bool, timeout=20):
    end = time.monotonic() + BUDGET.cap(timeout)
    last = None
    while True:
        try:
            last = read()
            if accept(last):
                return last
        except (URLError, TimeoutError, OSError):
            pass
        if time.monotonic() >= end:
            raise AssertionError(f"Timed out waiting for {label}: {last!r}")
        time.sleep(0.1)


def make_environment(output, include_context_plugin, password):
    """A Windows whitelist; no host credential or daemon setting leaks in."""
    allowed = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "SYSTEMDRIVE",
               "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS", "TEMP", "TMP"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    for key, subdirectory in {"HOME": "home", "USERPROFILE": "home", "APPDATA": "roaming",
                              "LOCALAPPDATA": "local", "TEMP": "tmp", "TMP": "tmp",
                              "OPENCODE_CONFIG_DIR": "config",
                              "XDG_CONFIG_HOME": "xdg/config", "XDG_DATA_HOME": "xdg/data",
                              "XDG_CACHE_HOME": "xdg/cache", "XDG_STATE_HOME": "xdg/state"}.items():
        directory = output / subdirectory
        directory.mkdir(parents=True, exist_ok=True)
        env[key] = str(directory)
    plugins = [str((output / "fixture-plugin").resolve())]
    if include_context_plugin:
        plugins.append(str((output / "context-plugin").resolve()))
    provider_port = PROVIDER.server_port
    config = {
        "model": "fixture/fixture-model",
        "providers": {
            "fixture": {
                "name": "Mermaid fixture",
                "env": ["FIXTURE_API_KEY"],
                "package": "@opencode/ai/providers/openai-compatible",
                "settings": {
                    "apiKey": "fixture-only",
                    "baseURL": f"http://127.0.0.1:{provider_port}/v1",
                    "encoding": "none",
                },
                "models": {
                    "fixture-model": {
                        "name": "Fixture",
                        "limit": {"context": 128000, "output": 8192},
                        "capabilities": {"tools": True, "input": ["text"], "output": ["text"]},
                    },
                },
            },
        },
        "plugins": plugins,
        "permissions": [{"action": "*", "resource": "*", "effect": "allow"}],
        "update": "disable",
    }
    env.update({
        "OPENCODE_CONFIG_CONTENT": json.dumps(config),
        "OPENCODE_DB": str(output / "opencode.db"),
        "OPENCODE_SERVER_USERNAME": "opencode",
        "OPENCODE_SERVER_PASSWORD": password,
        "FIXTURE_API_KEY": "fixture-only",
        "OPENCODE_DISABLE_AUTOUPDATE": "1",
        "OPENCODE_DISABLE_FILEWATCHER": "1",
        "OPENCODE_DISABLE_PROJECT_CONFIG": "1",
        "OPENCODE_CONFIG_PROJECT_DISABLE": "1",
        "NO_PROXY": "127.0.0.1,localhost,::1",
    })
    return env


def execute(binary, output):
    work = output / "workspace"
    fixture = output / "fixture-plugin"
    plugin = output / "context-plugin"
    for directory in (work, fixture, plugin):
        directory.mkdir()
    shutil.copyfile(PLUGIN_SOURCE, plugin / "index.ts")
    (fixture / "index.ts").write_text(FIXTURE_PLUGIN, encoding="utf-8")

    global PROVIDER, PROVIDER_PORT
    PROVIDER = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    PROVIDER_PORT = PROVIDER.server_port
    threading.Thread(target=PROVIDER.serve_forever, daemon=True).start()

    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    password = uuid.uuid4().hex
    authorization = "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()
    base = f"http://127.0.0.1:{port}"
    query = "?" + urlencode({"location[directory]": str(work)})
    server = {"process": None}

    def api(method, path, value=None, allow_error=False, timeout=15):
        data = None if value is None else json.dumps(value).encode("utf-8")
        request = Request(base + path, data=data, method=method,
                          headers={"Authorization": authorization, "Content-Type": "application/json"})
        try:
            with HTTP.open(request, timeout=BUDGET.cap(timeout)) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except HTTPError as error:
            detail = error.read().decode(errors="replace")
            if allow_error:
                return {"status": error.code, "detail": detail[:500]}
            raise AssertionError(f"{method} {path} failed: {error.code} {detail[:500]}") from error

    def session_info(session_id):
        result = api("GET", f"/api/session/{session_id}" + query)
        return result.get("data", result) if isinstance(result, dict) else result

    def create_session(version=None):
        body = {"location": {"directory": str(work)}}
        if version is not None:
            body["metadata"] = {
                "source": "fintwind",
                "task": "mermaid-prompt-e2e-" + uuid.uuid4().hex,
                "fintwindMermaidPrompt": version,
            }
        result = api("POST", "/api/session", body)
        data = result.get("data", result) if isinstance(result, dict) else result
        return data["id"]

    def prompt(session_id, text, allow_error=False):
        return api("POST", f"/api/session/{session_id}/prompt" + query,
                   {"text": text}, allow_error=allow_error)

    def wait_ready():
        def read():
            process = server["process"]
            if process is None or not process.alive():
                raise AssertionError("Owned OpenCode process exited during startup.")
            try:
                return api("GET", "/api/info", timeout=3)
            except (URLError, TimeoutError, OSError):
                return None
        return poll("OpenCode startup", read, bool, timeout=45)

    def wait_plugin(plugin_id):
        return poll(f"{plugin_id} activation", lambda: api("GET", "/api/plugin" + query, allow_error=True),
                    lambda result: isinstance(result, dict) and any(
                        item.get("id") == plugin_id and item.get("state", {}).get("status") == "active"
                        for item in result.get("data", [])),
                    timeout=30)

    def start(include_context_plugin):
        global ACTIVE_PROCESS
        env = make_environment(output, include_context_plugin, password)
        process = runner.OwnedProcess(str(binary), ["serve", "--hostname", "127.0.0.1",
                                                    "--port", str(port)], work, env)
        ACTIVE_PROCESS = process
        server["process"] = process
        return process

    def stop():
        global ACTIVE_PROCESS
        process = server["process"]
        server["process"] = None
        ACTIVE_PROCESS = None
        if process is None:
            return
        try:
            process.close()
        except Exception as error:
            CLOSE_ERRORS.append(str(error))

    def turn(session_id, text, expected=1, mode="text", label=None, timeout=45):
        BRAIN.mode = mode
        start_records = len(BRAIN.snapshot())
        start_primary = BRAIN.primary_count
        prompt(session_id, text)
        if expected:
            poll(f"{label or text} provider request", lambda: BRAIN.primary_count,
                 lambda count: count >= start_primary + expected, timeout=timeout)
            BUDGET.sleep(0.5)
            window = BRAIN.snapshot()[start_records:]
            observed = [record for record in window if record["class"] == "primary"]
            if len(observed) != expected:
                raise AssertionError(
                    f"{label or text}: expected {expected} primary requests, observed {len(observed)}")
        else:
            BUDGET.sleep(2.0)
            window = BRAIN.snapshot()[start_records:]
            observed = [record for record in window if record["class"] == "primary"]
            if observed:
                raise AssertionError(f"{label or text}: unexpected primary request")
        for record in observed:
            if label:
                record["label"] = label
        BRAIN.mode = "text"
        return observed

    baseline_payload = None
    baseline_system = None
    baseline_tools = None
    v1_system = None
    fork_system = None
    off_restart_system = None
    v1_restart_system = None

    # Phase A: no context plugin. Capture the untouched request shape.
    start(False)
    wait_ready()
    wait_plugin("mermaid.fixture")
    old = create_session(None)
    baseline_records = turn(old, "Baseline turn 1.", label="off-baseline")
    baseline = baseline_records[0]
    baseline_payload = baseline["payload"]
    baseline_system = baseline["systemSha256"]
    baseline_tools = baseline["toolsSha256"]
    check("plugin_absent_baseline_captured",
          baseline["instructionCount"] == 0 and baseline["class"] == "primary",
          {"systemSha256": baseline_system, "toolsSha256": baseline_tools})
    stop()

    # Phase B: the real context plugin is loaded against the same database.
    start(True)
    wait_ready()
    wait_plugin("mermaid.fixture")
    activated = wait_plugin("fintwind.context")
    check("context_plugin_active",
          any(item.get("id") == "fintwind.context"
              for item in (activated or {}).get("data", [])),
          "fintwind.context reports active")

    old_info = session_info(old)
    old_metadata = old_info.get("metadata") or {}
    check("old_session_metadata_absent", "fintwindMermaidPrompt" not in old_metadata, old_metadata)

    off_records = turn(old, "Baseline turn 2.", label="off")
    off_records += turn(old, "Baseline turn 3.", label="off")
    check("off_session_multi_turn_no_instruction",
          all(record["instructionCount"] == 0 and record["markerCount"] == 0 for record in off_records),
          [{"instructionCount": record["instructionCount"],
            "markerCount": record["markerCount"]} for record in off_records])
    check("off_session_system_bytes_unchanged",
          all(record["systemSha256"] == baseline_system for record in off_records),
          {"baseline": baseline_system, "observed": sorted({r["systemSha256"] for r in off_records})})
    check("off_session_tools_schema_unchanged",
          all(record["toolsSha256"] == baseline_tools for record in off_records),
          {"baseline": baseline_tools, "observed": sorted({r["toolsSha256"] for r in off_records})})
    phase_b_messages = off_records[0]["payload"].get("messages") or []
    phase_a_messages = baseline_payload.get("messages") or []
    phase_b_history = [message for message in phase_b_messages if message.get("role") != "system"]
    phase_a_history = [message for message in phase_a_messages if message.get("role") != "system"]
    check("off_session_history_prefix_unchanged",
          phase_b_history[:len(phase_a_history)] == phase_a_history,
          {"baselineMessages": len(phase_a_history), "observedMessages": len(phase_b_history)})

    v1 = create_session("v1")
    v1_info = session_info(v1)
    v1_metadata = v1_info.get("metadata") or {}
    check("v1_session_metadata_echoed", v1_metadata.get("fintwindMermaidPrompt") == "v1", v1_metadata)

    v1_records = turn(v1, "V1 turn 1.", label="v1")
    v1_records += turn(v1, "V1 turn 2.", label="v1")
    probe_records = turn(v1, "V1 probe turn.", expected=2, mode="probe", label="v1-probe")
    v1_all = v1_records + probe_records
    v1_system = v1_all[0]["systemSha256"]
    check("v1_each_request_instruction_once",
          all(record["instructionCount"] == 1 and record["markerCount"] == 1 for record in v1_all),
          [{"instructionCount": record["instructionCount"],
            "markerCount": record["markerCount"]} for record in v1_all])
    check("v1_system_bytes_constant",
          all(record["systemSha256"] == v1_system for record in v1_all),
          {"expected": v1_system, "observed": sorted({r["systemSha256"] for r in v1_all})})
    off_system_text = system_text(off_records[0]["payload"])
    v1_system_text = system_text(v1_all[0]["payload"])
    check("v1_adds_only_instruction_to_off_system",
          " ".join(v1_system_text.replace(MERMAID_PROMPT_V1, "").split())
          == " ".join(off_system_text.split())
          and v1_system_text.count(MERMAID_PROMPT_V1) == 1,
          {"offLen": len(off_system_text), "v1Len": len(v1_system_text)})
    check("v1_tools_schema_unchanged",
          all(record["toolsSha256"] == baseline_tools for record in v1_all),
          {"baseline": baseline_tools, "observed": sorted({r["toolsSha256"] for r in v1_all})})
    check("v1_tool_continuation_instruction_once",
          len(probe_records) == 2
          and probe_records[0]["toolCall"]
          and not probe_records[1]["toolCall"]
          and all(record["instructionCount"] == 1 for record in probe_records),
          {"requests": len(probe_records), "toolCalls": [r["toolCall"] for r in probe_records],
           "instructionCounts": [r["instructionCount"] for r in probe_records]})

    plain = create_session(None)
    plain_records = turn(plain, "Plain turn 1.", label="off-plain")
    plain_records += turn(plain, "Plain turn 2.", label="off-plain")
    check("plain_session_off",
          all(record["instructionCount"] == 0 and record["systemSha256"] == baseline_system
              and record["toolsSha256"] == baseline_tools for record in plain_records),
          [{"instructionCount": r["instructionCount"], "systemSha256": r["systemSha256"]}
           for r in plain_records])

    # An independent preference file is not read by the plugin. Change it in
    # both directions and require exact request hashes to survive.
    decoy = output / "independent-preference" / "fintwind-global-preference.json"
    decoy.parent.mkdir(parents=True, exist_ok=True)
    decoy.write_text(json.dumps({"mermaidPrompt": "off"}), encoding="utf-8")
    old_decoy = turn(old, "Baseline with independent preference off.", label="off-decoy")
    check("global_preference_file_ignored_off",
          old_decoy[0]["instructionCount"] == 0
          and old_decoy[0]["systemSha256"] == baseline_system
          and old_decoy[0]["toolsSha256"] == baseline_tools,
          {"systemSha256": old_decoy[0]["systemSha256"], "toolsSha256": old_decoy[0]["toolsSha256"]})
    decoy.write_text(json.dumps({"mermaidPrompt": "v1"}), encoding="utf-8")
    v1_decoy = turn(v1, "V1 with independent preference on.", label="v1-decoy")
    check("global_preference_file_ignored_v1",
          v1_decoy[0]["instructionCount"] == 1
          and v1_decoy[0]["systemSha256"] == v1_system
          and v1_decoy[0]["toolsSha256"] == baseline_tools,
          {"systemSha256": v1_decoy[0]["systemSha256"], "toolsSha256": v1_decoy[0]["toolsSha256"]})

    fork_result = api("POST", f"/api/session/{v1}/fork", {})
    fork_data = fork_result.get("data", fork_result) if isinstance(fork_result, dict) else fork_result
    fork_id = fork_data.get("id") if isinstance(fork_data, dict) else None
    check("fork_returns_session", isinstance(fork_id, str) and bool(fork_id), {"id": fork_id})
    fork_info = session_info(fork_id)
    fork_metadata = (fork_info or {}).get("metadata") or {}
    check("fork_inherits_metadata", fork_metadata.get("fintwindMermaidPrompt") == "v1", fork_metadata)
    fork_records = turn(fork_id, "Fork turn 1.", label="v1-fork")
    fork_system = fork_records[0]["systemSha256"]
    check("fork_keeps_instruction_and_tools",
          fork_records[0]["instructionCount"] == 1
          and fork_system == v1_system
          and fork_records[0]["toolsSha256"] == baseline_tools,
          {"systemSha256": fork_system, "toolsSha256": fork_records[0]["toolsSha256"]})

    # Unknown versions must fail the request, not silently omit the instruction.
    unknown = create_session("v2")
    unknown_start = len(BRAIN.snapshot())
    unknown_response = prompt(unknown, "Unknown version turn.", allow_error=True)
    rejected = isinstance(unknown_response, dict) and "status" in unknown_response
    BUDGET.sleep(2.5)
    unknown_window = BRAIN.snapshot()[unknown_start:]
    unknown_primary = [record for record in unknown_window if record["class"] == "primary"]
    check("unknown_version_dispatches_nothing",
          not unknown_primary,
          {"promptRejected": rejected, "primaryRequests": len(unknown_primary),
           "responseStatus": unknown_response.get("status") if isinstance(unknown_response, dict) else None})

    stop()

    # Phase C: same private database and plugin path, fresh process.
    start(True)
    wait_ready()
    wait_plugin("mermaid.fixture")
    wait_plugin("fintwind.context")
    off_restart = turn(old, "Restart baseline turn.", label="off-restart")
    off_restart_system = off_restart[0]["systemSha256"]
    check("restart_off_preference_preserved",
          off_restart[0]["instructionCount"] == 0
          and off_restart_system == baseline_system
          and off_restart[0]["toolsSha256"] == baseline_tools,
          {"systemSha256": off_restart_system, "toolsSha256": off_restart[0]["toolsSha256"]})
    v1_restart = turn(v1, "Restart V1 turn.", label="v1-restart")
    v1_restart_system = v1_restart[0]["systemSha256"]
    check("restart_v1_preference_preserved",
          v1_restart[0]["instructionCount"] == 1
          and v1_restart_system == v1_system
          and v1_restart[0]["toolsSha256"] == baseline_tools,
          {"systemSha256": v1_restart_system, "toolsSha256": v1_restart[0]["toolsSha256"]})

    untested("lookup_failure_not_silent",
             "No black-box way exists to make ctx.session.get reject without patching the plugin; "
             "the same rejection path is exercised by the unknown-version request, which dispatched nothing.")
    untested("compaction_preference_rebuild",
             "Optional in the brief; this runner does not force a compaction turn.")


def capture_summary():
    records = BRAIN.records
    primary = [record for record in records if record["class"] == "primary"]
    auxiliary = [record for record in records if record["class"] == "auxiliary"]
    counts = {"0": 0, "1": 0, "more": 0}
    for record in primary:
        key = "0" if record["instructionCount"] == 0 else ("1" if record["instructionCount"] == 1 else "more")
        counts[key] += 1
    by_label = {}
    for record in primary:
        label = record.get("label", "unlabelled")
        entry = by_label.setdefault(label, {"primary": 0, "instructionCounts": {}})
        entry["primary"] += 1
        key = str(record["instructionCount"])
        entry["instructionCounts"][key] = entry["instructionCounts"].get(key, 0) + 1
    return {
        "total": len(records),
        "primary": len(primary),
        "auxiliary": len(auxiliary),
        "auxiliaryWithInstruction": sum(1 for record in auxiliary if record["instructionCount"] > 0),
        "primaryInstructionCounts": counts,
        "byLabel": by_label,
    }


def unique_hashes(records, field):
    return sorted({record[field] for record in records})


def main():
    global BUDGET, PROVIDER, PROVIDER_PORT, ACTIVE_PROCESS
    parser = argparse.ArgumentParser(description="Real OpenCode V2 Mermaid prompt E2E.")
    parser.add_argument("--opencode", help="Absolute path to a known OpenCode executable.")
    parser.add_argument("--deadline", type=float, default=180.0,
                        help="Total wall-clock deadline in seconds (default 180).")
    args = parser.parse_args()

    run_id = str(uuid.uuid4())
    output = ROOT / "target" / "mermaid-prompt-e2e" / run_id
    output.mkdir(parents=True, exist_ok=True)
    report = {
        "schemaVersion": 1,
        "runId": run_id,
        "surface": "real OpenCode V2 server + loopback OpenAI-compatible provider",
        "nativeUI": "not exercised",
        "provider": "loopback chat completions; no real model or network endpoint",
        "deadlineSeconds": args.deadline,
    }
    status = "failed"
    error_text = None
    PROVIDER = None
    PROVIDER_PORT = None
    started = time.monotonic()

    if not args.opencode:
        error_text = "--opencode is required"
    elif not Path(args.opencode).is_absolute():
        error_text = "--opencode must be an absolute path"
    elif not Path(args.opencode).is_file():
        error_text = "--opencode does not name an existing file"
    elif args.deadline < 30:
        error_text = "--deadline must be at least 30 seconds"
    else:
        BUDGET = Budget(args.deadline)
        try:
            execute(Path(args.opencode), output)
            status = "passed"
        except Exception as error:
            error_text = f"{type(error).__name__}: {error}"
            print("[FAIL] " + error_text, flush=True)
        finally:
            if ACTIVE_PROCESS is not None:
                try:
                    ACTIVE_PROCESS.close()
                except Exception as error:
                    CLOSE_ERRORS.append(str(error))
                ACTIVE_PROCESS = None
            if PROVIDER is not None:
                try:
                    PROVIDER.shutdown()
                    PROVIDER.server_close()
                except Exception as error:
                    CLOSE_ERRORS.append(str(error))

    if error_text is not None:
        status = "failed"
    if CLOSE_ERRORS:
        status = "failed"
        error_text = (error_text + "; " if error_text else "") + "cleanup: " + "; ".join(CLOSE_ERRORS)

    primary = [record for record in BRAIN.records if record["class"] == "primary"]
    report.update({
        "status": status,
        "error": error_text,
        "checks": CHECKS,
        "captures": capture_summary(),
        "systemPromptSha256": {
            "off": unique_hashes([r for r in primary if str(r.get("label", "")).startswith("off")
                                  and r["instructionCount"] == 0], "systemSha256"),
            "v1": unique_hashes([r for r in primary if str(r.get("label", "")).startswith("v1")
                                 and r["instructionCount"] == 1], "systemSha256"),
        },
        "toolsSchemaSha256": unique_hashes(primary, "toolsSha256"),
        "plugin": {
            "resource": str(PLUGIN_SOURCE.relative_to(ROOT)).replace("\\", "/"),
            "sourceSha256": sha256(PLUGIN_SOURCE.read_text(encoding="utf-8")),
        },
        "notTested": [entry["id"] for entry in CHECKS if entry["status"] == "untested"],
        "randomized": {"runId": run_id, "serverPort": PROVIDER_PORT},
        "artifacts": {
            "report": str(output / "report.json"),
            "workspace": str(output / "workspace"),
            "plugin": str(output / "context-plugin"),
            "database": str(output / "opencode.db"),
        },
        "elapsedSeconds": round(time.monotonic() - started, 1),
    })
    (output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(("Report: " + str(output / "report.json")), flush=True)
    return 0 if status == "passed" else 1


if __name__ == "__main__":
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("goal_e2e", ROOT / "scripts" / "goal-opencode-e2e.py")
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    sys.exit(main())
