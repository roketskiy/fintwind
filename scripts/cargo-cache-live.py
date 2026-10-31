"""Stream Cargo log files while its entire owned Windows job is bounded.

The tiny PowerShell child performs byte-preserving native file redirection.
This supervisor never scans for or terminates any process it did not create.
"""

import argparse
import codecs
import ctypes
import importlib.util
import json
from pathlib import Path
import re
import shutil
import sys
import time
import os


ROOT = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=('Test', 'StableRelease'), required=True)
    parser.add_argument('--log-directory', type=Path, required=True)
    parser.add_argument('--timeout-seconds', type=int, default=3000)
    args = parser.parse_args()
    if not 1 <= args.timeout_seconds <= 5400:
        parser.error('timeout must be between 1 and 5400 seconds')
    args.log_directory.mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location('owned_cargo_live', ROOT / 'goal-opencode-e2e.py')
    support = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(support)
    files = {}
    for stream in ('stdout', 'stderr'):
        path = args.log_directory / ('cargo.' + stream + '.log')
        # This invocation owns these paths; truncate before starting the child.
        path.write_bytes(b'')
        files[stream] = {'file': path.open('rb'), 'decoder': codecs.getincrementaldecoder('utf-8')('replace'),
                         'pending': ''}
    started = time.monotonic()
    stage = 'compile'
    compile_seconds = None
    live_lines = 0
    next_heartbeat = started + 30
    code = 1
    timed_out = False
    error = None
    child = None

    def emit(line, alive):
        nonlocal stage, compile_seconds, live_lines
        elapsed = time.monotonic() - started
        if re.search(r'Finished `(test|release)` profile', line):
            compile_seconds = elapsed
            stage = 'tests' if args.mode == 'Test' else 'complete'
        if 'Doc-tests ' in line:
            stage = 'documentation-tests'
        # Fingerprint trace remains in the raw artifact. Show compiler output,
        # warnings, test progress and errors without thousands of trace lines.
        if re.match(r'^\s*\d+(?:\.\d+)?s\s+(TRACE|DEBUG|INFO)\b', line):
            return
        print(line, flush=True)
        if alive and line.strip():
            live_lines += 1

    def drain(alive, final=False):
        for stream in files.values():
            # Bound each turn so a noisy stderr cannot starve stdout or the
            # deadline. The complete stream is already retained on disk.
            data = stream['file'].read(256 * 1024)
            text = stream['decoder'].decode(data, final=False)
            stream['pending'] += text
            lines = stream['pending'].split('\n')
            stream['pending'] = lines.pop()
            for line in lines:
                emit(line.rstrip('\r'), alive)
            if len(stream['pending']) > 64 * 1024:
                emit(stream['pending'], alive)
                stream['pending'] = ''
            if final and not data:
                tail = stream['pending'] + stream['decoder'].decode(b'', final=True)
                if tail:
                    emit(tail, alive)
                stream['pending'] = ''
        return any(stream['file'].tell() < Path(stream['file'].name).stat().st_size for stream in files.values())

    try:
        pwsh = shutil.which('pwsh')
        if not pwsh:
            raise RuntimeError('PowerShell 7 is required to run Cargo diagnostics')
        child = support.OwnedProcess(pwsh, [
            '-NoProfile', '-File', str(ROOT / 'cargo-cache-command.ps1'),
            '-Mode', args.mode, '-LogDirectory', str(args.log_directory.resolve()),
        ], Path.cwd(), dict(os.environ))
        while child.alive():
            now = time.monotonic()
            drain(True)
            if now >= started + args.timeout_seconds:
                timed_out = True
                code = 124
                print(f'[X] Cargo {stage} exceeded {args.timeout_seconds}s; closing its owned job.', flush=True)
                break
            if now >= next_heartbeat:
                print(f'[INFO] Cargo stage={stage}; elapsed={int(now - started)}s', flush=True)
                next_heartbeat = now + 30
            time.sleep(0.05)
        if not timed_out:
            status = ctypes.c_ulong()
            child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
            if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
                raise ctypes.WinError(ctypes.get_last_error())
            code = status.value
    except BaseException as failure:
        error = str(failure)
        raise
    finally:
        if child:
            child.close()
        while drain(False):
            pass
        drain(False, final=True)
        for stream in files.values():
            stream['file'].close()
        elapsed = time.monotonic() - started
        report = {'seconds': round(elapsed, 3), 'compileSeconds': round(compile_seconds, 3) if compile_seconds is not None else None,
                  'afterCompileSeconds': round(elapsed - compile_seconds, 3) if compile_seconds is not None else None,
                  'lastStage': stage, 'exitCode': code, 'timedOut': timed_out,
                  'liveLinesBeforeExit': live_lines, 'error': error}
        (args.log_directory / 'phases.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(f'[INFO] Cargo phases: {json.dumps(report)}', flush=True)
    return code


if __name__ == '__main__':
    sys.exit(main())
