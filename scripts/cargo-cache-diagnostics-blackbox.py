"""Reproduce Cargo cache freshness with an isolated, offline path dependency.

Failure inventory: cold build not recorded, diagnostics absent on success or
failure, warm cache unnecessarily rebuilt, unchanged source with checkout-time
mtimes invalidating the cache, changed source incorrectly treated as fresh,
loss of Cargo's nonzero exit status, resource/build-script/list changes hidden
by timestamp restoration, malformed/profile-mismatched snapshots, path traversal,
and following junctions outside a package. No project target or Cargo home is used.
All compiler descendants stay inside a newly created Windows Job Object.
CI additionally requires an activated MSVC toolset, not Git's GNU link.exe.
"""

import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import time
import uuid


ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sysroot", type=Path, required=True)
    parser.add_argument("--require-msvc-env", action="store_true",
                        help="Require an activated MSVC developer environment (used in CI)")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("This reproduction requires Windows Job Objects")
    args.output = args.output.resolve()
    run = args.output.parent / ("cargo-cache-fixture-" + uuid.uuid4().hex)
    run.mkdir(parents=True)
    fixture = run / "workspace"
    gpui = fixture / "vendor/gpui-pre/gpui-pre-0.3.8"
    windows = fixture / "vendor/gpui-pre/gpui-pre-windows-0.3.8"
    source = gpui / "src/lib.rs"
    source.parent.mkdir(parents=True)
    (fixture / "src").mkdir()
    (fixture / "Cargo.toml").write_text(
        '[package]\nname="fintwind"\nversion="0.1.0"\nedition="2024"\n'
        '[workspace]\nmembers=["daemon"]\n'
        '[dependencies]\ngpui-pre={path="vendor/gpui-pre/gpui-pre-0.3.8"}\n', encoding="utf-8")
    (fixture / "daemon/src").mkdir(parents=True)
    (fixture / "daemon/Cargo.toml").write_text(
        '[package]\nname="fintwind-daemon"\nversion="0.1.0"\nedition="2024"\n',
        encoding="utf-8")
    (fixture / "daemon/src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
    (gpui / "Cargo.toml").write_text(
        '[package]\nname="gpui-pre"\nversion="0.1.0"\nedition="2024"\n'
        '[dependencies]\ngpui-pre-windows={path="../gpui-pre-windows-0.3.8"}\n', encoding="utf-8")
    (gpui / "assets").mkdir()
    resource = gpui / "assets/input.txt"
    resource.write_text("initial resource\n", encoding="utf-8")
    build_script = gpui / "build.rs"
    build_script.write_text(
        'fn main() { println!("cargo:rerun-if-changed=assets"); '
        'println!("cargo:rerun-if-env-changed=GPUI_CACHE_TEST_VALUE"); }\n', encoding="utf-8")
    (windows / "src").mkdir(parents=True)
    (windows / "Cargo.toml").write_text(
        '[package]\nname="gpui-pre-windows"\nversion="0.1.0"\nedition="2024"\n', encoding="utf-8")
    # Like the real Windows GPUI debug script, this emits no rerun directives.
    (windows / "build.rs").write_text("fn main() {}\n", encoding="utf-8")
    (windows / "src/lib.rs").write_text("pub fn value() -> u32 { 7 }\n", encoding="utf-8")
    source.write_text("pub fn value() -> u32 { gpui_pre_windows::value() }\n", encoding="utf-8")
    (fixture / "src/main.rs").write_text(
        'fn main() { println!("{}", gpui_pre::value()); }\n', encoding="utf-8")
    (fixture / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname="fintwind"\nversion="0.1.0"\n'
        'dependencies=["gpui-pre"]\n[[package]]\nname="fintwind-daemon"\nversion="0.1.0"\n'
        '[[package]]\nname="gpui-pre"\nversion="0.1.0"\ndependencies=["gpui-pre-windows"]\n'
        '[[package]]\nname="gpui-pre-windows"\nversion="0.1.0"\n',
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
    environment = {key.upper(): value for key, value in os.environ.items() if key.upper() in {
        "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS", "PROGRAMFILES", "PROGRAMFILES(X86)", "VSINSTALLDIR",
        "VCTOOLSINSTALLDIR", "VCINSTALLDIR", "WINDOWSSDKDIR", "WINDOWSSDKVERSION",
        "VSCMD_ARG_HOST_ARCH", "VSCMD_ARG_TGT_ARCH", "INCLUDE", "LIB", "LIBPATH",
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
    original_bytes = {path: path.read_bytes() for path in files}
    original_times = {path: path.stat().st_mtime_ns for path in files}
    original_hash = hashlib.sha256(source.read_bytes()).hexdigest()
    results = []
    stamp_operations = []

    def run_owned(executable, arguments):
        child = support.OwnedProcess(executable, arguments, fixture, environment)
        try:
            deadline = time.monotonic() + 90
            while child.alive() and time.monotonic() < deadline:
                time.sleep(0.05)
            if child.alive():
                raise TimeoutError("Owned fixture process exceeded 90 seconds")
            status = ctypes.c_ulong()
            child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
            if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
                raise ctypes.WinError(ctypes.get_last_error())
            code = status.value
        finally:
            child.close()
        return code

    def execute(name, *, expect_compile=None, expect_failure=False, mode="Test"):
        logs = run / name
        code = run_owned(pwsh, [
            "-NoProfile", "-File", str(ROOT / "scripts/cargo-cache-diagnostics.ps1"),
            "-Mode", mode, "-LogDirectory", str(logs),
        ])
        stderr = (logs / "cargo.stderr.log").read_text(encoding="utf-8-sig", errors="replace")
        metadata = json.loads((logs / "diagnostics.json").read_text(encoding="utf-8-sig"))
        compiled = "Compiling gpui-pre v" in stderr or "Checking gpui-pre v" in stderr
        windows_compiled = "Compiling gpui-pre-windows v" in stderr or "Checking gpui-pre-windows v" in stderr
        assert code == (101 if expect_failure else 0), (name, code, stderr)
        assert metadata["exitCode"] == code, (name, metadata)
        assert metadata["mode"] == mode, (name, metadata)
        expected_profile = "release" if mode == "StableRelease" else "debug"
        assert metadata["profile"] == expected_profile, (name, metadata)
        assert metadata["sourceStampsBeforeCargo"], (name, metadata)
        assert "fingerprint" in stderr, (name, "fingerprint tracing absent")
        if expect_compile is not None:
            assert compiled == expect_compile, (name, compiled, stderr)
            if not expect_compile:
                assert not windows_compiled, (name, "Windows GPUI unexpectedly rebuilt", stderr)
        assert (logs / "cargo.stdout.log").is_file()
        assert (logs / "fingerprint-summary.log").is_file()
        result = {"name": name, "exitCode": code, "gpuiCompiled": compiled,
                  "windowsGpuiCompiled": windows_compiled,
                  "mode": mode, "logDirectory": str(logs), "passed": True}
        results.append(result)
        print(f"[OK] {name}: gpuiCompiled={compiled}, exitCode={code}")
        return stderr

    def restore_artifacts(cache="cache", profile="debug"):
        target = fixture / "target"
        # This directory was created by this run beneath its UUID-owned fixture.
        shutil.rmtree(target)
        for name in ("deps", "build", ".fingerprint"):
            shutil.copytree(run / cache / name, target / profile / name)

    def save_artifacts(cache, profile="debug"):
        for name in ("deps", "build", ".fingerprint"):
            shutil.copytree(fixture / "target" / profile / name, run / cache / name)

    def stamp_state(profile="debug"):
        return fixture / "target" / profile / ".fingerprint/fintwind-gpui-source-stamps.json"

    def stamps(name, operation, *, profile="debug", restored=None):
        report_path = run / (name + "-stamps.json")
        code = run_owned(sys.executable, [
            str(ROOT / "scripts/cargo-cache-source-stamps.py"), operation,
            "--profile", profile, "--report", str(report_path),
        ])
        assert code == 0, (name, code)
        data = json.loads(report_path.read_text(encoding="utf-8"))
        assert data["operation"] == operation and data["profile"] == profile, data
        if restored is not None:
            assert sum(item["status"] == "restored" for item in data["packages"]) == restored, data
        stamp_operations.append({"name": name, "passed": True, **data})
        return data

    def times_for(root):
        paths = [root, *(path for path in files if path.is_relative_to(root) and path.exists())]
        return {str(path): path.stat().st_mtime_ns for path in paths}

    def prepare_fixed_case(cache="fixed-cache", profile="debug"):
        for path, content in original_bytes.items():
            path.write_bytes(content)
        for path in files:
            os.utime(path, None)
        for root in (gpui, windows):
            for directory in sorted((path for path in root.rglob("*") if path.is_dir()), reverse=True):
                os.utime(directory, None)
            os.utime(root, None)
        restore_artifacts(cache, profile)

    report = {"passed": False, "fixture": str(run), "cases": results,
              "stampOperations": stamp_operations}
    try:
        linker = shutil.which("link.exe", path=environment["PATH"])
        toolset = environment.get("VCTOOLSINSTALLDIR")
        native_linker = bool(linker and toolset and
                             Path(linker).resolve().is_relative_to(Path(toolset).resolve()))
        report["msvc"] = {
            "required": args.require_msvc_env, "linker": linker, "toolset": toolset,
            "nativeLinker": native_linker, "libraryPathsConfigured": bool(environment.get("LIB")),
            "targetArch": environment.get("VSCMD_ARG_TGT_ARCH"),
        }
        if args.require_msvc_env and (not native_linker or not environment.get("LIB")):
            raise RuntimeError(
                "Activate the MSVC developer environment before this isolated check. "
                f"Expected link.exe under VCToolsInstallDir with LIB configured; got {linker!r}. "
                "Git's GNU link.exe is not an MSVC linker."
            )
        print(f"[INFO] Fixture linker on PATH: {linker}")
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

        # The fix must protect the whole package, including a default-scanning
        # build script and a watched resource directory, not only build.rs.
        stamps("missing-state", "restore", restored=0)
        for path, content in original_bytes.items():
            path.write_bytes(content)
        stamps("fixed-baseline", "capture")
        execute("fixed-baseline", expect_compile=True)
        save_artifacts("fixed-cache")

        prepare_fixed_case()
        app_stamp = (fixture / "src/main.rs").stat().st_mtime_ns
        stamps("unchanged-package", "restore", restored=2)
        assert (fixture / "src/main.rs").stat().st_mtime_ns == app_stamp
        stamps("unchanged-package", "capture")
        execute("unchanged-package-reuses-gpui", expect_compile=False)

        mutations = {
            "rust-source": lambda: source.write_text("pub fn value() -> u32 { 9 }\n"),
            "build-script": lambda: build_script.write_text("fn main() {}\n"),
            "resource": lambda: resource.write_text("changed resource\n"),
            "new-file": lambda: (gpui / "extra.txt").write_text("new input\n"),
            "deleted-file": resource.unlink,
        }
        for name, mutate in mutations.items():
            prepare_fixed_case()
            mutate()
            before = times_for(gpui)
            stamps(name, "restore", restored=1)
            assert times_for(gpui) == before, name
            stamps(name, "capture")
            execute(name + "-still-rebuilds", expect_compile=True)
            extra = gpui / "extra.txt"
            if extra.exists():
                extra.unlink()

        prepare_fixed_case()
        (windows / "src/lib.rs").write_text("pub fn value() -> u32 { 42 }\n", encoding="utf-8")
        windows_stamps = times_for(windows)
        stamps("windows-package-change", "restore", restored=1)
        assert times_for(windows) == windows_stamps
        stamps("windows-package-change", "capture")
        execute("windows-package-change-invalidates-dependent-gpui", expect_compile=True)

        prepare_fixed_case()
        original_mode = source.stat().st_mode
        try:
            source.chmod(stat.S_IREAD)
            before = times_for(gpui)
            stamps("file-mode-change", "restore", restored=1)
            assert times_for(gpui) == before
        finally:
            source.chmod(original_mode)

        prepare_fixed_case()
        (fixture / "src/main.rs").write_text(
            'fn main() { println!("{}", gpui_pre::value() + 1); }\n', encoding="utf-8")
        stamps("application-change", "restore", restored=2)
        stamps("application-change", "capture")
        app_log = execute("application-change-does-not-rebuild-gpui", expect_compile=False)
        assert "Compiling fintwind v" in app_log

        prepare_fixed_case()
        environment["GPUI_CACHE_TEST_VALUE"] = "changed"
        stamps("environment-change", "restore", restored=2)
        stamps("environment-change", "capture")
        execute("environment-change-still-rebuilds", expect_compile=True)
        environment.pop("GPUI_CACHE_TEST_VALUE")

        outside = run / "outside"
        outside.mkdir()
        sentinel = outside / "sentinel.txt"
        sentinel.write_text("must not be touched\n")
        sentinel_time = sentinel.stat().st_mtime_ns
        prepare_fixed_case()
        link = gpui / "linked"
        assert run_owned(environment["COMSPEC"], [
            "/d", "/c", "mklink", "/J", str(link), str(outside),
        ]) == 0
        stamps("junction-is-not-followed", "restore", restored=1)
        assert sentinel.stat().st_mtime_ns == sentinel_time
        link.rmdir()

        for name in ("malformed", "profile-mismatch", "traversal", "invalid-timestamp"):
            prepare_fixed_case()
            state = stamp_state()
            data = json.loads(state.read_text(encoding="utf-8"))
            if name == "malformed":
                state.write_text("not JSON")
            else:
                if name == "profile-mismatch":
                    data["profile"] = "release"
                elif name == "traversal":
                    data["packages"][0]["entries"][1]["path"] = "../../outside/sentinel.txt"
                else:
                    data["packages"][0]["entries"][0]["mtime_ns"] = "invalid"
                state.write_text(json.dumps(data), encoding="utf-8")
            before = (times_for(gpui), times_for(windows))
            stamps(name, "restore", restored=0)
            assert before == (times_for(gpui), times_for(windows)), name
            assert sentinel.stat().st_mtime_ns == sentinel_time

        prepare_fixed_case()
        stamps("release-baseline", "capture", profile="release")
        execute("fixed-release-baseline", mode="StableRelease", expect_compile=True)
        save_artifacts("fixed-release", "release")
        prepare_fixed_case("fixed-release", "release")
        stamps("release-unchanged", "restore", profile="release", restored=2)
        stamps("release-unchanged", "capture", profile="release")
        execute("release-unchanged-reuses-gpui", mode="StableRelease", expect_compile=False)
        report["passed"] = True
    except BaseException as error:
        report["error"] = str(error)
        raise
    finally:
        report["cargoCases"] = len(results)
        report["stampOperationsCount"] = len(stamp_operations)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"Cargo cases: {len(results)}; snapshot operations: {len(stamp_operations)}")
        print(f"Report: {args.output}")


if __name__ == "__main__":
    main()
