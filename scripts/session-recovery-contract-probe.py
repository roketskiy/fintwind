"""Batch-A contract probe for docs/session-recovery-plan.md against isolated real OpenCode.

Verifies on the actual OpenCode binary (v2.2.1): prompt admission with a
client-supplied input ID, exact-ID history lookup, duplicate-ID behaviour,
interrupt evidence (idle and mid-execution), missing-session shape, active
listing, session terminal evidence, and the SSE event sequence around a
prompt. No real provider is contacted: a loopback fixture answers model
requests. No Fintwind code is involved: this probes the raw V2 HTTP API so
the recovery implementation cannot rest on guessed contracts.

Isolation (plan 11.1): exclusive run dir, environment whitelist, random
loopback ports, Windows Job containment for the owned `opencode serve`
child, bounded per-request timeouts plus a total run budget. Only the
owned child is ever stopped (via its Job handle).

Artifacts: temp/session-recovery-contract/<uuid>/result.json — machine
readable, de-identified (shapes, keys, counts, lengths only; no passwords,
no prompt content, relative paths only).

Run: python scripts/session-recovery-contract-probe.py
Requires: Windows, Python 3.12+, `opencode` on PATH.
"""

import base64
import importlib.util
import json
import os
import shutil
import socket
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import ProxyHandler, Request, build_opener

ROOT = Path(__file__).resolve().parent.parent
RUN_ID = uuid.uuid4().hex
OUTPUT = ROOT / "temp" / "session-recovery-contract" / RUN_ID
HTTP = build_opener(ProxyHandler({}))

TOTAL_BUDGET_S = 420
REQUEST_TIMEOUT_S = 15
POLL_TIMEOUT_S = 60

# Reuse the proven Windows Job containment helper.
_spec = importlib.util.spec_from_file_location(
    "goal_e2e_helpers", ROOT / "scripts" / "goal-opencode-e2e.py")
_helpers = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_helpers)
OwnedProcess = _helpers.OwnedProcess
wait_for = _helpers.wait_for


