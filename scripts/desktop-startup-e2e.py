"""Exercise first launch and restart of the isolated Windows debug desktop.

Build the app and daemon into target/startup-e2e-build before running. The
test never starts the installed app, and a private Windows Job contains every
process it creates. Each run keeps its data, logs, and JSON proof under target.
"""
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid

from school_build.process import WindowsJob

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / 'target/startup-e2e-build/debug/fintwind.exe'
DAEMON = BINARY.with_name('fintwind-daemon.exe')


def visible_windows(pid):
    user32 = ctypes.WinDLL('user32', use_last_error=True)
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user32.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
    user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user32.IsWindowVisible.argtypes = [wintypes.HWND]
    found = []

    @callback_type
    def visit(hwnd, _):
        owner = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(hwnd):
            found.append(int(hwnd))
        return True

    user32.EnumWindows(visit, 0)
    return found


def launch(run, env, name):
    job = WindowsJob()
    child = None
    try:
        with (run / (name + '.stdout.log')).open('wb') as stdout, \
                (run / (name + '.stderr.log')).open('wb') as stderr:
            child = subprocess.Popen(
                [str(run / BINARY.name)], cwd=run, env=env, stdin=subprocess.DEVNULL,
                stdout=stdout, stderr=stderr, creationflags=0x4,
            )
            job.resume(child)
            deadline = time.monotonic() + 45
            visible = False
            while child.poll() is None and time.monotonic() < deadline:
                if visible_windows(child.pid):
                    visible = True
                    break
                time.sleep(0.2)
            if visible:
                # Startup work continues after the first window appears.
                time.sleep(5)
            status = child.poll()
        stderr_text = (run / (name + '.stderr.log')).read_text(encoding='utf-8', errors='replace')
        return {'name': name, 'visibleWindow': visible, 'exitCode': status,
                'passed': visible and status is None, 'stderr': stderr_text}
    finally:
        job.close()
        if child is not None:
            child.wait(timeout=10)


def main():
    if os.name != 'nt' or not BINARY.is_file() or not DAEMON.is_file():
        raise RuntimeError('The dedicated Windows debug app and daemon must be built first')
    run = ROOT / 'target/desktop-startup-e2e' / uuid.uuid4().hex
    data = run / 'data'
    data.mkdir(parents=True)
    shutil.copy2(BINARY, run / BINARY.name)
    shutil.copy2(DAEMON, run / DAEMON.name)
    system_root = os.environ['SYSTEMROOT']
    env = {key: os.environ[key] for key in ('SYSTEMROOT', 'WINDIR', 'SYSTEMDRIVE', 'COMSPEC')
           if key in os.environ}
    env.update({
        'PATH': str(Path(system_root) / 'System32'),
        'FINTWIND_DEBUG_DATA_DIR': str(data),
        'FINTWIND_DAEMON_PATH': str(run / DAEMON.name),
        'RUST_BACKTRACE': '1',
        'HOME': str(run), 'USERPROFILE': str(run),
        'APPDATA': str(run / 'roaming'), 'LOCALAPPDATA': str(run / 'local'),
        'TEMP': str(run), 'TMP': str(run),
    })
    report = {'passed': False, 'cases': [], 'data': str(data)}
    try:
        for name in ('first-launch', 'restart'):
            result = launch(run, env, name)
            report['cases'].append(result)
            if not result['passed']:
                raise RuntimeError(name + ' did not stay open after displaying its window')
        report['passed'] = True
    finally:
        proof = run / 'proof.json'
        proof.write_text(json.dumps(report, indent=2), encoding='utf-8')
        print('Desktop startup proof: ' + str(proof), flush=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())
