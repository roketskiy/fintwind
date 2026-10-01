#!/usr/bin/env python3
"""A local fake `opencode serve` used only by Fintwind's recovery E2E tests.

It speaks the small slice of the OpenCode HTTP + server-sent-event contract
that the Fintwind driver and model discovery actually exercise, all over
loopback TCP. It never touches the network and never a real provider.

Conventions the E2E harness relies on:

* The process records its own argv to `FINTWIND_FAKE_SPAWN_LOG` on startup, so
  a test can prove it was only ever started as `serve ...` (never `api` or
  `models`, which would join/start the real public service).
* Every route enforces the private Basic auth Fintwind injects, and every auth
  failure is recorded, so a test can prove the whole chain authenticated.
* Behaviour (models, scripted SSE timelines, durable history) comes from
  `FINTWIND_FAKE_BEHAVIOR` (a JSON manifest), so one engine drives every
  scenario; per-request detail is appended to `FINTWIND_FAKE_LOG`.
"""

import base64
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

LOCK = threading.Condition()
STATE = {
    "session_id": None,
    "current_turn": None,      # index of the active/most-recent turn
    "signaled_turns": 0,       # how many prompts have arrived
    "conn_count": 0,           # SSE connections opened (>=2 means a reconnect)
    "reconnect_done": False,
    "accepted": {},            # turn index -> the native id the input was stored under
    "prompt_text": {},          # turn index -> the real prompt text stored for that input
    "steer": {},                # turn index -> [ack ids of steers submitted into it]
    "steer_text": {},           # turn index -> [prompt texts of those steers]
    "steer_arrived": False,     # a steer POST has arrived (unblocks `await_steer`)
    "delivered": {},            # turn index -> the latest-steer durable window has landed
    "terminal_sent": {},        # turn index -> native execution is now idle
}

BEHAVIOR = {}
SPAWN_LOG = os.environ.get("FINTWIND_FAKE_SPAWN_LOG")
REQUEST_LOG = os.environ.get("FINTWIND_FAKE_LOG")
USERNAME = "opencode"
PASSWORD = os.environ.get("OPENCODE_SERVER_PASSWORD", "")


def append_log(entry):
    if not REQUEST_LOG:
        return
    with LOCK:
        try:
            os.makedirs(os.path.dirname(REQUEST_LOG), exist_ok=True)
            with open(REQUEST_LOG, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry, ensure_ascii=False) + "\n")
        except OSError:
            pass


def record_spawn(port):
    if not SPAWN_LOG:
        return
    entry = {
        "event": "spawn",
        "argv": sys.argv[1:],
        "pid": os.getpid(),
        "port": port,
        "ts": time.time(),
    }
    with LOCK:
        try:
            os.makedirs(os.path.dirname(SPAWN_LOG), exist_ok=True)
            with open(SPAWN_LOG, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry, ensure_ascii=False) + "\n")
        except OSError:
            pass


def log(entry):
    entry["ts"] = time.time()
    append_log(entry)


