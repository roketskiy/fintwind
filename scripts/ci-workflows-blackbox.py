"""Black-box checks for release CI, using private fixtures and a loopback API.

Failure inventory: inconsistent manifest/lock/tag, absent or empty notes,
published version reuse, conflicting lightweight/annotated tags and drafts,
failed/cancelled/missing/wrong-SHA/PR-only checks, newer failed runs overriding
old success, and unavailable API. Invalid or transient dispatches to the external
website repository no longer exist.
No credentials or application processes are inherited or started.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


SHA = "a" * 40
OTHER_SHA = "b" * 40
REPO = "/repos/example/fintwind"
RELEASE = f"{REPO}/releases/tags/v1.2.3"
REF = f"{REPO}/git/ref/tags/v1.2.3"
RUNS = f"{REPO}/actions/workflows/test.yml/runs"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    script = Path(__file__).resolve().with_name("release-ci.ts")
    bun = shutil.which("bun")
    if not bun:
        raise SystemExit("Bun is required")

    routes = {}
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def handle_request(self):
            path = self.path.split("?", 1)[0]
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            requests.append({"method": self.command, "path": path,
                             "body": body.decode(),
                             "auth": self.headers.get("Authorization")})
            value = routes.get((self.command, path), (404, {}))
            if callable(value):
                value = value()
            status, payload = value
            data = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data) if status != 204 else 0))
            self.end_headers()
            if status != 204:
                self.wfile.write(data)

        do_GET = handle_request
        do_POST = handle_request

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    results = []

    def run_case(name, command="prepare", *, success=True, mutate=None,
                 api=None, env_extra=None, verify=None):
        routes.clear()
        requests.clear()
        routes[("GET", f"{REPO}/releases")] = (200, [])
        routes.update(api or {})
        started = time.monotonic()
        result = {"name": name}
        with tempfile.TemporaryDirectory(prefix="ci-fixture-", dir=args.output.parent) as root:
            root = Path(root)
            (root / "Cargo.toml").write_text(
                '[package]\nname = "fintwind"\nversion = "1.2.3"\n', encoding="utf-8")
            (root / "Cargo.lock").write_text(
                'version = 4\n[[package]]\nname = "fintwind"\nversion = "1.2.3"\n',
                encoding="utf-8")
            (root / "CHANGELOG.md").write_text(
                "## [unreleased]\n\n## [1.2.3]\n\nRelease notes.\n", encoding="utf-8")
            env = {key: os.environ[key] for key in (
                "PATH", "PATHEXT", "SystemRoot", "SYSTEMROOT", "WINDIR", "COMSPEC",
                "TEMP", "TMP", "USERPROFILE", "LOCALAPPDATA", "APPDATA",
            ) if key in os.environ}
            env.update({
                "GITHUB_API_URL": f"http://127.0.0.1:{server.server_port}",
                "GITHUB_REPOSITORY": "example/fintwind", "GITHUB_SHA": SHA,
                "GITHUB_EVENT_NAME": "push", "GITHUB_REF_NAME": "v1.2.3",
                "GITHUB_OUTPUT": str(root / "outputs.txt"),
                "GITHUB_TOKEN": "fixture-read-token",
                "RELEASE_CHECK_TIMEOUT_SECONDS": "1",
                "NO_PROXY": "127.0.0.1,localhost",
            })
            env.update(env_extra or {})
            try:
                if mutate:
                    mutate(root)
                completed = subprocess.run(
                    [bun, str(script), command], cwd=root, env=env,
                    capture_output=True, text=True, encoding="utf-8", timeout=10,
                )
                result.update(returncode=completed.returncode,
                              stdout=completed.stdout, stderr=completed.stderr)
                assert (completed.returncode == 0) == success, result
                if verify:
                    verify(root, list(requests), completed)
                result["passed"] = True
            except Exception as error:
                result.update(passed=False, error=str(error))
            result["seconds"] = round(time.monotonic() - started, 3)
            results.append(result)
            print(f"[{'OK' if result['passed'] else 'X'}] {name}")

    def run_record(run_id=1, status="completed", conclusion="success", **extra):
        return {"id": run_id, "head_sha": SHA, "event": "push",
                "head_branch": "main", "status": status, "conclusion": conclusion,
                "created_at": f"2026-10-30T00:00:{run_id:02d}Z",
                "html_url": f"https://example.invalid/runs/{run_id}", **extra}

    def verify_prepare(root, calls, _completed):
        assert (root / "outputs.txt").read_text().splitlines() == [
            "version=1.2.3", "tag=v1.2.3"]
        assert (root / "release-notes.md").read_text().strip() == "Release notes."
        assert all(call["method"] == "GET" for call in calls)

    try:
        run_case("fresh tagged release", verify=verify_prepare)
        run_case("manual release without existing tag", env_extra={
            "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF_NAME": "main"},
            verify=verify_prepare)
        run_case("tag version mismatch", success=False,
                 env_extra={"GITHUB_REF_NAME": "v9.9.9"})
        run_case("lock version mismatch", success=False, mutate=lambda root:
                 (root / "Cargo.lock").write_text(
                     '[[package]]\nname="fintwind"\nversion="1.2.2"\n'))
        run_case("missing notes", success=False, mutate=lambda root:
                 (root / "CHANGELOG.md").write_text("## [unreleased]\n"))
        run_case("empty notes", success=False, mutate=lambda root:
                 (root / "CHANGELOG.md").write_text("## [1.2.3]\n\n## [1.2.2]\nOld\n"))
        run_case("non-stable version rejected", success=False, mutate=lambda root:
                 (root / "Cargo.toml").write_text(
                     '[package]\nname="fintwind"\nversion="1.2.3-beta.1"\n'))
        run_case("published release cannot be replaced", success=False,
                 api={("GET", RELEASE): (200, {"draft": False})})
        run_case("matching lightweight tag", api={
            ("GET", REF): (200, {"object": {"type": "commit", "sha": SHA}})},
            verify=verify_prepare)
        run_case("conflicting lightweight tag", success=False, api={
            ("GET", REF): (200, {"object": {"type": "commit", "sha": OTHER_SHA}})})
        run_case("matching annotated tag", api={
            ("GET", REF): (200, {"object": {"type": "tag", "sha": OTHER_SHA}}),
            ("GET", f"{REPO}/git/tags/{OTHER_SHA}"): (200, {
                "object": {"type": "commit", "sha": SHA}})}, verify=verify_prepare)
        run_case("conflicting annotated tag", success=False, api={
            ("GET", REF): (200, {"object": {"type": "tag", "sha": OTHER_SHA}}),
            ("GET", f"{REPO}/git/tags/{OTHER_SHA}"): (200, {
                "object": {"type": "commit", "sha": OTHER_SHA}})})
        run_case("matching draft can be retried", api={
            ("GET", RELEASE): (200, {"draft": True, "target_commitish": SHA})},
            verify=verify_prepare)
        run_case("conflicting draft", success=False, api={
            ("GET", RELEASE): (200, {"draft": True, "target_commitish": OTHER_SHA})})
        run_case("unpublished draft found through release listing", api={
            ("GET", f"{REPO}/releases"): (200, [{
                "tag_name": "v1.2.3", "draft": True, "target_commitish": SHA}])},
            verify=verify_prepare)
        run_case("conflicting unpublished draft blocks release", success=False, api={
            ("GET", f"{REPO}/releases"): (200, [{
                "tag_name": "v1.2.3", "draft": True, "target_commitish": OTHER_SHA}])})
        pages = []

        def paginated_releases():
            pages.append(True)
            if len(pages) == 1:
                return 200, [{"tag_name": f"v0.0.{index}"} for index in range(100)]
            return 200, [{"tag_name": "v1.2.3", "draft": True,
                          "target_commitish": OTHER_SHA}]

        run_case("draft lookup follows pagination", success=False, api={
            ("GET", f"{REPO}/releases"): paginated_releases})
        run_case("API authorization failure is not absence", success=False,
                 api={("GET", RELEASE): (403, {"message": "Forbidden"})})
        run_case("successful checks", "checks", api={
            ("GET", RUNS): (200, {"workflow_runs": [run_record()]})})
        for conclusion in ("failure", "cancelled", "timed_out", "skipped"):
            run_case(f"checks {conclusion} blocks release", "checks", success=False, api={
                ("GET", RUNS): (200, {"workflow_runs": [run_record(conclusion=conclusion)]})})
        for name, runs in (
            ("missing", []), ("wrong SHA", [run_record(head_sha=OTHER_SHA)]),
            ("PR-only", [run_record(event="pull_request")]),
            ("feature push", [run_record(head_branch="feature")]),
        ):
            run_case(f"{name} checks cannot authorize release", "checks", success=False,
                     api={("GET", RUNS): (200, {"workflow_runs": runs})})
        run_case("newer failed checks override old success", "checks", success=False, api={
            ("GET", RUNS): (200, {"workflow_runs": [
                run_record(), run_record(2, conclusion="failure")]})})
        polls = []

        def pending_then_success():
            polls.append(True)
            return 200, {"workflow_runs": [run_record(
                status="in_progress" if len(polls) == 1 else "completed",
                conclusion=None if len(polls) == 1 else "success")]}

        run_case("running checks are awaited", "checks", api={
            ("GET", RUNS): pending_then_success})
        run_case("manual same-SHA checks accepted", "checks", api={
            ("GET", RUNS): (200, {"workflow_runs": [run_record(
                event="workflow_dispatch", head_branch="v1.2.3")]})})
        run_case("running checks timeout", "checks", success=False, api={
            ("GET", RUNS): (200, {"workflow_runs": [run_record(
                status="in_progress", conclusion=None)]})})
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        report = {"passed": all(item["passed"] for item in results),
                  "total": len(results), "cases": results}
        args.output.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"Report: {args.output.resolve()}")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
