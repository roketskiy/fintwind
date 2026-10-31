"""Reproduce Cargo cache freshness with an isolated, offline path dependency.

Failure inventory: cold build not recorded, diagnostics absent on success or
failure, warm cache unnecessarily rebuilt, unchanged source with checkout-time
mtimes invalidating the cache, changed source incorrectly treated as fresh,
and loss of Cargo's nonzero exit status. No project target or Cargo home is used.
All compiler descendants stay inside a newly created Windows Job Object.
"""

import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import time
import uuid


ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sysroot", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("This reproduction requires Windows Job Objects")
    args.output = args.output.resolve()
    run = args.output.parent / ("cargo-cache-fixture-" + uuid.uuid4().hex)
    run.mkdir(parents=True)
    fixture = run / "workspace"
    source = fixture / "vendor/gpui-pre/src/lib.rs"
    source.parent.mkdir(parents=True)
    (fixture / "src").mkdir()
    (fixture / "Cargo.toml").write_text(
        '[package]\nname="fintwind"\nversion="0.1.0"\nedition="2024"\n'
        '[workspace]\nmembers=["daemon"]\n'
        '[dependencies]\ngpui-pre={path="vendor/gpui-pre"}\n', encoding="utf-8")
    (fixture / "daemon/src").mkdir(parents=True)
    (fixture / "daemon/Cargo.toml").write_text(
        '[package]\nname="fintwind-daemon"\nversion="0.1.0"\nedition="2024"\n',
        encoding="utf-8")
    (fixture / "daemon/src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
    (fixture / "vendor/gpui-pre/Cargo.toml").write_text(
        '[package]\nname="gpui-pre"\nversion="0.1.0"\nedition="2024"\n', encoding="utf-8")
    source.write_text("pub fn value() -> u32 { 7 }\n", encoding="utf-8")
    (fixture / "src/main.rs").write_text(
        'fn main() { println!("{}", gpui_pre::value()); }\n', encoding="utf-8")
    (fixture / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname="fintwind"\nversion="0.1.0"\n'
        'dependencies=["gpui-pre"]\n[[package]]\nname="fintwind-daemon"\nversion="0.1.0"\n'
        '[[package]]\nname="gpui-pre"\nversion="0.1.0"\n',
        encoding="utf-8")

    spec = importlib.util.spec_from_file_location("owned_cargo_process", ROOT / "scripts/goal-opencode-e2e.py")
    support = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(support)
    pwsh = shutil.which("pwsh")
    if not pwsh:
        raise RuntimeError("pwsh is required")
    cargo_bin = args.sysroot.resolve() / "bin"
    if not (cargo_bin / "cargo.exe").is_file() or not (cargo_bin / "rustc.exe").is_file():
        raise RuntimeError("sysroot must contain the actual Cargo and rustc executables")
    environment = {key: value for key, value in os.environ.items() if key.upper() in {
        "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS", "PROGRAMFILES", "PROGRAMFILES(X86)", "VSINSTALLDIR",
        "VCTOOLSINSTALLDIR", "WINDOWSSDKDIR", "INCLUDE", "LIB", "LIBPATH",
    }}
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "CARGO_HOME"):
        path = run / key
        path.mkdir()
        environment[key] = str(path)
    environment.update({
        "PATH": str(cargo_bin) + os.pathsep + environment.get("PATH", ""),
        "RUSTC": str(cargo_bin / "rustc.exe"),
        "RUSTDOC": str(cargo_bin / "rustdoc.exe"),
        "CARGO_NET_OFFLINE": "true", "CARGO_INCREMENTAL": "0",
        "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_TEST_DEBUG": "0",
        "CARGO_TARGET_DIR": str(fixture / "target"),
    })
    files = [path for path in fixture.rglob("*") if path.is_file()]
    original_times = {path: path.stat().st_mtime_ns for path in files}
    original_hash = hashlib.sha256(source.read_bytes()).hexdigest()
    results = []

    def execute(name, *, expect_compile=None, expect_failure=False, mode="Test"):
        logs = run / name
        child = support.OwnedProcess(pwsh, [
            "-NoProfile", "-File", str(ROOT / "scripts/cargo-cache-diagnostics.ps1"),
            "-Mode", mode, "-LogDirectory", str(logs),
        ], fixture, environment)
        try:
            deadline = time.monotonic() + 90
            while child.alive() and time.monotonic() < deadline:
                time.sleep(0.05)
            if child.alive():
                raise TimeoutError(f"Owned Cargo fixture exceeded 90 seconds: {logs}")
            status = ctypes.c_ulong()
            child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
            if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
                raise ctypes.WinError(ctypes.get_last_error())
            code = status.value
        finally:
            child.close()
        stderr = (logs / "cargo.stderr.log").read_text(encoding="utf-8-sig", errors="replace")
        metadata = json.loads((logs / "diagnostics.json").read_text(encoding="utf-8-sig"))
        compiled = "Compiling gpui-pre v" in stderr or "Checking gpui-pre v" in stderr
        assert code == (101 if expect_failure else 0), (name, code, stderr)
        assert metadata["exitCode"] == code, (name, metadata)
        assert metadata["mode"] == mode, (name, metadata)
        expected_profile = "release" if mode == "StableRelease" else "debug"
        assert metadata["profile"] == expected_profile, (name, metadata)
        assert metadata["sourceStampsBeforeCargo"], (name, metadata)
        assert "fingerprint" in stderr, (name, "fingerprint tracing absent")
        if expect_compile is not None:
            assert compiled == expect_compile, (name, compiled, stderr)
        assert (logs / "cargo.stdout.log").is_file()
        assert (logs / "fingerprint-summary.log").is_file()
        result = {"name": name, "exitCode": code, "gpuiCompiled": compiled,
                  "mode": mode, "logDirectory": str(logs), "passed": True}
        results.append(result)
        print(f"[OK] {name}: gpuiCompiled={compiled}, exitCode={code}")
        return stderr

    def restore_artifacts():
        target = fixture / "target"
        # This directory was created by this run beneath its UUID-owned fixture.
        shutil.rmtree(target)
        for name in ("deps", "build", ".fingerprint"):
            shutil.copytree(run / "cache" / name, target / "debug" / name)

    report = {"passed": False, "fixture": str(run), "cases": results}
    try:
        execute("cold", expect_compile=True)
        execute("same-checkout-warm", expect_compile=False)
        for name in ("deps", "build", ".fingerprint"):
            shutil.copytree(fixture / "target/debug" / name, run / "cache" / name)
        time.sleep(0.1)
        for path in files:
            os.utime(path, None)
        assert hashlib.sha256(source.read_bytes()).hexdigest() == original_hash
        restore_artifacts()
        dirty = execute("unchanged-checkout-mtimes", expect_compile=True)
        assert "stale" in dirty.lower() or "FsStatusOutdated" in dirty, dirty
        report["unchangedSourceDirtyReasons"] = [line for line in dirty.splitlines()
            if any(marker in line.lower() for marker in ("stale", "dirty", "outdated"))]
        for path, stamp in original_times.items():
            os.utime(path, ns=(path.stat().st_atime_ns, stamp))
        restore_artifacts()
        execute("unchanged-original-mtimes-control", expect_compile=False)
        source.write_text("pub fn value() -> u32 { 8 }\n", encoding="utf-8")
        execute("changed-source", expect_compile=True)
        source.write_text("this is not Rust\n", encoding="utf-8")
        execute("compiler-error-is-preserved", expect_failure=True)
        source.write_text("pub fn value() -> u32 { 8 }\n", encoding="utf-8")
        execute("stable-release-cold", mode="StableRelease", expect_compile=True)
        execute("stable-release-warm", mode="StableRelease", expect_compile=False)
        report["passed"] = True
    except BaseException as error:
        report["error"] = str(error)
        raise
    finally:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"Report: {args.output}")


if __name__ == "__main__":
    main()