def authorized(handler):
    """Check the private Basic auth, logging every failure."""
    header = handler.headers.get("Authorization", "")
    expected = "Basic " + base64.b64encode(
        f"{USERNAME}:{PASSWORD}".encode()
    ).decode()
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
    server_version = "fake-opencode/1.0"

    def log_message(self, *args):
        pass

    def _body(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        if length > 0:
            return self.rfile.read(length)
        return b""

    def _json(self, status, obj):
        payload = json.dumps(obj, ensure_ascii=False).encode("utf-8")
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
        return urlparse(self.path).path

    def _directory(self):
        return self.headers.get("x-opencode-directory")

    def _require_auth(self):
        if not authorized(self):
            self._json(401, {"error": "unauthorized"})
            return False
        return True

    def _session_id(self):
        parts = self._path().strip("/").split("/")
        if len(parts) >= 3 and parts[1] == "session":
            return parts[2]
        return None

    def do_GET(self):
        if not self._require_auth():
            return
        path = self._path()
        if path in ("/api/info", "/api/status", "/api/health"):
            return self._json(200, {"name": "fake-opencode", "version": "0.0.0-fake"})
        if path == "/api/event":
            return self.handle_sse()
        if path == "/api/session/active":
            return self.handle_active()
        if path == "/api/model":
            return self.handle_model()
        if path == "/api/permission/request":
            return self._json(200, {"data": []})
        if path in ("/api/form", "/api/form/request"):
            return self._json(200, {"data": []})
        if path.endswith("/message"):
            return self.handle_messages()
        if path == "/api/session" or path.startswith("/api/session/"):
            return self.handle_session_get()
        return self._json(404, {"error": "not found", "path": path})

    def do_POST(self):
        if not self._require_auth():
            return
        body = self._body()
        path = self._path()
        if path == "/api/session":
            return self.handle_create_session()
        if path.endswith("/prompt"):
            return self.handle_prompt(body)
        if path.endswith("/agent"):
            return self._empty(200)
        if path.endswith("/model"):
            return self._empty(200)
        if path.endswith("/interrupt"):
            return self._empty(200)
        if path.endswith("/compact"):
            return self._empty(200)
        if path.endswith("/fork"):
            sid = self._session_id()
            return self._json(200, {"id": f"{sid}_fork"})
        if path.endswith("/revert/stage") or path.endswith("/revert/commit"):
            return self._empty(200)
        if path == "/api/plugin/await-activation":
            return self._empty(200)
        if "permission" in path and path.endswith("/reply"):
            return self._empty(200)
        if "form" in path and path.endswith("/reply"):
            return self._empty(200)
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

    def handle_active(self):
        turn = turn_by_index(current_turn())
        # The active roster lists a session solely by the `active` flag — it is
        # independent of session `status`, which is what makes it a real veto
        # rather than a status-shaped one.
        running = bool(turn and turn.get("active"))
        if turn and turn.get("active_until_steer"):
            running = not bool(STATE.get("steer_arrived"))
        if turn and turn.get("active_until_terminal"):
            running = not bool(STATE["terminal_sent"].get(current_turn()))
        data = {}
        if running and STATE.get("session_id"):
            data[STATE["session_id"]] = {"type": "running"}
        log({"event": "active", "running": running, "data": list(data.keys())})
        return self._json(200, {"data": data})

    def handle_model(self):
        directory = self._directory()
        port = self.server.server_address[1]
        log({"event": "model", "directory": directory, "port": port})
        rows = BEHAVIOR.get("models", {}).get(directory)
        if rows is None:
            rows = []
        return self._json(200, {"data": rows})

    def _rewrite_user(self, messages, created, new_id, text):
        if created is None:
            return
        for message in messages:
            if not (isinstance(message, dict) and message.get("type") == "user"
                    and (message.get("time") or {}).get("created") == created):
                continue
            if new_id:
                message["id"] = new_id
            if text is not None:
                message["text"] = text
                if isinstance(message.get("content"), list):
                    for part in message["content"]:
                        if isinstance(part, dict) and part.get("type") == "text":
                            part["text"] = text

    def handle_messages(self):
        idx = current_turn()
        turn = turn_by_index(idx)
        delivered = bool(STATE.get("delivered", {}).get(idx))
        if delivered and turn and "durable_messages_after" in turn:
            # R1 post-steer window: the latest steer's durable rows have landed.
            messages = json.loads(json.dumps(turn.get("durable_messages_after", [])))
        elif turn:
            messages = json.loads(json.dumps(turn.get("durable_messages", [])))
        else:
            messages = []
        # Restate the acknowledged native ids (and the real prompt/steer text) on
        # the durable inputs, the way a real server persists client messages, so
        # the reconciliation walk finds each input by id and a client's
        # content-based presentation match still finds its user.
        self._rewrite_user(messages, (turn or {}).get("created_ms"),
                           STATE.get("accepted", {}).get(idx),
                           STATE.get("prompt_text", {}).get(idx))
        steer_ids = STATE.get("steer", {}).get(idx, [])
        steer_texts = STATE.get("steer_text", {}).get(idx, [])
        steer_created = (turn or {}).get("steer_created_ms", []) or []
        for position, created in enumerate(steer_created):
            self._rewrite_user(messages, created,
                               steer_ids[position] if position < len(steer_ids) else None,
                               steer_texts[position] if position < len(steer_texts) else None)
        # Mirror the prompt's attachment files onto the initial durable user row
        # (P1: an attachment-only prompt has empty text but non-empty files, so
        # the persisted native user must carry `text: ''` plus `files: [...]`).
        prompt_files = STATE.get("prompt_files", {}).get(idx)
        if prompt_files is not None:
            first_created = (turn or {}).get("created_ms")
            for message in messages:
                if (isinstance(message, dict) and message.get("type") == "user"
                        and (message.get("time") or {}).get("created") == first_created):
                    message["files"] = json.loads(json.dumps(prompt_files))
        return self._json(200, {"data": messages})

    def handle_session_get(self):
        sid = self._session_id()
        if sid is None:
            return self._json(200, {"data": [], "cursor": {"next": ""}})
        turn = turn_by_index(current_turn())
        idx = current_turn()
        delivered = bool(STATE.get("delivered", {}).get(idx))
        base = (turn or {}).get("session_after", {}) if delivered else {}
        status = (base or {}).get("session_status") or (turn or {}).get("session_status") or BEHAVIOR.get("default_session_status", "idle")
        outcome = (base or {}).get("session_outcome", (turn or {}).get("session_outcome"))
        idle = (base or {}).get("session_time_idle", (turn or {}).get("session_time_idle"))
        log({"event": "session_get", "sid": sid, "status": status, "outcome": outcome, "idle": idle, "delivered": delivered})
        data = {
            "id": sid,
            "status": {"type": status},
            "time": {"created": 1700000000000, "idle": idle},
            "outcome": outcome,
        }
        return self._json(200, {"data": data})

    def handle_create_session(self):
        with LOCK:
            STATE["session_id"] = f"ses_fake_{os.getpid():06d}"
            sid = STATE["session_id"]
        return self._json(200, {"data": {"id": sid}})

    def handle_prompt(self, body):
        try:
            parsed = json.loads(body.decode("utf-8")) if body else {}
        except (ValueError, UnicodeDecodeError):
            parsed = {}
        body_id = parsed.get("id") if isinstance(parsed, dict) else None
        body_text = parsed.get("text") if isinstance(parsed, dict) else None
        body_files = parsed.get("files") if isinstance(parsed, dict) else None
        with LOCK:
            steer_mode = bool(BEHAVIOR.get("steer_mode"))
            prior_turn = STATE.get("current_turn")
            is_steer = (
                steer_mode
                and prior_turn is not None
                and STATE["signaled_turns"] >= 1
            )
            if is_steer:
                # R1: a steer rides the same live turn; it is not a new turn.
                index = prior_turn
                accepted = body_id if isinstance(body_id, str) and body_id else f"steer_{index}_{len(STATE['steer'].get(index, []))}"
                STATE["steer"].setdefault(index, []).append(accepted)
                STATE["steer_text"].setdefault(index, []).append(
                    body_text if isinstance(body_text, str) else "steer")
                STATE["steer_arrived"] = True
                turn = turn_by_index(index)
            else:
                index = STATE["signaled_turns"]
                turn = turn_by_index(index)
                if turn and turn.pop("rebase_clock", False):
                    # Match a real server's clock at receipt of the POST. This
                    # verifies a lost ack without decades-future timestamps.
                    offset = int(time.time() * 1000) - turn["created_ms"]
                    def shift_times(value):
                        if isinstance(value, dict):
                            for key, item in value.items():
                                if key in ("created", "completed", "idle", "created_ms", "session_time_idle") and isinstance(item, int):
                                    value[key] = item + offset
                                else:
                                    shift_times(item)
                        elif isinstance(value, list):
                            for item in value:
                                shift_times(item)
                    shift_times(turn)
                STATE["current_turn"] = index
                STATE["signaled_turns"] = index + 1
                STATE["reconnect_done"] = False
            sid = STATE["session_id"] or f"ses_fake_{os.getpid():06d}"
            STATE["session_id"] = sid
            # Preserve the client's native message id (V2 prompt payload `id`)
            # so the stored input, the acknowledgement, and the reconciliation
            # walk all name the same message.
            if is_steer:
                pass  # steer id/text recorded above
            else:
                accepted = turn.get("user_msg_id", "msg_user") if turn is not None else "msg_unknown"
                if isinstance(body_id, str) and body_id:
                    accepted = body_id
                STATE["accepted"][index] = accepted
                # The stored user row must carry the real prompt text, not a
                # synthetic one, so a client that reconciles the snapshot matches
                # the local user by content (see reconcile_active_transcript).
                STATE.setdefault("prompt_text", {})[index] = (
                    body_text if isinstance(body_text, str) else None)
                # A prompt may carry attachments (non-image PDF/txt/dir) with
                # empty text; mirror them as the native `files` field so the
                # durable user row keeps its empty text plus its files.
                STATE.setdefault("prompt_files", {})[index] = (
                    body_files if isinstance(body_files, list) else [])
            LOCK.notify_all()
        if turn is None:
            return self._json(200, {"data": {"id": "msg_unknown", "time": {"created": 0}}})
        log({"event": "steer" if is_steer else "prompt", "turn": index,
             "sid": sid, "input_id": accepted})
        if turn.get("drop_ack"):
            # The input is durably stored, but the acknowledgement never comes
            # back: the connection is torn down instead of answering. The driver
            # must treat this as an unknown transport fault — never resubmit,
            # never fail the turn — and reconcile from durable history later.
            log({"event": "prompt_ack_dropped", "turn": index})
            self.close_connection = True
            try:
                self.connection.shutdown(2)
            except OSError:
                pass
            return
        return self._json(200, {
            "data": {
                "id": accepted,
                "time": {"created": turn.get("created_ms", 0)},
            }
        })

    def handle_sse(self):
        with LOCK:
            STATE["conn_count"] += 1
            gen = STATE["conn_count"]
        log({"event": "sse_open", "conn": gen})
        # A scenario can force `/api/event` to refuse the subscription (e.g. a
        # server that never accepts an SSE handshake). The ordinary HTTP prompt
        # and history routes still work, so the turn must still converge through
        # reconciliation — no SSE generation ever establishes (gen0).
        sse_status = int(BEHAVIOR.get("sse_status", 200))
        if sse_status != 200:
            log({"event": "sse_refused", "conn": gen, "status": sse_status})
            try:
                self.connection.sendall(
                    f"HTTP/1.1 {sse_status} Error\r\n"
                    "Content-Length: 0\r\nConnection: close\r\n\r\n".encode("ascii")
                )
            except OSError:
                pass
            self.close_connection = True
            return
        # Each SSE event is one line; a chunked stream frames each event as its
        # own HTTP chunk. The line reader below tolerates either framing.
        self.chunked = bool(BEHAVIOR.get("sse_chunked"))
        head = (
            "HTTP/1.1 200 OK\r\n"
            "Content-Type: text/event-stream\r\n"
            "Cache-Control: no-cache\r\n"
        )
        if self.chunked:
            head += "Transfer-Encoding: chunked\r\n"
        head += "\r\n"
        try:
            self.connection.sendall(head.encode("ascii"))
        except OSError:
            return
        self.close_connection = True
        try:
            if gen == 1:
                self._serve_first_generation()
            else:
                self._serve_reconnect()
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

    @staticmethod
    def _frame_bytes(obj):
        return ("data: " + json.dumps(obj, ensure_ascii=False) + "\n\n").encode("utf-8")

    @staticmethod
    def _chunk(frame):
        return format(len(frame), "x").encode("ascii") + b"\r\n" + frame + b"\r\n"

    def _emit(self, obj, step=None):
        if step and "created" in step:
            obj["created"] = step["created"]
        frame = self._frame_bytes(obj)
        if self.chunked:
            frame = self._chunk(frame)
        if step and step.get("fragment"):
            self._send_fragmented(frame, step.get("pause_ms", 320))
        else:
            self.connection.sendall(frame)

    def _send_comment(self, text):
        frame = (":" + text + "\n\n").encode("utf-8")
        if self.chunked:
            frame = self._chunk(frame)
        self.connection.sendall(frame)

    @staticmethod
    def _fragment_offsets(frame):
        # Deliberately land splits inside a multibyte UTF-8 char, between the
        # CR and LF of a chunk/line boundary, and once mid-body — so a reader
        # must retain partial bytes across a paused TCP gap to reassemble.
        n = len(frame)
        offsets = set()
        for index, byte in enumerate(frame):
            if byte >= 0x80:
                offsets.add(index + 1)  # split mid-codepoint, after its lead byte
                break
        crlf = frame.find(b"\r\n")
        if crlf >= 0:
            offsets.add(crlf + 1)       # split between CR and LF
        offsets.add(n // 2)             # split mid-body
        return {offset for offset in offsets if 0 < offset < n}

    def _send_fragmented(self, frame, pause_ms):
        prev = 0
        for offset in sorted(self._fragment_offsets(frame)):
            self.connection.sendall(frame[prev:offset])
            time.sleep(pause_ms / 1000.0)
            prev = offset
        self.connection.sendall(frame[prev:])

    def _heartbeat(self):
        self._send_comment("keep-alive")

    def _serve_first_generation(self):
        index = 0
        total = len(BEHAVIOR.get("turns", []))
        while True:
            turn = turn_by_index(index) or {}
            if turn.get("server_initiated"):
                # R2: the provider starts this run on its own (no prompt POST);
                # it runs automatically right after the previous turn's output.
                with LOCK:
                    STATE["current_turn"] = index
                    if STATE["signaled_turns"] <= index:
                        STATE["signaled_turns"] = index + 1
                    STATE["reconnect_done"] = False
            elif not self._wait_for_turn(index):
                return
            if self._run_steps(turn.get("sse", [])):
                return
            index += 1
            if index >= total:
                self._hold()
                return

    def _serve_reconnect(self):
        with LOCK:
            turn = turn_by_index(current_turn())
            already = STATE["reconnect_done"]
            STATE["reconnect_done"] = True
        log({"event": "sse_reconnect", "conn": STATE["conn_count"]})
        steps = [] if (already or turn is None) else turn.get("reconnect_sse", [])
        self._run_steps(steps)
        self._hold()

    def _run_steps(self, steps):
        turn = turn_by_index(current_turn()) or {}
        sid = STATE["session_id"] or "ses_fake"
        for step in steps or []:
            asst = step.get("assistant_msg_id", turn.get("assistant_msg_id", "msg_asst"))
            if "sleep_ms" in step:
                time.sleep(step["sleep_ms"] / 1000.0)
            if "heartbeat_ms" in step:
                try:
                    self._heartbeat()
                except OSError:
                    return True
                time.sleep(step["heartbeat_ms"] / 1000.0)
            if "hold_ms" in step:
                # Heartbeat through a window so the SSE stays live without any
                # session event, giving the test a phase to observe (no settle)
                # before a later step advances the fixture.
                end = time.time() + step["hold_ms"] / 1000.0
                while time.time() < end:
                    try:
                        self._heartbeat()
                    except OSError:
                        return True
                    time.sleep(min(0.4, max(0.0, end - time.time())))
            if step.get("text_started"):
                self._emit({"type": "session.text.started",
                            "data": {"sessionID": sid, "assistantMessageID": asst,
                                     "ordinal": 0}}, step)
            if "delta" in step:
                self._emit({"type": "session.text.delta",
                            "data": {"sessionID": sid, "assistantMessageID": asst,
                                     "ordinal": 0, "delta": step["delta"]}}, step)
            if "reasoning_delta" in step:
                self._emit({"type": "session.reasoning.delta",
                            "data": {"sessionID": sid, "assistantMessageID": asst,
                                     "ordinal": 0, "delta": step["reasoning_delta"]}}, step)
            if "text_ended" in step:
                self._emit({"type": "session.text.ended",
                            "data": {"sessionID": sid, "assistantMessageID": asst,
                                     "ordinal": 0, "text": step["text_ended"]}}, step)
            if step.get("step_started"):
                self._emit({"type": "session.step.started",
                            "data": {"sessionID": sid,
                                     "assistantMessageID": asst, "agent": "build"}}, step)
            if step.get("step_ended"):
                self._emit({"type": "session.step.ended",
                            "data": {"sessionID": sid,
                                     "assistantMessageID": asst}}, step)
            if "tool_input_started" in step:
                self._emit({"type": "session.tool.input.started",
                            "data": {"sessionID": sid, "id": "call_tool_1",
                                     "name": step["tool_input_started"],
                                     "assistantMessageID": asst}}, step)
            if "terminal" in step:
                with LOCK:
                    STATE["terminal_sent"][current_turn()] = True
                kind = ("session.execution.succeeded"
                        if step["terminal"] == "succeeded"
                        else "session.execution.failed")
                self._emit({"type": kind, "data": {"sessionID": sid}}, step)
                log({"event": "terminal_sent", "turn": current_turn(),
                     "kind": kind})
            if "execution_started" in step:
                # R2: a server-initiated execution. The native envelope carries
                # a top-level sortable event id and created millis; the coast is
                # derived by swapping evt_ for msg_ (SessionMessage.fromEvent).
                spec = step["execution_started"]
                event = {"type": "session.execution.started",
                         "data": {"sessionID": sid},
                         "id": spec["id"], "created": spec["created"]}
                self._emit(event, step)
                log({"event": "execution_started", "id": spec["id"],
                     "created": spec["created"]})
            if step.get("await_steer"):
                # Block until a steer POST lands (R1), so the stale terminal is
                # emitted only after the steer has reopened the turn.
                deadline = time.time() + 30.0
                with LOCK:
                    while not STATE.get("steer_arrived"):
                        remaining = deadline - time.time()
                        if remaining <= 0:
                            return True
                        LOCK.wait(remaining)
            if step.get("deliver_latest"):
                # R1: the latest steer's durable window lands now — only after
                # this does reconciliation have the latest input to settle on.
                with LOCK:
                    STATE.setdefault("delivered", {})[current_turn()] = True
                    LOCK.notify_all()
                log({"event": "deliver_latest", "turn": current_turn()})
            if step.get("close"):
                try:
                    self.connection.shutdown(2)
                    self.connection.close()
                except OSError:
                    pass
                return True
        return False

    def _hold(self):
        # Keep the connection open with heartbeats so the hub sees liveness by
        # raw data (including heartbeats), not by model output.
        while True:
            try:
                self._heartbeat()
            except OSError:
                return
            time.sleep(2.0)


def load_behavior():
    path = os.environ.get("FINTWIND_FAKE_BEHAVIOR")
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
    port = port_from_argv(argv)
    if port is None:
        # The driver always supplies the port; refuse to guess one because the
        # pool and health-probe assume the exact port.
        print("fake-opencode: no --port supplied", file=sys.stderr)
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
        print(f"fake-opencode: could not bind port {port}: {last_error}", file=sys.stderr)
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
