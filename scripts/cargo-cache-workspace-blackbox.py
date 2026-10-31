"""Offline Windows black-box checks for whole-workspace cache safety.

Failure inventory: checkout mtimes rebuilding unchanged local crates/examples;
changed bytes with an old mtime being hidden; external locale/resource/migration
inputs not invalidating consumers; additions/deletions/renames hidden by restored
directories; docs causing duplicate keys; build-time edits acquiring trusted
snapshots; malformed/context-mismatched/traversal snapshots; following junctions;
missing artifacts; lost failure codes; buffered progress; unbounded owned trees.
All data is private and every child is contained in a new Windows Job Object.
"""

import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import time
import uuid


ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sysroot', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('Windows Job Objects are required')
    run = args.output.resolve().parent / ('workspace-cache-' + uuid.uuid4().hex)
    workspace = run / 'workspace'
    workspace.mkdir(parents=True)
    content = {
        'Cargo.toml': '[package]\nname="cache-app"\nversion="0.1.0"\nedition="2024"\n'
                      '[workspace]\nmembers=["crates/local"]\n'
                      '[dependencies]\nlocal={path="crates/local"}\n',
        'Cargo.lock': 'version=4\n[[package]]\nname="cache-app"\nversion="0.1.0"\n'
                      'dependencies=["local"]\n[[package]]\nname="local"\nversion="0.1.0"\n',
        'src/main.rs': 'fn main() { assert_eq!(local::value(), 7); }\n',
        'CHANGELOG.md': '# Embedded release notes\n',
        'examples/probe.rs': 'fn main() { println!("{}", local::value()); }\n',
        'crates/local/Cargo.toml': '[package]\nname="local"\nversion="0.1.0"\nedition="2024"\n',
        'crates/local/src/lib.rs': 'pub fn value() -> u32 { 7 }\n'
                                 'pub const LOCALE: &str = include_str!("../../../locales/en.yml");\n'
                                 'pub const SQL: &str = include_str!("../../../db/migrations/0000.sql");\n'
                                 'pub const NOTES: &str = include_str!("../../../CHANGELOG.md");\n',
        'crates/local/build.rs': 'fn main() { println!("cargo:rerun-if-changed=../../resources"); '
                                 'println!("cargo:rerun-if-env-changed=CACHE_BLACKBOX_VALUE"); }\n',
        'locales/en.yml': 'title: initial\n',
        'db/migrations/0000.sql': 'select 1;\n',
        'resources/input.txt': 'resource\n',
        'docs/readme.md': 'Documentation.\n',
    }
    for name, text in content.items():
        path = workspace / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8')
    spec = importlib.util.spec_from_file_location('owned_workspace', ROOT / 'scripts/goal-opencode-e2e.py')
    support = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(support)
    env = {key.upper(): value for key, value in os.environ.items() if key.upper() in {
        'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATH', 'PATHEXT', 'PROCESSOR_ARCHITECTURE',
        'NUMBER_OF_PROCESSORS', 'PROGRAMFILES', 'PROGRAMFILES(X86)', 'VSINSTALLDIR',
        'VCTOOLSINSTALLDIR', 'VCINSTALLDIR', 'WINDOWSSDKDIR', 'WINDOWSSDKVERSION',
        'VSCMD_ARG_HOST_ARCH', 'VSCMD_ARG_TGT_ARCH', 'INCLUDE', 'LIB', 'LIBPATH',
    }}
    for name in ('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'CARGO_HOME'):
        path = run / name
        path.mkdir()
        env[name] = str(path)
    binary = args.sysroot.resolve() / 'bin'
    env.update(PATH=str(binary) + os.pathsep + env.get('PATH', ''),
               RUSTC=str(binary / 'rustc.exe'), RUSTDOC=str(binary / 'rustdoc.exe'),
               CARGO_TARGET_DIR=str(workspace / 'target'), CARGO_NET_OFFLINE='true',
               CARGO_INCREMENTAL='0', CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0',
               CARGO_PROFILE_DEV_OPT_LEVEL='0', CARGO_PROFILE_TEST_OPT_LEVEL='0')
    env['GITHUB_STEP_SUMMARY'] = str(run / 'summary.md')
    pwsh = shutil.which('pwsh')
    cases = []
    report = {'passed': False, 'fixture': str(run), 'cases': cases}

    def owned(executable, arguments, timeout=60, observe=None):
        child = support.OwnedProcess(executable, arguments, workspace, env)
        try:
            deadline = time.monotonic() + timeout
            while child.alive() and time.monotonic() < deadline:
                if observe:
                    observe()
                time.sleep(0.03)
            if child.alive():
                raise TimeoutError('Owned workspace check exceeded its deadline')
            status = ctypes.c_ulong()
            child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
            if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
                raise ctypes.WinError(ctypes.get_last_error())
            return status.value
        finally:
            child.close()

    def snapshot(name, operation, *, success=True, extra=()):
        output = run / (name + '.json')
        code = owned(sys.executable, [str(ROOT / 'scripts/cargo-cache-workspace.py'), operation,
                                     '--profile', 'debug', '--report', str(output), *extra])
        assert (code == 0) == success, (name, code)
        data = json.loads(output.read_text(encoding='utf-8'))
        return data

    def cargo(name, *, fresh=False, failure=False, timeout=45):
        logs = run / name
        code = owned(pwsh, ['-NoProfile', '-File', str(ROOT / 'scripts/cargo-cache-diagnostics.ps1'),
                           '-Mode', 'Test', '-LogDirectory', str(logs),
                           '-TimeoutSeconds', str(timeout)], timeout + 15)
        assert code == (101 if failure else 0), (name, code)
        stderr = (logs / 'cargo.stderr.log').read_text(encoding='utf-8-sig')
        if fresh:
            assert 'Compiling ' not in stderr, (name, stderr)
        meta = json.loads((logs / 'diagnostics.json').read_text(encoding='utf-8-sig'))
        assert meta['exitCode'] == code and meta['phases'], (name, meta)
        cases.append({'name': name, 'passed': True, 'seconds': meta['seconds']})
        return logs, stderr

    def reset():
        target = workspace / 'target'
        shutil.rmtree(target)
        shutil.copytree(run / 'baseline-target', target)
        for name, text in content.items():
            (workspace / name).write_text(text, encoding='utf-8')
        for directory in sorted((p for p in workspace.rglob('*') if p.is_dir()
                                 and not p.is_relative_to(target)), reverse=True):
            os.utime(directory, None)

    try:
        # Fake developer commands exercise native-arch selection/export only;
        # they never execute a mock linker or claim to compile ARM64 code.
        for mode, arch in (('x64', 'x64'), ('arm64', 'arm64'), ('wrong-linker', 'x64'),
                           ('missing-libraries', 'x64'), ('wrong-arch', 'arm64')):
            toolset = run / ('toolset-' + mode)
            tool_bin = toolset / 'bin'
            tool_bin.mkdir(parents=True)
            (tool_bin / 'link.exe').write_bytes(b'fixture-not-executed')
            developer = run / ('developer-' + mode + '.cmd')
            developer.write_text(
                '@echo off\n'
                f'if not "%*"=="-no_logo -arch={arch} -host_arch={arch}" exit /b 12\n'
                f'set "VCTOOLSINSTALLDIR={toolset}"\n'
                + (f'set "PATH={tool_bin};%PATH%"\n' if mode != 'wrong-linker' else '')
                + ('set "LIB="\n' if mode == 'missing-libraries' else 'set "LIB=fixture-library"\n')
                + f'set "VSCMD_ARG_HOST_ARCH={arch}"\n'
                + f'set "VSCMD_ARG_TGT_ARCH={"x64" if mode == "wrong-arch" else arch}"\n'
                + 'set "FINTWIND_PRIVATE_VALUE=not-exported"\nexit /b 0\n', encoding='ascii')
            export = run / ('export-' + mode + '.env')
            export.write_text('')
            env['GITHUB_ENV'] = str(export)
            code = owned(sys.executable, [str(ROOT / 'scripts/cargo-cache-msvc.py'), '--arch', arch,
                                         '--dev-command', str(developer)])
            expected_success = mode in ('x64', 'arm64')
            assert (code == 0) == expected_success, (mode, code)
            exported = export.read_text(encoding='utf-8')
            assert 'FINTWIND_PRIVATE_VALUE' not in exported and 'not-exported' not in exported
            if expected_success:
                assert f'VSCMD_ARG_TGT_ARCH={arch}' in exported
            else:
                assert not exported, (mode, exported)
            cases.append({'name': 'native-msvc-' + mode, 'passed': True})
        env.pop('GITHUB_ENV')
        first = snapshot('identity', 'prepare')
        snapshot('capture', 'capture')
        cargo('cold')
        summary = (run / 'summary.md').read_text(encoding='utf-8-sig')
        assert '### Cargo Test' in summary and '| Compile seconds |' in summary, summary
        assert snapshot('verify', 'verify')['cacheable']
        assert not snapshot('wrong-save-key', 'verify', extra=('--expected-key', 'different-key'))['cacheable']
        shutil.copytree(workspace / 'target', run / 'baseline-target')
        reset()
        snapshot('unchanged', 'restore')
        cargo('all-local-and-examples-fresh', fresh=True)
        reset()
        (workspace / 'docs/readme.md').write_text('Different docs.\n')
        assert snapshot('docs-key', 'prepare')['inputHash'] == first['inputHash']
        snapshot('docs', 'restore')
        cargo('documentation-only-is-fresh', fresh=True)
        for name, path, text in (
            ('rust', 'crates/local/src/lib.rs', content['crates/local/src/lib.rs'] + '// changed\n'),
            ('locale', 'locales/en.yml', 'title: changed\n'),
            ('migration', 'db/migrations/0000.sql', 'select 2;\n'),
            ('resource', 'resources/input.txt', 'changed\n'),
            ('embedded-changelog', 'CHANGELOG.md', '# Updated embedded notes\n'),
        ):
            reset()
            changed = workspace / path
            changed.write_text(text, encoding='utf-8')
            os.utime(changed, ns=(1_000_000_000, 1_000_000_000))
            assert snapshot(name + '-key', 'prepare')['inputHash'] != first['inputHash']
            snapshot(name, 'restore')
            _, stderr = cargo(name + '-still-rebuilds')
            assert 'Compiling local v' in stderr, (name, stderr)
        for mutation in ('add', 'delete', 'rename'):
            reset()
            path = workspace / 'resources/input.txt'
            if mutation == 'add':
                (workspace / 'resources/extra.txt').write_text('new\n')
            elif mutation == 'delete':
                path.unlink()
            else:
                path.rename(workspace / 'resources/renamed.txt')
            snapshot(mutation, 'restore')
            _, stderr = cargo(mutation + '-directory-change')
            assert 'Compiling local v' in stderr
            for extra in ('extra.txt', 'renamed.txt'):
                (workspace / 'resources' / extra).unlink(missing_ok=True)
        reset()
        snapshot('capture-before-edit', 'capture')
        (workspace / 'src/main.rs').write_text('fn main() {}\n')
        assert not snapshot('post-build-edit', 'verify')['cacheable']
        cases.append({'name': 'build-time-edit-rejects-save', 'passed': True})
        reset()
        env['CACHE_BLACKBOX_VALUE'] = 'different'
        snapshot('env', 'restore')
        _, stderr = cargo('environment-input-still-rebuilds')
        assert 'Compiling local v' in stderr
        env.pop('CACHE_BLACKBOX_VALUE')
        for mode in ('corrupt', 'traversal', 'context', 'profile', 'timestamp'):
            reset()
            state = workspace / 'target/debug/.fingerprint/fintwind-workspace-inputs.json'
            data = json.loads(state.read_text())
            if mode == 'corrupt':
                state.write_text('invalid JSON')
            else:
                if mode == 'traversal':
                    data['entries'][0]['path'] = '../../escape'
                elif mode == 'context':
                    data['context']['workspace'] = 'different-checkout'
                elif mode == 'profile':
                    data['profile'] = 'release'
                else:
                    data['entries'][0]['mtime_ns'] = 'invalid'
                state.write_text(json.dumps(data))
            before = (workspace / 'src/main.rs').stat().st_mtime_ns
            assert not snapshot(mode, 'restore')['restoredFiles']
            assert (workspace / 'src/main.rs').stat().st_mtime_ns == before
            cases.append({'name': mode + '-safe-fallback', 'passed': True})
        reset()
        outside = run / 'outside'
        outside.mkdir()
        sentinel = outside / 'sentinel'
        sentinel.write_text('untouched')
        before = sentinel.stat().st_mtime_ns
        link = workspace / 'resources/linked'
        assert owned(env['COMSPEC'], ['/d', '/c', 'mklink', '/J', str(link), str(outside)]) == 0
        assert not snapshot('junction', 'restore')['cacheable']
        assert sentinel.stat().st_mtime_ns == before
        link.rmdir()
        cases.append({'name': 'junction-not-followed', 'passed': True})
        reset()
        (workspace / 'src/main.rs').write_text('not Rust\n')
        cargo('compiler-error-preserved', failure=True)
        reset()
        (workspace / 'crates/local/build.rs').write_text(
            'fn main() { println!("cargo:warning=live-progress-sentinel"); '
            'std::thread::sleep(std::time::Duration::from_secs(2)); }\n')
        logs, _ = cargo('live-progress')
        live = json.loads((logs / 'phases.json').read_text())
        assert live['liveLinesBeforeExit'] > 0 and live['compileSeconds'] is not None, live
        cases.append({'name': 'progress-visible-before-exit', 'passed': True})
        reset()
        # Observe the public PowerShell entry point from a separate process,
        # rather than trusting the live supervisor's own counters alone.
        (workspace / 'crates/local/build.rs').write_text(
            'fn main() { std::thread::sleep(std::time::Duration::from_secs(3)); }\n')
        console = run / 'live-console.log'
        log_directory = run / 'externally-observed-live'
        launcher = run / 'observe-console.ps1'
        launcher.write_text(
            "& '" + pwsh.replace("'", "''") + "' -NoProfile -File '"
            + str(ROOT / 'scripts/cargo-cache-diagnostics.ps1').replace("'", "''")
            + "' -Mode Test -LogDirectory '" + str(log_directory).replace("'", "''")
            + "' 1> '" + str(console).replace("'", "''") + "'\nexit $LASTEXITCODE\n",
            encoding='utf-8')
        observed = []

        def observe_console():
            if console.exists() and 'Compiling local v' in console.read_text(encoding='utf-8-sig', errors='replace'):
                observed.append(time.monotonic())

        assert owned(pwsh, ['-NoProfile', '-File', str(launcher)], observe=observe_console) == 0
        assert observed, 'The public CI entry point buffered compile progress until exit'
        cases.append({'name': 'external-console-progress-before-exit', 'passed': True,
                      'transcript': str(console)})
        reset()
        (workspace / 'crates/local/build.rs').write_text(
            'fn main() { for _ in 0..8000 { println!("cargo:warning=output-pressure"); } }\n')
        cargo('large-output-does-not-deadlock')
        reset()
        (workspace / 'crates/local/build.rs').write_text(
            'fn main() { std::thread::sleep(std::time::Duration::from_secs(60)); }\n')
        logs = run / 'owned-timeout'
        started = time.monotonic()
        code = owned(pwsh, ['-NoProfile', '-File', str(ROOT / 'scripts/cargo-cache-diagnostics.ps1'),
                           '-Mode', 'Test', '-LogDirectory', str(logs), '-TimeoutSeconds', '4'], 20)
        assert code == 124 and time.monotonic() - started < 15, code
        timed_out = json.loads((logs / 'phases.json').read_text())
        assert timed_out['timedOut'] and timed_out['exitCode'] == 124, timed_out
        cases.append({'name': 'timeout-closes-only-owned-job', 'passed': True})
        report['passed'] = True
    except BaseException as error:
        report['error'] = str(error)
        raise
    finally:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(f"[INFO] Workspace cases: {len(cases)}; report: {args.output}")


if __name__ == '__main__':
    main()
