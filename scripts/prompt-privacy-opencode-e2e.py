"""Run a private OpenCode server against a loopback-only provider fixture."""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import socket
import struct
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import URLError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener
import uuid


ROOT = Path(__file__).resolve().parent.parent
OwnedProcess = runpy.run_path(str(ROOT / "scripts" / "goal-opencode-e2e.py"))["OwnedProcess"]
HTTP = build_opener(ProxyHandler({}))
SECRET = "fixture-" + uuid.uuid4().hex
REF = re.compile(r"FINTWIND_SECRET_[A-F0-9]{24}")
CALLS = []


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        if self.headers.get("Upgrade", "").lower() != "websocket":
            self.send_error(404)
            return
        key = self.headers["Sec-WebSocket-Key"]
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.connection.settimeout(15)

        def read_exact(length):
            data = b""
            while len(data) < length:
                chunk = self.rfile.read(length - len(data))
                if not chunk:
                    raise EOFError("WebSocket closed")
                data += chunk
            return data

        def send_frame(text):
            data = text.encode()
            length = len(data)
            header = bytes([0x81, length]) if length < 126 else bytes([0x81, 126]) + struct.pack("!H", length)
            self.wfile.write(header + data)
            self.wfile.flush()

        try:
            while True:
                head = read_exact(2)
                opcode = head[0] & 15
                size = head[1] & 127
                if size == 126:
                    size = struct.unpack("!H", read_exact(2))[0]
                elif size == 127:
                    size = struct.unpack("!Q", read_exact(8))[0]
                mask = read_exact(4) if head[1] & 128 else None
                data = read_exact(size)
                if mask:
                    data = bytes(value ^ mask[index % 4] for index, value in enumerate(data))
                if opcode == 8:
                    break
                if opcode != 1:
                    continue
                payload = json.loads(data)
                serialized = json.dumps(payload)
                match = REF.search(serialized)
                has_result = "function_call_output" in serialized
                CALLS.append({"exposed": SECRET in serialized, "reference": match.group() if match else None,
                              "tool_result": has_result, "websocket": True})
                text = "result " + match.group() if match else "fixture title"
                called = "fixture ws tool" in serialized and "privacy_fixture_capture" in serialized and match
                events = self.responses_tool_events(match.group()) if called and not has_result else self.responses_events(text)
                for event in events:
                    send_frame(json.dumps(event))
        except (EOFError, OSError, TimeoutError):
            pass

    def responses_events(self, text):
        response_id = "resp_" + uuid.uuid4().hex
        item_id = "msg_" + uuid.uuid4().hex
        item = {"id": item_id, "type": "message", "status": "completed", "role": "assistant",
                "content": [{"type": "output_text", "text": text, "annotations": []}]}
        return [
            {"type": "response.created", "response": {"id": response_id, "status": "in_progress", "output": []}},
            {"type": "response.output_item.added", "output_index": 0, "item": {**item, "status": "in_progress", "content": []}},
            {"type": "response.content_part.added", "item_id": item_id, "output_index": 0, "content_index": 0,
             "part": {"type": "output_text", "text": "", "annotations": []}},
            {"type": "response.output_text.delta", "item_id": item_id, "output_index": 0, "content_index": 0, "delta": text[:15]},
            {"type": "response.output_text.delta", "item_id": item_id, "output_index": 0, "content_index": 0, "delta": text[15:]},
            {"type": "response.output_text.done", "item_id": item_id, "output_index": 0, "content_index": 0, "text": text},
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": {"id": response_id, "status": "completed", "output": [item],
             "usage": {"input_tokens": 10, "output_tokens": 10, "total_tokens": 20}}},
        ]

    def responses_tool_events(self, reference):
        response_id = "resp_" + uuid.uuid4().hex
        item_id = "fc_" + uuid.uuid4().hex
        arguments = json.dumps({"value": reference})
        item = {"id": item_id, "type": "function_call", "status": "completed", "call_id": "call_ws_fixture",
                "name": "privacy_fixture_capture", "arguments": arguments}
        return [
            {"type": "response.created", "response": {"id": response_id, "status": "in_progress", "output": []}},
            {"type": "response.output_item.added", "output_index": 0, "item": {**item, "status": "in_progress", "arguments": ""}},
            {"type": "response.function_call_arguments.delta", "item_id": item_id, "output_index": 0, "delta": arguments[:20]},
            {"type": "response.function_call_arguments.delta", "item_id": item_id, "output_index": 0, "delta": arguments[20:]},
            {"type": "response.function_call_arguments.done", "item_id": item_id, "output_index": 0, "arguments": arguments},
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": {"id": response_id, "status": "completed", "output": [item],
             "usage": {"input_tokens": 10, "output_tokens": 10, "total_tokens": 20}}},
        ]

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        body = json.dumps(payload)
        match = REF.search(body)
        text = "result " + match.group() if match else "fixture title"
        if self.path.split("?", 1)[0].endswith(":streamGenerateContent"):
            CALLS.append({"exposed": SECRET in body, "reference": match.group() if match else None,
                          "tool_result": False, "gemini": True})
            events = [
                {"candidates": [{"index": 0, "content": {"role": "model", "parts": [{"text": text[:15]}]}}]},
                {"candidates": [{"index": 0, "content": {"role": "model", "parts": [{"text": text[15:]}]}, "finishReason": "STOP"}],
                 "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 10, "totalTokenCount": 20}},
            ]
            data = "".join("data: " + json.dumps(item) + "\n\n" for item in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        if self.path.endswith("/messages"):
            signed_blocks = [part for message in payload.get("messages", [])
                             for part in (message.get("content") if isinstance(message.get("content"), list) else [])
                             if isinstance(part, dict) and part.get("type") == "thinking"]
            CALLS.append({"exposed": SECRET in body, "reference": match.group() if match else None,
                          "tool_result": False, "anthropic": True,
                          "signed_ok": any(block.get("thinking") == "think " + match.group()
                                           and block.get("signature") == "fixture-signature"
                                           for block in signed_blocks) if match else False})
            thinking = "think " + match.group() if match else "ordinary thought"
            events = [
                {"type": "message_start", "message": {"id": "msg_fixture", "type": "message", "role": "assistant",
                 "content": [], "model": "fixture-model", "stop_reason": None, "stop_sequence": None,
                 "usage": {"input_tokens": 10, "output_tokens": 0}}},
                {"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}},
                {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": thinking[:15]}},
                {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": thinking[15:]}},
                {"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "fixture-signature"}},
                {"type": "content_block_stop", "index": 0},
                {"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}},
                {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": text}},
                {"type": "content_block_stop", "index": 1},
                {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                 "usage": {"output_tokens": 10}},
                {"type": "message_stop"},
            ]
            data = "".join("event: " + item["type"] + "\ndata: " + json.dumps(item) + "\n\n" for item in events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        if self.path.endswith("/responses"):
            CALLS.append({"exposed": SECRET in body, "reference": match.group() if match else None,
                          "tool_result": False, "responses": True})
            events = self.responses_events(text)
            data = ("".join("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n" for event in events)
                    + "data: [DONE]\n\n").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        common = {"id": "chatcmpl-fixture", "created": 0, "model": "fixture-model", "object": "chat.completion.chunk"}
        tool_enabled = any(tool.get("function", {}).get("name") == "privacy_fixture_capture" for tool in payload.get("tools", []))
        child_enabled = any(tool.get("function", {}).get("name") == "privacy_fixture_child" for tool in payload.get("tools", []))
        tool_result = any(message.get("role") == "tool" for message in payload.get("messages", []))
        prefix = {"system": [message for message in payload.get("messages", []) if message.get("role") == "system"],
                  "tools": payload.get("tools", [])}
        CALLS.append({"contained_original": SECRET in body, "reference": match.group() if match else None,
                      "tool_result": tool_result, "prefix_hash": hashlib.sha256(json.dumps(prefix, sort_keys=True).encode()).hexdigest()})
        if "You MUST summarize" in body and match:
            text = ("## Objective\n- " + match.group() + "\n\n## Requirements\n- (none)\n\n"
                    "## Decisions\n- (none)\n\n## Work State\n### Completed\n- (none)\n\n"
                    "### Active\n- Fixture task\n\n### Blocked\n- (none)\n\n"
                    "## Next Move\n1. Continue\n\n## Relevant Files\n- (none)\n\n"
                    "## Important Context\n- (none)")
        child_requested = "fixture child request" in body and child_enabled and match and not tool_result
        tool_requested = ("fixture tool request" in body and tool_enabled or child_requested) and match and not tool_result
        if tool_requested:
            arguments = json.dumps({"value": match.group()})
            deltas = [{"role": "assistant", "tool_calls": [{"index": 0, "id": "call_fixture", "type": "function",
                       "function": {"name": "privacy_fixture_child" if child_requested else "privacy_fixture_capture", "arguments": arguments[:20]}}]},
                      {"tool_calls": [{"index": 0, "function": {"arguments": arguments[20:]}}]}]
            reason = "tool_calls"
        else:
            deltas = [{"role": "assistant", "content": text[:12]}, {"content": text[12:]}]
            reason = "stop"
        chunks = [
            *[{**common, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]} for delta in deltas],
            {**common, "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
             "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}},
        ]
        if payload.get("stream"):
            data = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
            content_type = "text/event-stream"
        else:
            data = json.dumps({"id": "chatcmpl-fixture", "created": 0, "model": "fixture-model",
                               "object": "chat.completion", "choices": [{"index": 0,
                               "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                               "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}}).encode()
            content_type = "application/json"
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    binary = shutil.which("opencode")
    if not binary:
        raise RuntimeError("OpenCode not found on PATH")
    output = Path(os.environ.get("TEMP", ".")) / ("fintwind-privacy-opencode-" + uuid.uuid4().hex)
    output.mkdir(parents=True)
    work = output / "workspace"
    work.mkdir()
    plugin = output / "plugin"
    plugin.mkdir()
    for source, target in [("opencode-prompt-privacy-plugin.ts", "index.ts"),
                           ("prompt-privacy-detect.ts", "prompt-privacy-detect.ts"),
                           ("prompt-privacy-rules.json", "prompt-privacy-rules.json")]:
        shutil.copyfile(ROOT / "resources" / source, plugin / target)
    tool_plugin = output / "tool-plugin"
    tool_plugin.mkdir()
    captured = output / "captured.txt"
    child_path = output / "child-session.txt"
    (tool_plugin / "index.ts").write_text('''import { writeFile } from "node:fs/promises";
      export default { id: "privacy.fixture", async setup(ctx) {
        const registration = await ctx.tool.transform(editor => { editor.add({
          name: "privacy_fixture_capture", description: "Capture fixture input.",
          input: {type:"object",properties:{value:{type:"string"}},required:["value"]}, options:{codemode:false},
          async execute(input) { await writeFile(%s, input.value); return {content: input.value}; }
        }); editor.add({name: "privacy_fixture_child", description: "Launch a child session.",
          input: {type:"object",properties:{value:{type:"string"}},required:["value"]}, options:{codemode:false},
          async execute(input, context) {
            const child = await ctx.session.create({parentID: context.sessionID, title: "Privacy child"});
            await writeFile(%s, child.id);
            await ctx.session.prompt({sessionID: child.id, text: `child work password="${input.value}"`});
            return {content: "child started"};
          }
        }); });
        return async () => { await registration.dispose(); };
      }};''' % (json.dumps(str(captured)), json.dumps(str(child_path))), encoding="utf-8")
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=provider.serve_forever, daemon=True)
    thread.start()
    config = {"model": "fixture/fixture-model", "small_model": "fixture/fixture-model", "plugins": [str(plugin), str(tool_plugin)],
              "permission": {"privacy_fixture_capture": "allow", "privacy_fixture_child": "allow"},
              "providers": {"fixture-responses": {"name": "Responses fixture", "env": ["FIXTURE_RESPONSE_KEY"],
                "package": "@opencode/ai/providers/openai", "settings": {"baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                "models": {"fixture-model": {"name": "Fixture", "limit": {"context": 128000, "output": 8192}}}},
                "fixture-websocket": {"name": "WebSocket fixture", "env": ["FIXTURE_RESPONSE_KEY"],
                "package": "@opencode/ai/providers/openai", "settings": {"baseURL": f"http://127.0.0.1:{provider.server_port}/v1", "transport": "websocket"},
                "models": {"fixture-model": {"name": "Fixture", "limit": {"context": 128000, "output": 8192}}}},
                "fixture-anthropic": {"name": "Anthropic fixture", "env": ["FIXTURE_RESPONSE_KEY"],
                "package": "@opencode/ai/providers/anthropic", "settings": {"baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                "models": {"fixture-model": {"name": "Fixture", "reasoning": True, "limit": {"context": 128000, "output": 8192}}}},
                "fixture-gemini": {"name": "Gemini fixture", "env": ["FIXTURE_RESPONSE_KEY"],
                "package": "@opencode/ai/providers/google", "settings": {"baseURL": f"http://127.0.0.1:{provider.server_port}/v1beta"},
                "models": {"fixture-model": {"name": "Fixture", "limit": {"context": 128000, "output": 8192}}}}},
              "provider": {"fixture": {"npm": "@ai-sdk/openai-compatible", "name": "Privacy fixture",
                           "options": {"apiKey": "fixture-key", "baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                           "models": {"fixture-model": {"name": "Fixture", "tool_call": True,
                                      "limit": {"context": 128000, "output": 8192}}}}}}
    allowed = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    for key, subdir in {"HOME": "home", "USERPROFILE": "home", "APPDATA": "roaming", "LOCALAPPDATA": "local",
                        "TEMP": "tmp", "TMP": "tmp", "OPENCODE_CONFIG_DIR": "config", "XDG_CONFIG_HOME": "xdg/config",
                        "XDG_DATA_HOME": "xdg/data", "XDG_CACHE_HOME": "xdg/cache", "XDG_STATE_HOME": "xdg/state"}.items():
        directory = output / subdir
        directory.mkdir(parents=True, exist_ok=True)
        env[key] = str(directory)
    password = uuid.uuid4().hex
    env.update({"FIXTURE_RESPONSE_KEY": "fixture-key", "OPENCODE_CONFIG_CONTENT": json.dumps(config), "OPENCODE_DB": str(output / "opencode.db"),
                "FINTWIND_PROMPT_PRIVACY_KEY_PATH": str(output / "privacy.key"),
                "OPENCODE_SERVER_USERNAME": "opencode", "OPENCODE_SERVER_PASSWORD": password,
                "OPENCODE_DISABLE_AUTOUPDATE": "1", "OPENCODE_DISABLE_FILEWATCHER": "1",
                "OPENCODE_DISABLE_PROJECT_CONFIG": "1", "OPENCODE_CONFIG_PROJECT_DISABLE": "1",
                "NO_PROXY": "127.0.0.1,localhost,::1"})
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    env["FINTWIND_OPENCODE_SERVER_PORT"] = str(port)
    authorization = "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()
    base = f"http://127.0.0.1:{port}"
    query = "?" + urlencode({"location[directory]": str(work)})

    def api(method, path, value=None):
        data = None if value is None else json.dumps(value).encode()
        request = Request(base + path, data=data, method=method,
                          headers={"Authorization": authorization, "Content-Type": "application/json"})
        with HTTP.open(request, timeout=5) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    child = None
    try:
        child = OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)
        deadline = time.monotonic() + 45
        while True:
            if not child.alive():
                raise AssertionError("Owned OpenCode exited during startup")
            try:
                api("GET", "/api/info")
                break
            except (URLError, TimeoutError, ConnectionResetError):
                if time.monotonic() >= deadline:
                    raise TimeoutError("OpenCode startup timed out")
                time.sleep(0.1)
        session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        deadline = time.monotonic() + 30
        while True:
            plugins = api("GET", "/api/plugin" + query)
            if any(item.get("id") == "fintwind.prompt-privacy" and item.get("state", {}).get("status") == "active"
                   for item in plugins.get("data", [])):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Privacy plugin did not activate")
            time.sleep(0.1)
        api("POST", f"/api/session/{session}/prompt" + query, {"text": f'password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while True:
            messages = api("GET", f"/api/session/{session}/context" + query)["data"]
            if "result " + SECRET in json.dumps(messages):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Restored assistant text did not reach OpenCode context")
            time.sleep(0.1)
        title_deadline = time.monotonic() + 20
        while True:
            info = api("GET", f"/api/session/{session}")["data"]
            if SECRET in str(info.get("title", "")):
                break
            if time.monotonic() >= title_deadline:
                raise TimeoutError("Generated title did not restore the fixture value")
            time.sleep(0.1)
        original_calls = [call for call in CALLS if call["reference"]]
        if not original_calls or any(call["contained_original"] for call in CALLS):
            raise AssertionError("Provider requests did not protect the fixture secret")
        baseline = len(CALLS)
        api("POST", f"/api/session/{session}/prompt" + query, {"text": "Repeat the previous result."})
        deadline = time.monotonic() + 45
        while len(CALLS) == baseline:
            if time.monotonic() >= deadline:
                raise TimeoutError("Follow-up provider request did not arrive")
            time.sleep(0.1)
        later = CALLS[baseline:]
        first_prefixes = {call.get("prefix_hash") for call in CALLS[:baseline] if call.get("prefix_hash") and call.get("reference")}
        later_prefixes = {call.get("prefix_hash") for call in later if call.get("prefix_hash") and call.get("reference")}
        if not first_prefixes.intersection(later_prefixes):
            raise AssertionError("System and tool prefix changed during a follow-up")
        if any(call["contained_original"] for call in later) or not any(
                call["reference"] == original_calls[0]["reference"] for call in later):
            raise AssertionError("Follow-up did not redact restored history with the same reference")
        deadline = time.monotonic() + 30
        while True:
            session_info = api("GET", f"/api/session/{session}")["data"]
            if session_info.get("status", {}).get("type") == "idle" or not session_info.get("status"):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Session did not settle before compaction")
            time.sleep(0.1)
        compact_baseline = len(CALLS)
        api("POST", f"/api/session/{session}/compact" + query, {})
        deadline = time.monotonic() + 45
        while len(CALLS) == compact_baseline:
            if time.monotonic() >= deadline:
                raise TimeoutError("Compaction request did not reach the fixture")
            time.sleep(0.1)
        if any(next(iter(call.values())) for call in CALLS[compact_baseline:]):
            raise AssertionError("Compaction request exposed the fixture secret")
        deadline = time.monotonic() + 45
        while True:
            compacted = api("GET", f"/api/session/{session}/context" + query)["data"]
            if any(message.get("type") == "compaction" for message in compacted):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Compaction was not stored in context")
            time.sleep(0.1)
        (output / "compacted-context.json").write_text(json.dumps(compacted, indent=2), encoding="utf-8")
        if not any(message.get("type") == "compaction" and SECRET in json.dumps(message) for message in compacted):
            raise AssertionError("Compaction summary did not restore the fixture secret")
        tool_session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        api("POST", f"/api/session/{tool_session}/prompt" + query,
            {"text": f'fixture tool request password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while not captured.exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("Fixture tool did not execute")
            time.sleep(0.1)
        if captured.read_text(encoding="utf-8") != SECRET:
            raise AssertionError("Fixture tool did not receive the original value")
        deadline = time.monotonic() + 45
        while not any(call["tool_result"] for call in CALLS):
            if time.monotonic() >= deadline:
                raise TimeoutError("Fixture tool result was not sent back to the provider")
            time.sleep(0.1)
        if any(call["contained_original"] for call in CALLS):
            raise AssertionError("A provider request contained the original value")
        child_parent = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        child_baseline = len(CALLS)
        api("POST", f"/api/session/{child_parent}/prompt" + query,
            {"text": f'fixture child request password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while not child_path.exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("Child session was not created")
            time.sleep(0.1)
        child_id = child_path.read_text(encoding="utf-8")
        deadline = time.monotonic() + 45
        while True:
            child_messages = api("GET", f"/api/session/{child_id}/context" + query)["data"]
            if "result " + SECRET in json.dumps(child_messages):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Child session did not restore its response")
            time.sleep(0.1)
        if any(next(iter(call.values())) for call in CALLS[child_baseline:]):
            raise AssertionError("Child session exposed the fixture secret")
        response_session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        api("POST", f"/api/session/{response_session}/model", {"model": {"providerID": "fixture-responses", "id": "fixture-model"}})
        baseline = len(CALLS)
        api("POST", f"/api/session/{response_session}/prompt" + query, {"text": f'password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while True:
            messages = api("GET", f"/api/session/{response_session}/context" + query)["data"]
            if "result " + SECRET in json.dumps(messages):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Responses fixture did not restore its text")
            time.sleep(0.1)
        if not any(call.get("responses") and call["reference"] and not next(iter(call.values()))
                   for call in CALLS[baseline:]):
            raise AssertionError("Responses fixture did not receive masked input")
        ws_session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        api("POST", f"/api/session/{ws_session}/model", {"model": {"providerID": "fixture-websocket", "id": "fixture-model"}})
        baseline = len(CALLS)
        api("POST", f"/api/session/{ws_session}/prompt" + query, {"text": f'password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while True:
            messages = api("GET", f"/api/session/{ws_session}/context" + query)["data"]
            if "result " + SECRET in json.dumps(messages):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("WebSocket fixture did not restore its text")
            time.sleep(0.1)
        if not any(call.get("websocket") and call["reference"] and not next(iter(call.values()))
                   for call in CALLS[baseline:]):
            raise AssertionError("WebSocket fixture did not receive masked input")
        captured.unlink()
        ws_tool_session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        api("POST", f"/api/session/{ws_tool_session}/model", {"model": {"providerID": "fixture-websocket", "id": "fixture-model"}})
        baseline = len(CALLS)
        api("POST", f"/api/session/{ws_tool_session}/prompt" + query,
            {"text": f'fixture ws tool password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while not captured.exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("WebSocket fixture tool did not execute")
            time.sleep(0.1)
        if captured.read_text(encoding="utf-8") != SECRET:
            raise AssertionError("WebSocket tool did not receive the original value")
        deadline = time.monotonic() + 45
        while not any(call.get("websocket") and call.get("tool_result") for call in CALLS[baseline:]):
            if time.monotonic() >= deadline:
                raise TimeoutError("WebSocket tool result did not return to the provider")
            time.sleep(0.1)
        if any(next(iter(call.values())) for call in CALLS[baseline:]):
            raise AssertionError("WebSocket provider received the original value")
        if not any(call.get("websocket") and call["reference"] and not next(iter(call.values()))
                   for call in CALLS[baseline:]):
            raise AssertionError("WebSocket tool fixture did not receive masked input")
        anth_session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        api("POST", f"/api/session/{anth_session}/model", {"model": {"providerID": "fixture-anthropic", "id": "fixture-model"}})
        anth_baseline = len(CALLS)
        api("POST", f"/api/session/{anth_session}/prompt" + query, {"text": f'password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while True:
            anth_messages = api("GET", f"/api/session/{anth_session}/context" + query)["data"]
            if "result " + SECRET in json.dumps(anth_messages):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Anthropic fixture did not restore its text")
            time.sleep(0.1)
        if not any(call.get("anthropic") and call["reference"] and not next(iter(call.values()))
                   for call in CALLS[anth_baseline:]):
            raise AssertionError("Anthropic fixture did not receive masked input")
        anth_baseline = len(CALLS)
        api("POST", f"/api/session/{anth_session}/prompt" + query, {"text": "Continue the previous task."})
        deadline = time.monotonic() + 45
        while not any(call.get("anthropic") and call.get("signed_ok") for call in CALLS[anth_baseline:]):
            if time.monotonic() >= deadline:
                raise TimeoutError("Signed thinking did not return unchanged to Anthropic fixture")
            time.sleep(0.1)
        if any(next(iter(call.values())) for call in CALLS[anth_baseline:]):
            raise AssertionError("Signed thinking replay exposed the fixture value")
        gemini_session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        api("POST", f"/api/session/{gemini_session}/model", {"model": {"providerID": "fixture-gemini", "id": "fixture-model"}})
        gemini_baseline = len(CALLS)
        api("POST", f"/api/session/{gemini_session}/prompt" + query, {"text": f'password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while True:
            gemini_messages = api("GET", f"/api/session/{gemini_session}/context" + query)["data"]
            if "result " + SECRET in json.dumps(gemini_messages):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Gemini fixture did not restore its text")
            time.sleep(0.1)
        if not any(call.get("gemini") and call["reference"] and not call["exposed"] for call in CALLS[gemini_baseline:]):
            raise AssertionError("Gemini fixture did not receive masked input")
        if any(next(iter(call.values())) for call in CALLS[gemini_baseline:]):
            raise AssertionError("Gemini fixture received the original value")
        expected_reference = next(call["reference"] for call in CALLS if call["reference"])
        child.close()
        child = None
        child = OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)
        deadline = time.monotonic() + 45
        while True:
            if not child.alive():
                raise AssertionError("Owned OpenCode exited during restart")
            try:
                api("GET", "/api/info")
                break
            except (URLError, TimeoutError, ConnectionResetError):
                if time.monotonic() >= deadline:
                    raise TimeoutError("OpenCode restart timed out")
                time.sleep(0.1)
        restarted = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
        deadline = time.monotonic() + 30
        while True:
            plugins = api("GET", "/api/plugin" + query)
            if any(item.get("id") == "fintwind.prompt-privacy" and item.get("state", {}).get("status") == "active"
                   for item in plugins.get("data", [])):
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("Privacy plugin did not reactivate")
            time.sleep(0.1)
        baseline = len(CALLS)
        api("POST", f"/api/session/{restarted}/prompt" + query, {"text": f'password="{SECRET}"'})
        deadline = time.monotonic() + 45
        while not any(call["reference"] for call in CALLS[baseline:]):
            if time.monotonic() >= deadline:
                raise TimeoutError("Restarted provider request did not arrive")
            time.sleep(0.1)
        if any(next(iter(call.values())) for call in CALLS[baseline:]) or not any(
                call["reference"] == expected_reference for call in CALLS[baseline:]):
            raise AssertionError("Restart changed the private reference or exposed the original")
        baseline = len(CALLS)
        api("POST", f"/api/session/{tool_session}/prompt" + query, {"text": "Resume the earlier work."})
        deadline = time.monotonic() + 45
        while not any(call["reference"] for call in CALLS[baseline:]):
            if time.monotonic() >= deadline:
                resumed = api("GET", f"/api/session/{session}/context" + query)["data"]
                (output / "resume-context.json").write_text(json.dumps(resumed, indent=2), encoding="utf-8")
                raise TimeoutError(f"Resumed provider request did not arrive; context: {output / 'resume-context.json'}")
            time.sleep(0.1)
        if any(next(iter(call.values())) for call in CALLS[baseline:]) or not any(
                call["reference"] == expected_reference for call in CALLS[baseline:]):
            raise AssertionError("Resumed session failed to reconstruct its references")
        baseline = len(CALLS)
        api("POST", f"/api/session/{session}/prompt" + query, {"text": "Resume compacted work."})
        deadline = time.monotonic() + 45
        while not any(call["reference"] for call in CALLS[baseline:]):
            if time.monotonic() >= deadline:
                resumed = api("GET", f"/api/session/{session}/context" + query)["data"]
                (output / "resume-context.json").write_text(json.dumps(resumed, indent=2), encoding="utf-8")
                raise TimeoutError("Resumed compacted provider request did not arrive")
            time.sleep(0.1)
        if any(next(iter(call.values())) for call in CALLS[baseline:]) or not any(
                call["reference"] == expected_reference for call in CALLS[baseline:]):
            raise AssertionError("Resumed compacted session failed to reconstruct its references")
        result = {"passed": True, "compacted_restart_restored": True, "gemini": True, "stable_system_tool_prefix": True, "anthropic_signed": True, "child_session": True, "responses_http": True, "responses_websocket": True, "websocket_tool": True, "title_restored": True, "compaction_request_redacted": True, "compaction_summary_restored": True, "restart_stable": True, "resume_redacted": True, "provider_requests": len(CALLS), "follow_up_redacted": True,
                  "tool_received_original": True, "tool_result_redacted": True, "output": str(output)}
        (output / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(json.dumps(result))
    finally:
        if child:
            child.close()
        provider.shutdown()
        provider.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    main()
