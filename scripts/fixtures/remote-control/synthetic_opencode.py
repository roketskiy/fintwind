#!/usr/bin/env python3
"""A synthetic OpenCode provider for the remote-control acceptance chain.

It speaks the slice of the OpenCode HTTP + SSE contract that the Fintwind
driver exercises for a scripted turn: session create, prompt, event stream,
permission asks, question forms, and the reply routes for both. Everything
runs on loopback TCP. There is no real provider, no model key, and no
network access of any kind.

Differences from scripts/fixtures/opencode-recovery/fake_opencode.py (read
as a reference, never modified): this fixture adds the two interactive
provider surfaces the remote chain must accept through the real wire —

  * ``permission.requested`` on the event stream, answered by
    ``POST /api/session/{sid}/permission/{id}/reply`` with
    ``{"decision": "once"|"reject"|"always"}``;
  * ``form.created`` with ``metadata.kind == "question"`` on the event
    stream, answered by ``POST /api/session/{sid}/form/{id}/reply`` with
    ``{"answer": {field: value | [values]}}``.

Behaviour comes from ``FINTWIND_RC_BEHAVIOR`` (a JSON manifest) so one
engine drives every scenario. The native request log records the path
alias, a counter and the request id only — never a payload, never a
pattern, never a prompt. Conventions the harness relies on:

* the process records its own argv to ``FINTWIND_RC_SPAWN_LOG`` on
  startup, so a test can prove it was only ever started as ``serve``;
* every route enforces the private Basic auth the driver injects;
* ``await_reply`` steps block the timeline until the named reply lands, so
  an approval or answer is observed before the turn continues.
"""

import base64
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Condition()
STATE = {
    "session_id": None,
    "current_turn": None,
    "signaled_turns": 0,
    "conn_count": 0,
    "replies": {},          # request id -> the recorded decision/answer
    "reply_arrived": {},    # request id -> bool, released with LOCK
    "active": {},
}

BEHAVIOR = {}
SPAWN_LOG = os.environ.get("FINTWIND_RC_SPAWN_LOG")
REQUEST_LOG = os.environ.get("FINTWIND_RC_LOG")
USERNAME = "opencode"
PASSWORD = os.environ.get("OPENCODE_SERVER_PASSWORD", "")


def append_log(entry):
    if not REQUEST_LOG:
        return
    with LOCK:
        try:
            os.makedirs(os.path.dirname(REQUEST_LOG), exist_ok=True)
            with open(REQUEST_LOG, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry) + "\n")
        except OSError:
            pass


def record_spawn(port):
    if not SPAWN_LOG:
        return
    # Variable names only: the whitelist is the privacy evidence, and no
    # value ever reaches a log.
    entry = {
        "event": "spawn",
        "argv": sys.argv[1:],
        "pid": os.getpid(),
        "port": port,
        "envNames": sorted(os.environ.keys()),
    }
    with LOCK:
        try:
            os.makedirs(os.path.dirname(SPAWN_LOG), exist_ok=True)
            with open(SPAWN_LOG, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry) + "\n")
        except OSError:
            pass


def log(entry):
    entry["ts"] = time.time()
    append_log(entry)


def authorized(handler):
    header = handler.headers.get("Authorization", "")
    expected = "Basic " + base64.b64encode(f"{USERNAME}:{PASSWORD}".encode()).decode()
    ok = header == expected
    if not ok:
        log({"event": "auth_fail", "path": handler.path})
    return ok


def current_turn():
    return STATE.get("current_turn")


