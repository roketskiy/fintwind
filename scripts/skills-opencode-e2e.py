"""Run the daemon/driver explicit-skill E2E in an owned Windows Job Object.

No public OpenCode service, credentials, or real model is used. The fake
provider records the actual HTTP requests in temp/recovery-e2e/<uuid>/.
"""

import importlib.util
import argparse
import base64
import ctypes
import json
import os
from pathlib import Path
import shutil
import socket
import sys
import threading
import time
import uuid
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener


ROOT = Path(__file__).resolve().parent.parent


def verify_real(binary, report, output, helper, env):
    """Replay captured driver requests against an isolated real OpenCode.

    A scripted loopback provider captures the model's actual context. It never
    calls tools, so the test cannot pass by searching or reading skill files.
    """
    work = output / "workspace"
    work.mkdir()
    source = output / "skills"
    source.mkdir()
    markers = {}
    for skill_id in ("poteto-mode", "hidden-mode"):
        directory = source / skill_id
        directory.mkdir()
        marker = f"SKILL_CONTENT_{uuid.uuid4().hex}"
        markers[skill_id] = marker
        (directory / "SKILL.md").write_text(
            f"---\nname: Display {skill_id}\ndescription: Fixture\nmetadata:\n  opencode/autoinvoke: false\n---\n{marker}\n",
            encoding="utf-8")
        (directory / "reference.txt").write_text("Supporting resource", encoding="utf-8")
    password = uuid.uuid4().hex
    class Brain:
        def __init__(self):
            self.calls = []

        def reply(self, payload):
            self.calls.append(payload)
            return {"role": "assistant", "content": "Skills fixture completed."}

    brain = Brain()
    helper.MODEL = brain
    provider = ThreadingHTTPServer(("127.0.0.1", 0), helper.Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    config = {"model": "fixture/fixture-model", "small_model": "fixture/fixture-model",
              "skills": [str(source)],
              "permissions": [{"action": "skill", "resource": "*", "effect": "allow"}],
              "provider": {"fixture": {"npm": "@ai-sdk/openai-compatible", "name": "Skills fixture",
                "options": {"apiKey": "fixture-only", "baseURL": f"http://127.0.0.1:{provider.server_port}/v1"},
                "models": {"fixture-model": {"name": "Fixture", "tool_call": True,
                  "limit": {"context": 128000, "output": 8192}}}}}}
    env = dict(env)
    env.update({"OPENCODE_CONFIG_CONTENT": json.dumps(config),
                "OPENCODE_DB": str(output / "opencode.db"),
                "OPENCODE_CONFIG_DIR": str(output / "config"),
                "OPENCODE_SERVER_USERNAME": "opencode", "OPENCODE_SERVER_PASSWORD": password,
                "OPENCODE_DISABLE_AUTOUPDATE": "1", "OPENCODE_DISABLE_FILEWATCHER": "1",
                "OPENCODE_DISABLE_PROJECT_CONFIG": "1", "OPENCODE_CONFIG_PROJECT_DISABLE": "1",
                "NO_PROXY": "127.0.0.1,localhost,::1"})
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    http = build_opener(ProxyHandler({}))
    authorization = "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()

    def api(method, path, value=None):
        data = None if value is None else json.dumps(value).encode()
        request = Request(f"http://127.0.0.1:{port}" + path, data=data, method=method,
                          headers={"Authorization": authorization, "Content-Type": "application/json"})
        with http.open(request, timeout=15) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    child = helper.OwnedProcess(str(Path(binary).resolve()), ["serve", "--hostname", "127.0.0.1", "--port", str(port)], work, env)
    facts = []
    try:
        deadline = time.monotonic() + 45
        while True:
            try:
                info = api("GET", "/api/info")
                break
            except OSError:
                if not child.alive() or time.monotonic() > deadline:
                    raise RuntimeError("Isolated real OpenCode did not start")
                time.sleep(0.1)
        query = "?" + urlencode({"location[directory]": str(work)})
        configuration = api("GET", "/api/config" + query)
        deadline = time.monotonic() + 20
        while True:
            catalog = api("GET", "/api/skill" + query)
            if all(any(skill["id"] == skill_id for skill in catalog["data"]) for skill_id in markers):
                break
            if time.monotonic() > deadline:
                raise AssertionError({"info": info, "config": configuration, "catalog": catalog})
            time.sleep(0.2)
        for case in report["cases"]:
            session = api("POST", "/api/session", {"location": {"directory": str(work)}})["data"]["id"]
            payload = dict(case["request"]["body"])
            payload.pop("id", None)
            start = len(brain.calls)
            api("POST", f"/api/session/{session}/prompt", payload)
            deadline = time.monotonic() + 30
            while True:
                context = api("GET", f"/api/session/{session}/context")["data"]
                if "Skills fixture completed." in json.dumps(context):
                    break
                if time.monotonic() > deadline:
                    raise AssertionError({"context": context, "calls": brain.calls[start:]})
                time.sleep(0.1)
            serialized = json.dumps([call.get("messages") for call in brain.calls[start:]])
            actual = [skill["id"] for message in context if message.get("type") == "user"
                      for skill in message.get("skills", [])]
            assert actual == case["expected"], (actual, case["expected"])
            for skill_id in case["expected"]:
                assert markers[skill_id] in serialized, (skill_id, context)
                assert "reference.txt" in serialized, context
            for skill_id in markers.keys() - set(case["expected"]):
                assert markers[skill_id] not in serialized, skill_id
            facts.append({"text": case["text"], "expected": case["expected"], "context": context,
                          "providerRequests": brain.calls[start:]})
        (output / "real-opencode.json").write_text(json.dumps({"passed": True, "info": info,
            "surface": "real OpenCode; replayed driver HTTP; scripted loopback model, no tool calls", "cases": facts}, indent=2), encoding="utf-8")
        print(f"[OK] Real OpenCode loaded skill bodies: {output / 'real-opencode.json'}")
    finally:
        child.close()
        provider.shutdown()
        provider.server_close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--opencode", help="Also verify captured requests on this real OpenCode executable")
    args = parser.parse_args()
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("goal_e2e", ROOT / "scripts/goal-opencode-e2e.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    output = ROOT / "target/skills-opencode-e2e" / str(uuid.uuid4())
    output.mkdir(parents=True)
    # Allow only build/runtime essentials. Never forward provider credentials
    # or existing daemon/service connection settings into this test.
    allowed = ("PATH", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT",
               "TEMP", "TMP", "CARGO_HOME", "RUSTUP_HOME", "PROCESSOR_ARCHITECTURE",
               "NUMBER_OF_PROCESSORS", "INCLUDE", "LIB", "LIBPATH")
    env = {key: value for key, value in os.environ.items()
           if key.upper() in {name.upper() for name in allowed}}
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    for key, directory in {"USERPROFILE": "home", "HOME": "home", "APPDATA": "roaming",
                           "LOCALAPPDATA": "local", "XDG_CONFIG_HOME": "config",
                           "XDG_DATA_HOME": "data", "XDG_CACHE_HOME": "cache"}.items():
        path = output / directory
        path.mkdir(exist_ok=True)
        env[key] = str(path)
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("cargo is required on PATH")
    before = set((ROOT / "temp/recovery-e2e").glob("*/result-explicit-skills.json"))
    child = helper.OwnedProcess(cargo, ["test", "--locked", "-p", "fintwind-core", "--test",
        "opencode_recovery", "opencode_explicit_skills", "--", "--ignored", "--exact", "--nocapture"], ROOT, env)
    try:
        deadline = time.monotonic() + 600
        while child.alive():
            if time.monotonic() > deadline:
                raise TimeoutError("explicit-skill E2E timed out")
            time.sleep(0.1)
        exit_code = ctypes.c_ulong()
        child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(exit_code)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        child.close()
    artifacts = set((ROOT / "temp/recovery-e2e").glob("*/result-explicit-skills.json")) - before
    if len(artifacts) != 1:
        raise RuntimeError(f"Expected one E2E artifact, got {artifacts}")
    artifact = artifacts.pop()
    report = json.loads(artifact.read_text(encoding="utf-8"))
    assert report["passed"] and exit_code.value == 0, f"E2E failed; see {artifact} (exit {exit_code.value})"
    print(f"[OK] Explicit skills E2E: {artifact}")
    if args.opencode:
        verify_real(args.opencode, report, output, helper, env)


if __name__ == "__main__":
    main()