class Provider(BaseHTTPRequestHandler):
    """Deterministic loopback model. Holds requests until released."""

    calls = []
    hold = False
    entered = threading.Event()
    release = threading.Event()
    lock = threading.Lock()

    def log_message(self, *_args):
        pass

    def do_GET(self):
        data = json.dumps({"object": "list", "data": [{"id": "fixture-model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def send_json(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        try:
            length = int(self.headers.get("Content-Length") or 0)
            payload = json.loads(self.rfile.read(length) or b"{}")
        except OSError:
            return
        with Provider.lock:
            Provider.calls.append({"stream": bool(payload.get("stream"))})
        Provider.entered.set()
        if Provider.hold and not Provider.release.wait(45):
            self.send_error(500, "fixture hold was never released")
            return
        message = {"role": "assistant", "content": "Contract probe reply."}
        common = {"id": "chatcmpl-probe", "created": 0, "model": "fixture-model"}
        usage = {"prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10}
        if not payload.get("stream"):
            try:
                self.send_json({**common, "object": "chat.completion", "usage": usage,
                                "choices": [{"index": 0, "message": message, "finish_reason": "stop"}]})
            except OSError:
                pass
            return
        chunks = [{**common, "object": "chat.completion.chunk",
                   "choices": [{"index": 0, "delta": message, "finish_reason": None}]},
                  {**common, "object": "chat.completion.chunk", "usage": usage,
                   "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}]
        data = ("".join("data: " + json.dumps(c) + "\n\n" for c in chunks) + "data: [DONE]\n\n").encode()
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except OSError:
            # The server tore the model request down (e.g. our interrupt won
            # the race). A late fixture reply has nowhere to go; not a failure.
            pass


def main():
    started = time.monotonic()
    binary = shutil.which("opencode") or shutil.which("opencode2")
    if not binary:
        raise RuntimeError("opencode must be available on PATH.")
    if os.name != "nt":
        raise RuntimeError("This runner requires Windows Job Objects.")

    OUTPUT.mkdir(parents=True)
    work = OUTPUT / "workspace"
    work.mkdir()
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    config = {"model": "fixture/fixture-model", "small_model": "fixture/fixture-model",
              "provider": {"fixture": {"npm": "@ai-sdk/openai-compatible", "name": "Fixture",
                  "options": {"apiKey": "fixture-only",
                              "baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                  "models": {"fixture-model": {"name": "Fixture",
                      "limit": {"context": 128000, "output": 8192}}}}}}
    env = {k: v for k, v in os.environ.items() if k.upper() in {
        "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT",
        "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}}
    for key, sub in {"HOME": "home", "USERPROFILE": "home", "APPDATA": "roaming",
                     "LOCALAPPDATA": "local", "TEMP": "tmp", "TMP": "tmp",
                     "OPENCODE_CONFIG_DIR": "config", "XDG_CONFIG_HOME": "xdg/config",
                     "XDG_DATA_HOME": "xdg/data", "XDG_CACHE_HOME": "xdg/cache",
                     "XDG_STATE_HOME": "xdg/state"}.items():
        d = OUTPUT / sub
        d.mkdir(parents=True, exist_ok=True)
        env[key] = str(d)
    password = uuid.uuid4().hex
    env.update({"OPENCODE_CONFIG_CONTENT": json.dumps(config),
                "OPENCODE_DB": str(OUTPUT / "opencode.db"),
                "OPENCODE_SERVER_USERNAME": "opencode",
                "OPENCODE_SERVER_PASSWORD": password,
                "OPENCODE_DISABLE_AUTOUPDATE": "1",
                "OPENCODE_DISABLE_FILEWATCHER": "1",
                "OPENCODE_DISABLE_PROJECT_CONFIG": "1",
                "OPENCODE_CONFIG_PROJECT_DISABLE": "1",
                "NO_PROXY": "127.0.0.1,localhost,::1"})
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    auth = "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()
    base = f"http://127.0.0.1:{port}"
    checks, probes = [], {}

    def check(label, ok, detail=""):
        checks.append({"label": label, "ok": bool(ok), "detail": str(detail)[:300]})
        print(("PASS " if ok else "FAIL ") + label, flush=True)
        if not ok:
            raise AssertionError(f"{label}: {detail}")

    def api(method, path, value=None, raw_body=None, allow_error=False, headers=None):
        if time.monotonic() - started > TOTAL_BUDGET_S:
            raise TimeoutError("Total probe budget exceeded.")
        data = raw_body if raw_body is not None else (
            None if value is None else json.dumps(value).encode())
        req = Request(base + path, data=data, method=method,
                      headers={"Authorization": auth,
                               "Content-Type": "application/json",
                               **(headers or {})})
        try:
            with HTTP.open(req, timeout=REQUEST_TIMEOUT_S) as resp:
                raw = resp.read()
                return {"status": resp.status,
                        "body": json.loads(raw) if raw else None}
        except HTTPError as err:
            detail = err.read().decode(errors="replace")
            if allow_error:
                try:
                    return {"status": err.code, "body": json.loads(detail)}
                except json.JSONDecodeError:
                    return {"status": err.code, "body": {"_raw_len": len(detail)}}
            raise AssertionError(f"{method} {path} failed: {err.code} {detail[:300]}")

    def shape(value):
        if isinstance(value, dict):
            return sorted(value.keys())
        return type(value).__name__

    child = None
    try:
        child = OwnedProcess(binary, ["serve", "--hostname", "127.0.0.1",
                                      "--port", str(port)], work, env)

        def ready():
            if not child.alive():
                raise AssertionError("Owned opencode process exited during startup.")
            try:
                with HTTP.open(Request(
                        base + "/api/info", headers={"Authorization": auth}),
                        timeout=3) as resp:
                    return resp.status == 200
            except OSError:
                return False

        wait_for("isolated opencode ready", ready, timeout=60)

        version = api("GET", "/api/info")
        info_data = (version["body"] or {}).get("data", version["body"] or {})
        probes["server_info"] = {"status": version["status"],
                                 "keys": shape(info_data),
                                 "version": str(info_data.get("version", ""))[:32]}
        check("server info reachable", version["status"] == 200)

        created = api("POST", "/api/session", {"location": {"directory": str(work)}})
        session_id = ((created["body"] or {}).get("data") or {}).get("id", "")
        check("session created", created["status"] in (200, 201) and bool(session_id),
              f"status={created['status']}")
        probes["session_create"] = {"status": created["status"],
                                    "id_len": len(session_id)}

        input_id = "msg_probe_" + uuid.uuid4().hex[:20]
        first = api("POST", f"/api/session/{quote(session_id)}/prompt",
                    {"text": "Contract probe ping one.", "id": input_id})
        first_data = (first["body"] or {}).get("data", first["body"] or {})
        echoed = first_data.get("id", "")
        probes["prompt_accept"] = {"status": first["status"],
                                   "response_keys": shape(first_data),
                                   "echoed_matches": echoed == input_id,
                                   "echoed_len": len(str(echoed))}
        check("prompt accepted with client id", first["status"] == 200 and echoed == input_id,
              f"status={first['status']} echoed_len={len(str(echoed))}")

        def turn_done():
            try:
                listing = api("GET", f"/api/session/{quote(session_id)}/message?limit=200")
            except AssertionError:
                return False
            rows = ((listing["body"] or {}).get("data") or [])
            texts = [r for r in rows if r.get("type") in ("assistant", "user")]
            return any(r.get("id") == input_id for r in rows) and any(
                r.get("type") == "assistant" for r in rows)

        wait_for("first turn visible in history", turn_done, timeout=POLL_TIMEOUT_S)
        history = api("GET", f"/api/session/{quote(session_id)}/message?limit=200")
        rows = ((history["body"] or {}).get("data") or [])
        user_rows = [r for r in rows if r.get("type") == "user"]
        probes["history_has_input"] = {"status": history["status"],
                                       "row_count": len(rows),
                                       "user_count": len(user_rows),
                                       "contains_input_id": any(r.get("id") == input_id for r in rows),
                                       "row_types": sorted({str(r.get('type')) for r in rows})}
        check("history contains exact input id",
              any(r.get("id") == input_id for r in rows))

        # Duplicate-ID behaviour (F08): same client id after the turn settled.
        before = sum(1 for r in rows if r.get("id") == input_id)
        calls_before = len(Provider.calls)
        repeat = api("POST", f"/api/session/{quote(session_id)}/prompt",
                     {"text": "Contract probe ping one.",
                      "id": input_id}, allow_error=True)
        time.sleep(3)
        history2 = api("GET", f"/api/session/{quote(session_id)}/message?limit=200")
        rows2 = ((history2["body"] or {}).get("data") or [])
        after = sum(1 for r in rows2 if r.get("id") == input_id)
        probes["repeat_same_id"] = {
            "status": repeat["status"],
            "body_keys": shape((repeat["body"] or {}).get("data", repeat["body"] or {})),
            "same_id_count_before": before, "same_id_count_after": after,
            "provider_calls_before": calls_before,
            "provider_calls_after": len(Provider.calls),
            "deduped": after == before,
            "conflict": repeat["status"] == 409}
        check("duplicate id answered without transport failure",
              repeat["status"] in (200, 201, 409),
              f"status={repeat['status']}")

        # Interrupt on an idle session.
        stopped = api("POST", f"/api/session/{quote(session_id)}/interrupt", {},
                      allow_error=True)
        probes["interrupt_on_idle"] = {
            "status": stopped["status"],
            "body_keys": shape((stopped["body"] or {}).get("data", stopped["body"] or {}))}
        check("interrupt on idle answers 2xx", 200 <= stopped["status"] < 300,
              f"status={stopped['status']}")

        # Mid-execution interrupt with a held provider (explicit barrier, no sleep race).
        Provider.hold = True
        Provider.entered.clear()
        Provider.release.clear()
        calls_mark = len(Provider.calls)
        held_id = "msg_probe_" + uuid.uuid4().hex[:20]
        held = api("POST", f"/api/session/{quote(session_id)}/prompt",
                   {"text": "Contract probe held ping.", "id": held_id})
        check("held prompt admitted", held["status"] == 200, f"status={held['status']}")
        if not Provider.entered.wait(45):
            raise AssertionError("Fixture provider never received the held request.")
        mid = api("POST", f"/api/session/{quote(session_id)}/interrupt", {},
                  allow_error=True)
        Provider.release.set()
        Provider.hold = False

        def held_settled():
            try:
                sess = api("GET", f"/api/session/{quote(session_id)}")
            except AssertionError:
                return False
            data = (sess["body"] or {}).get("data", sess["body"] or {})
            status = data.get("status", {})
            stype = status.get("type") if isinstance(status, dict) else status
            idle_at = ((data.get("time") or {}) if isinstance(data.get("time"), dict) else {}).get("idle")
            return (stype in ("idle", "done", None) and idle_at) or len(Provider.calls) > calls_mark

        try:
            wait_for("held turn settles after interrupt", held_settled, timeout=POLL_TIMEOUT_S)
            settled = True
        except AssertionError:
            settled = False
        final = api("GET", f"/api/session/{quote(session_id)}")
        fdata = (final["body"] or {}).get("data", final["body"] or {})
        fstatus = fdata.get("status", {})
        ftime = fdata.get("time", {}) if isinstance(fdata.get("time"), dict) else {}
        probes["interrupt_mid_execution"] = {
            "interrupt_status": mid["status"],
            "interrupt_keys": shape((mid["body"] or {}).get("data", mid["body"] or {})),
            "settled_within_budget": settled,
            "session_status": fstatus if isinstance(fstatus, str) else shape(fstatus),
            "outcome": fdata.get("outcome"),
            "time_keys": shape(ftime),
            "has_idle_time": bool(ftime.get("idle")),
            "provider_calls_for_held": len(Provider.calls) - calls_mark}
        check("mid-execution interrupt answered 2xx", 200 <= mid["status"] < 300,
              f"status={mid['status']}")

        # Missing session boundary (reject vs unknown).
        missing = api("GET", "/api/session/ses_does_not_exist_0000", allow_error=True)
        probes["missing_session"] = {"status": missing["status"],
                                     "body_keys": shape((missing["body"] or {}).get("data", missing["body"] or {}))}
        check("missing session is 404", missing["status"] == 404,
              f"status={missing['status']}")

        # Active listing + terminal evidence shape.
        active = api("GET", "/api/session/active",
                     headers={"x-opencode-directory": quote(str(work), safe="")},
                     allow_error=True)
        probes["active_listing"] = {"status": active["status"],
                                    "body_keys": shape((active["body"] or {}))}
        check("active listing reachable", active["status"] == 200,
              f"status={active['status']}")
        probes["idle_terminal"] = {"status": final["status"],
                                   "data_keys": shape(fdata),
                                   "status_value": fstatus,
                                   "outcome": fdata.get("outcome"),
                                   "time_keys": shape(ftime),
                                   "has_idle_time": bool(ftime.get("idle"))}

        # SSE event sequence around a fresh prompt (bounded).
        seen_types = []
        stop_sse = threading.Event()

        def read_sse():
            import http.client
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=REQUEST_TIMEOUT_S)
            try:
                conn.putrequest("GET", "/api/event")
                conn.putheader("Authorization", auth)
                conn.putheader("Accept", "text/event-stream")
                conn.endheaders()
                resp = conn.getresponse()
                if resp.status != 200:
                    return
                buf, deadline = "", time.monotonic() + 25
                while not stop_sse.is_set() and time.monotonic() < deadline:
                    chunk = resp.read(1024)
                    if not chunk:
                        break
                    buf += chunk.decode(errors="replace")
                    while "\n\n" in buf:
                        block, buf = buf.split("\n\n", 1)
                        for line in block.splitlines():
                            if line.startswith("data:"):
                                try:
                                    seen_types.append(json.loads(line[5:]).get("type", "?"))
                                except json.JSONDecodeError:
                                    seen_types.append("?")
            except OSError:
                pass
            finally:
                conn.close()

        sse_thread = threading.Thread(target=read_sse, daemon=True)
        sse_thread.start()
        time.sleep(1.5)
        sse_id = "msg_probe_" + uuid.uuid4().hex[:20]
        api("POST", f"/api/session/{quote(session_id)}/prompt",
            {"text": "Contract probe SSE ping.", "id": sse_id})
        try:
            wait_for("sse turn visible", lambda: any(
                r.get("id") == sse_id for r in
                ((api("GET", f"/api/session/{quote(session_id)}/message?limit=50")["body"] or {}).get("data") or [])),
                timeout=POLL_TIMEOUT_S)
        finally:
            stop_sse.set()
            sse_thread.join(timeout=REQUEST_TIMEOUT_S)
        probes["sse_sequence"] = {"event_count": len(seen_types),
                                  "event_types": sorted(set(seen_types))[:25],
                                  "saw_inbox_delivered": "session.inbox.delivered" in seen_types,
                                  "saw_execution_event": any(t.startswith("session.execution.")
                                                             for t in seen_types)}
        check("sse stream delivered events", len(seen_types) > 0,
              f"count={len(seen_types)}")
    finally:
        if child is not None:
            child.close()
        provider.shutdown()
        provider.server_close()

    elapsed = round(time.monotonic() - started, 1)
    failed = [c for c in checks if not c["ok"]]
    result = {
        "run_id": RUN_ID,
        "script": "scripts/session-recovery-contract-probe.py",
        "script_version": 1,
        "opencode_binary": binary,
        "opencode_version": probes.get("server_info", {}).get("version", ""),
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "elapsed_s": elapsed,
        "total_budget_s": TOTAL_BUDGET_S,
        "request_timeout_s": REQUEST_TIMEOUT_S,
        "isolation": {
            "run_dir": f"temp/session-recovery-contract/{RUN_ID}",
            "ports": "random loopback (serve + fixture provider)",
            "environment": "whitelist only; HOME/USERPROFILE/APPDATA/LOCALAPPDATA/TEMP/TMP/XDG_*/OPENCODE_CONFIG_DIR/OPENCODE_DB redirected under run dir",
            "containment": "Windows Job Object (KILL_ON_JOB_CLOSE) via OwnedProcess; only the owned child is stopped",
            "network": "loopback only; NO_PROXY loopback; fixture model only, no real provider",
        },
        "result": "complete" if not failed else "failed",
        "checks": checks,
        "probes": probes,
        "provider_post_count": len(Provider.calls),
        "limits": [
            "Prompt text/attachments not exercised beyond a short text ping.",
            "Permission/form/reply flows not probed here (covered by existing recovery matrix).",
            "Single session only; multi-session pool sharing not probed.",
            "Interrupt outcome documents observed behaviour; driver mapping rules live in implementation.",
            "No visual, desktop, or performance validation.",
        ],
    }
    (OUTPUT / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(f"Artifacts: {OUTPUT}", flush=True)
    print(f"Checks: {len(checks) - len(failed)}/{len(checks)} passed in {elapsed}s", flush=True)
    if failed:
        raise SystemExit(1)


if __name__ == "__main__":
    sys.exit(main())