def turn_by_index(index):
    turns = BEHAVIOR.get("turns", [])
    if index is None or index < 0 or index >= len(turns):
        return None
    return turns[index]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "synthetic-opencode/1.0"

    def log_message(self, *args):
        pass

    def _body(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        return self.rfile.read(length) if length > 0 else b""

    def _json(self, status, obj):
        payload = json.dumps(obj).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _empty(self, status):
        self.send_response(status)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _path(self):
        return self.path.split("?")[0]

    def _session_id(self):
        parts = self._path().strip("/").split("/")
        if len(parts) >= 3 and parts[1] == "session":
            return parts[2]
        return None

    def _require_auth(self):
        if not authorized(self):
            self._json(401, {"error": "unauthorized"})
            return False
        return True

    def do_GET(self):
        if not self._require_auth():
            return
        path = self._path()
        if path in ("/api/info", "/api/status", "/api/health"):
            return self._json(200, {"name": "synthetic-opencode", "version": "0.0.0-synthetic"})
        if path == "/api/event":
            return self.handle_sse()
        if path == "/api/session/active":
            return self.handle_active()
        if path == "/api/model":
            return self._json(200, {"data": BEHAVIOR.get("models", [])})
        if path in ("/api/permission/request", "/api/form", "/api/form/request"):
            return self._json(200, {"data": []})
        if path.endswith("/message"):
            turn = turn_by_index(current_turn()) or {}
            messages = json.loads(json.dumps(turn.get("durable_messages", [])))
            # Restate the acknowledged native id and the real prompt text on
            # the durable user row, the way a real server persists client
            # messages, so the reconciliation walk finds the input by id.
            accepted = STATE.get("accepted_id")
            prompt_text = STATE.get("prompt_text")
            prompt_created = STATE.get("prompt_created_ms")
            idle_at = STATE.get("idle_at_ms")
            if idle_at is None:
                messages = [message for message in messages if message.get("type") == "user"]
            for message in messages:
                if prompt_created is not None:
                    kind = message.get("type")
                    message.setdefault("time", {})["created"] = (
                        idle_at if kind == "idle" else prompt_created + (1 if kind == "assistant" else 0)
                    )
            for message in messages:
                if not (isinstance(message, dict) and message.get("type") == "user"):
                    continue
                if accepted:
                    message["id"] = accepted
                if prompt_text is not None:
                    message["text"] = prompt_text
                    if isinstance(message.get("content"), list):
                        for part in message["content"]:
                            if isinstance(part, dict) and part.get("type") == "text":
                                part["text"] = prompt_text
            log({"event": "messages", "count": len(messages)})
            return self._json(200, {"data": messages})
        if path == "/api/session" or path.startswith("/api/session/"):
            return self.handle_session_get()
        return self._json(404, {"error": "not found"})

    def do_POST(self):
        if not self._require_auth():
            return
        body = self._body()
        path = self._path()
        # Path alias and counter only: no payload is ever recorded.
        log({"event": "post", "path": path, "len": len(body)})
        if path == "/api/session":
            with LOCK:
                STATE["session_id"] = BEHAVIOR.get("session_id", "ses_synthetic")
            return self._json(200, {"data": {"id": STATE["session_id"]}})
        if path.endswith("/prompt"):
            return self.handle_prompt(body)
        if path.endswith("/interrupt"):
            return self._empty(200)
        if "permission" in path and path.endswith("/reply"):
            return self.handle_permission_reply(body)
        if "form" in path and path.endswith("/reply"):
            return self.handle_form_reply(body)
        if path.startswith("/api/question/") and path.endswith("/reply"):
            return self.handle_form_reply(body)
        return self._empty(200)

    def do_PATCH(self):
        if not self._require_auth():
            return
        self._body()
        return self._empty(200)

    def do_DELETE(self):
        if not self._require_auth():
            return
        return self._empty(200)

    def handle_prompt(self, body):
        log({"event": "prompt-enter", "len": len(body)})
        try:
            parsed = json.loads(body.decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            parsed = {}
        body_id = parsed.get("id") if isinstance(parsed, dict) else None
        body_text = parsed.get("text") if isinstance(parsed, dict) else None
        with LOCK:
            index = STATE["signaled_turns"]
            STATE["current_turn"] = index
            STATE["signaled_turns"] = index + 1
            STATE["prompt_created_ms"] = int(time.time() * 1000)
            STATE["idle_at_ms"] = None
            STATE["terminal_outcome"] = None
            # The client's native message id and real prompt text, so the
            # durable user row can name the same message the driver
            # acknowledged — the reconciliation walk matches on it.
            if isinstance(body_id, str) and body_id:
                STATE["accepted_id"] = body_id
            if isinstance(body_text, str):
                STATE["prompt_text"] = body_text
            LOCK.notify_all()
        log({"event": "prompt", "turn": index})
        turn = turn_by_index(index) or {}
        return self._json(200, {
            "data": {"id": STATE.get("accepted_id") or turn.get("user_msg_id", "msg_user"),
                     "time": {"created": STATE["prompt_created_ms"]}}
        })

    def handle_active(self):
        turn = turn_by_index(current_turn())
        running = bool(turn and turn.get("active") and STATE.get("idle_at_ms") is None)
        data = {}
        if running and STATE.get("session_id"):
            data[STATE["session_id"]] = {"type": "running"}
        return self._json(200, {"data": data})

    def handle_session_get(self):
        sid = self._session_id()
        if sid is None:
            return self._json(200, {"data": []})
        turn = turn_by_index(current_turn()) or {}
        status = turn.get("session_status") or BEHAVIOR.get("default_session_status", "idle")
        idle_at = STATE.get("idle_at_ms")
        if idle_at is not None:
            status = "idle"
        return self._json(200, {
            "data": {
                "id": sid,
                "status": {"type": status},
                "time": {"created": STATE.get("prompt_created_ms", 1700000000000), "idle": idle_at},
                "outcome": STATE.get("terminal_outcome") or turn.get("session_outcome"),
            }
        })

    def _record_reply(self, request_id, value, alias):
        with LOCK:
            STATE["replies"][request_id] = value
            STATE["reply_arrived"][request_id] = True
            LOCK.notify_all()
        log({"event": "reply", "alias": alias, "request": request_id})

    def handle_permission_reply(self, body):
        parts = self._path().strip("/").split("/")
        request_id = parts[4] if len(parts) >= 5 else "unknown"
        try:
            parsed = json.loads(body.decode("utf-8")) if body else {}
        except ValueError:
            parsed = {}
        decision = parsed.get("decision") or parsed.get("reply")
        self._record_reply(request_id, {"decision": decision}, "permission")
        return self._empty(200)

    def handle_form_reply(self, body):
        parts = self._path().strip("/").split("/")
        # /api/session/{sid}/form/{id}/reply or /api/question/{id}/reply
        request_id = parts[4] if len(parts) >= 5 else (parts[2] if len(parts) >= 3 else "unknown")
        try:
            parsed = json.loads(body.decode("utf-8")) if body else {}
        except ValueError:
            parsed = {}
        answer = parsed.get("answer", parsed.get("answers"))
        self._record_reply(request_id, {"answer": answer}, "form")
        return self._empty(200)

    def handle_sse(self):
        with LOCK:
            STATE["conn_count"] += 1
            gen = STATE["conn_count"]
        log({"event": "sse_open", "conn": gen})
        head = ("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n")
        try:
            self.connection.sendall(head.encode("ascii"))
        except OSError:
            return
        self.close_connection = True
        try:
            self._serve_timeline()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass

    def _wait_for_turn(self, index):
        deadline = time.time() + 30.0
        with LOCK:
            while STATE["signaled_turns"] <= index:
                remaining = deadline - time.time()
                if remaining <= 0:
                    return False
                LOCK.wait(remaining)
            return True

    def _emit(self, obj):
        frame = ("data: " + json.dumps(obj) + "\n\n").encode("utf-8")
        self.connection.sendall(frame)

    def _await_reply(self, request_id, timeout):
        deadline = time.time() + timeout
        with LOCK:
            while not STATE["reply_arrived"].get(request_id):
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None
                LOCK.wait(remaining)
            return STATE["replies"].get(request_id)

    def _serve_timeline(self):
        index = 0
        total = len(BEHAVIOR.get("turns", []))
        while True:
            turn = turn_by_index(index) or {}
            if not turn.get("server_initiated") and not self._wait_for_turn(index):
                return
            if self._run_steps(turn.get("sse", [])):
                return
            index += 1
            if index >= total:
                self._hold()
                return

    def _run_steps(self, steps):
        turn = turn_by_index(current_turn()) or {}
        sid = STATE.get("session_id") or "ses_synthetic"
        for step in steps or []:
            if "sleep_ms" in step:
                time.sleep(step["sleep_ms"] / 1000.0)
            if step.get("execution_started"):
                # A real server announces the run before any output; the
                # driver marks the turn active on it.
                self._emit({"type": "session.execution.started", "data": {"sessionID": sid}})
                log({"event": "execution_started_sent"})
            if step.get("text_started"):
                self._emit({"type": "session.text.started",
                            "data": {"sessionID": sid, "assistantMessageID": "msg_asst", "ordinal": 0}})
            if "delta" in step:
                self._emit({"type": "session.text.delta",
                            "data": {"sessionID": sid, "assistantMessageID": "msg_asst",
                                     "ordinal": 0, "delta": step["delta"]}})
            if "text_ended" in step:
                self._emit({"type": "session.text.ended",
                            "data": {"sessionID": sid, "assistantMessageID": "msg_asst",
                                     "ordinal": 0, "text": step["text_ended"]}})
            if "permission" in step:
                spec = step["permission"]
                self._emit({"type": "permission.requested",
                            "data": {"id": spec["id"], "sessionID": sid,
                                     "permission": spec["permission"],
                                     "patterns": spec.get("patterns", [])}})
                log({"event": "permission_asked", "request": spec["id"]})
            if "question" in step:
                spec = step["question"]
                self._emit({"type": "form.created",
                            "data": {"form": {"id": spec["id"], "sessionID": sid,
                                              "metadata": {"kind": "question"},
                                              "fields": spec["fields"]}}})
                log({"event": "question_asked", "request": spec["id"]})
            if "await_reply" in step:
                reply = self._await_reply(step["await_reply"], step.get("await_timeout_ms", 20000) / 1000.0)
                if reply is None:
                    log({"event": "reply_timeout", "request": step["await_reply"]})
                    return True
            if "terminal" in step:
                with LOCK:
                    STATE["terminal_outcome"] = step["terminal"]
                kind = ("session.execution.succeeded" if step["terminal"] == "succeeded"
                        else "session.execution.failed")
                self._emit({"type": kind, "data": {"sessionID": sid}})
                log({"event": "terminal_sent", "kind": kind})
            if step.get("idle"):
                with LOCK:
                    STATE["idle_at_ms"] = max(int(time.time() * 1000), STATE.get("prompt_created_ms", 0) + 2)
                # A real server publishes the idle companion of the terminal
                # transition; the driver settles the turn on it.
                self._emit({"type": "session.idle", "data": {"sessionID": sid}})
                log({"event": "idle_sent"})
            if step.get("close"):
                try:
                    self.connection.shutdown(2)
                    self.connection.close()
                except OSError:
                    pass
                return True
        return False

    def _hold(self):
        while True:
            try:
                self.connection.sendall(b": keep-alive\n\n")
            except OSError:
                return
            time.sleep(2.0)


def load_behavior():
    path = os.environ.get("FINTWIND_RC_BEHAVIOR")
    if not path:
        return {}
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, ValueError):
        return {}


def port_from_argv(argv):
    for i, item in enumerate(argv):
        if item == "--port" and i + 1 < len(argv):
            try:
                return int(argv[i + 1])
            except ValueError:
                continue
        if item.startswith("--port="):
            try:
                return int(item.split("=", 1)[1])
            except ValueError:
                continue
    return None


def main():
    global BEHAVIOR
    BEHAVIOR = load_behavior()
    argv = sys.argv[1:]
    # The version probe runs the binary with --version, exactly like the
    # real CLI. It never touches a server, so answer and leave.
    if "--version" in argv:
        if SPAWN_LOG:
            with LOCK:
                try:
                    os.makedirs(os.path.dirname(SPAWN_LOG), exist_ok=True)
                    with open(SPAWN_LOG, "a", encoding="utf-8") as handle:
                        handle.write(json.dumps({
                            "event": "spawn", "argv": argv, "pid": os.getpid(),
                        }) + "\n")
                except OSError:
                    pass
        print("1.0.0 (synthetic-opencode)")
        return 0
    port = port_from_argv(argv)
    if port is None:
        print("synthetic-opencode: no --port supplied", file=sys.stderr)
        sys.exit(2)

    class Server(ThreadingHTTPServer):
        daemon_threads = True
        allow_reuse_address = True

    last_error = None
    for _ in range(50):
        try:
            server = Server(("127.0.0.1", port), Handler)
            break
        except OSError as error:
            last_error = error
            time.sleep(0.1)
    else:
        print(f"synthetic-opencode: could not bind port {port}: {last_error}", file=sys.stderr)
        sys.exit(3)

    record_spawn(server.server_address[1])
    log({"event": "listening", "port": server.server_address[1]})
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
