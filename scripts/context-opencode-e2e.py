"""Check context composition through an isolated OpenCode and a local provider."""

import base64
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import time
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import URLError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener
import uuid


ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "target" / "context-opencode-e2e" / str(uuid.uuid4())
spec = importlib.util.spec_from_file_location("goal_runner", ROOT / "scripts/goal-opencode-e2e.py")
runner = importlib.util.module_from_spec(spec)
sys.dont_write_bytecode = True
spec.loader.exec_module(runner)
HTTP = build_opener(ProxyHandler({}))


class Provider(BaseHTTPRequestHandler):
    calls = []

    def log_message(self, *_args):
        pass

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.calls.append(payload)
        common = {"id": "chatcmpl-context", "created": 0, "model": "fixture-model"}
        usage = {"prompt_tokens": 1000, "completion_tokens": 10, "total_tokens": 1010}
        chunks = [
            {**common, "object": "chat.completion.chunk", "choices": [
                {"index": 0, "delta": {"role": "assistant", "content": "Fixture complete."}, "finish_reason": None}]},
            {**common, "object": "chat.completion.chunk", "usage": usage, "choices": [
                {"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        if payload.get("stream"):
            data = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks)
                    + "data: [DONE]\n\n").encode()
            content_type = "text/event-stream"
        else:
            data = json.dumps({**common, "object": "chat.completion", "usage": usage, "choices": [
                {"index": 0, "message": {"role": "assistant", "content": "Fixture complete."}, "finish_reason": "stop"}]}).encode()
            content_type = "application/json"
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    binary = shutil.which("opencode")
    if not binary:
        raise RuntimeError("OpenCode must be available on PATH.")
    OUTPUT.mkdir(parents=True)
    work, fixture, plugin = [OUTPUT / name for name in ("workspace", "fixture", "context")]
    for directory in (work, fixture, plugin):
        directory.mkdir()
    shutil.copyfile(ROOT / "resources/opencode-context-plugin.ts", plugin / "index.ts")
    (fixture / "index.ts").write_text('''export default { id: "context.fixture", async setup(ctx) {
      const tool = await ctx.tool.transform(editor => editor.add({
        name: "fixture_read", description: "Read", input: {type: "object", properties: {}},
        options: {codemode: false}, execute: async () => ({content: "fixture"})
      }));
      const hook = await ctx.session.hook("context", event => {
        const text = JSON.stringify(event.messages);
        const original = event.messages;
        event.system = [{type: "text", text: "abcdefgh"}];
        event.messages = [{role: "user", content: [
          {type: "text", text: text.includes("longer-fixture") ? "abcdefghijklmnop" : "abcd"}
        ]}];
        if (text.includes("opaque-fixture")) event.messages.push({role: "assistant", content: [
          {type: "reasoning", text: "", providerMetadata: {openai: {reasoningEncryptedContent: "not-a-token-count"}}}
        ]});
        if (text.includes("all-categories-fixture")) {
          const summary = original.find(message => message.content.some(part => part.type === "text" && part.text.startsWith("<conversation-checkpoint>")));
          event.messages.push(
            {role: "assistant", content: [
              {type: "reasoning", text: "abcdefgh", providerMetadata: {deepseek: {reasoningField: "reasoning_content"}}},
              {type: "tool-call", id: "read-1", name: "read", input: {path: "fixture.txt"}},
              {type: "tool-call", id: "shell-1", name: "shell", input: {command: "echo fixture"}}
            ]},
            {role: "tool", content: [
              {type: "tool-result", id: "read-1", name: "read", result: {type: "text", value: "abcdefghijkl"}},
              {type: "tool-result", id: "shell-1", name: "shell", result: {type: "content", value: [{type: "text", text: "abcdefgh"}]}}
            ]}
          );
          if (!summary) throw new Error("Missing official summary projection");
          event.messages.unshift(summary);
        }
        event.tools = {fixture_read: {description: "Read", input: {type: "object", properties: {}}}};
      });
      return async () => { await hook.dispose(); await tool.dispose(); };
    }};''', encoding="utf-8")
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    config = {"model": "fixture/fixture-model", "small_model": "fixture/fixture-model",
              "plugins": [str(fixture), str(plugin)],
              "provider": {"fixture": {"npm": "@ai-sdk/openai-compatible", "name": "Context fixture",
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
    query = "?" + urlencode({"location[directory]": str(work)})
    child = None

    def api(method, path, value=None):
        request = Request(f"http://127.0.0.1:{port}" + path,
                          data=None if value is None else json.dumps(value).encode(), method=method,
                          headers={"Authorization": authorization, "Content-Type": "application/json"})
        with HTTP.open(request, timeout=15) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    def snapshot(session_id):
        return api("POST", "/api/rpc/fintwind.context/get" + query,
                   {"input": {"sessionID": session_id}})["output"]

    def ready():
        if not child.alive():
            raise AssertionError("Owned OpenCode process exited during startup.")
        try:
            return api("GET", "/api/info")
        except (URLError, TimeoutError):
            return None

    def start():
        return runner.OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)

    try:
        child = start()
        runner.wait_for("isolated OpenCode startup", ready, timeout=45)
        sessions = [api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"] for _ in range(3)]
        runner.wait_for("context plugin activation", lambda: api("GET", "/api/plugin" + query),
                        lambda result: any(item.get("id") == "fintwind.context" and item.get("state", {}).get("status") == "active"
                                           for item in result.get("data", [])))
        assert snapshot(sessions[0]) is None
        observed = []
        for session_id, prompt in zip(sessions, ("short-fixture", "longer-fixture", "opaque-fixture")):
            api("POST", f"/api/session/{session_id}/prompt" + query, {"text": prompt})
            observed.append(runner.wait_for("captured context", lambda: snapshot(session_id)))
        assert observed[0]["model"] == "fixture/fixture-model", observed
        assert observed[0]["system"] == 2, observed
        assert observed[0]["memory"] == 0, observed
        assert observed[0]["messages"] == 1, observed
        assert observed[1]["messages"] == 4, observed
        assert observed[0]["tools"] > 0 and observed[0]["incomplete"] is False, observed
        assert observed[0]["version"] == 2, observed
        assert all(observed[0][key] == 0 for key in ("reasoning", "toolCalls", "files", "toolResults")), observed
        assert observed[2] == {**observed[0], "incomplete": True, "opaqueReasoning": True}, observed
        assert snapshot(sessions[0]) == observed[0], "Other sessions or auxiliary requests replaced the snapshot."
        runner.wait_for("primary provider request", lambda: any(
            "abcdefgh" in json.dumps(call) and "fixture_read" in json.dumps(call) for call in Provider.calls))
        # Exercise a genuine projected compaction, not a guessed text marker.
        compacted = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        with sqlite3.connect(OUTPUT / "opencode.db") as database:
            now = int(time.time() * 1000)
            data = {"time": {"created": now}, "status": "completed", "reason": "manual",
                    "model": {"providerID": "fixture", "id": "fixture-model"}, "summary": "abcdefgh"}
            database.execute("INSERT INTO session_message(id,session_id,type,seq,time_created,time_updated,data) VALUES(?,?,?,?,?,?,?)",
                             ("msg_" + uuid.uuid4().hex, compacted, "compaction", 1, now, now, json.dumps(data)))
        api("POST", f"/api/session/{compacted}/prompt" + query, {"text": "all-categories-fixture"})
        detailed = runner.wait_for("detailed context", lambda: snapshot(compacted))
        assert detailed["system"] == 2 and detailed["messages"] == 1 and detailed["reasoning"] == 2, detailed
        assert detailed["files"] == 3 and detailed["toolResults"] == 2, detailed
        assert detailed["tools"] == observed[0]["tools"] and detailed["toolCalls"] > 0, detailed
        assert detailed["memory"] > 0 and detailed["incomplete"] is False, detailed
        observed.append(detailed)
        child.close()
        child = None
        child = start()
        runner.wait_for("isolated OpenCode restart", ready, timeout=45)
        api("GET", f"/api/session/{sessions[0]}" + query)
        runner.wait_for("context plugin reactivation", lambda: api("GET", "/api/plugin" + query),
                        lambda result: any(item.get("id") == "fintwind.context" and item.get("state", {}).get("status") == "active"
                                           for item in result.get("data", [])))
        assert snapshot(sessions[0]) == observed[0], "Context snapshot did not survive restart."
        report = {"passed": ["assembled semantic categories", "provider request preserved", "session isolation",
                             "opaque payload stays uncounted", "auxiliary requests excluded", "snapshot survives restart", "definitions separated from calls and results", "file and shell results separated", "reasoning separated from dialogue", "official compaction summary provenance"], "snapshots": observed}
        (OUTPUT / "result.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print(json.dumps(report, ensure_ascii=False))
    finally:
        (OUTPUT / "requests.json").write_text(json.dumps(Provider.calls, ensure_ascii=False, indent=2), encoding="utf-8")
        if child is not None:
            child.close()
        provider.shutdown()
        provider.server_close()


if __name__ == "__main__":
    main()
