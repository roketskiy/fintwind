"""Initialize native MSVC on Windows x64/ARM64 without a Node action runtime.

Only SDK/compiler metadata is captured, never the complete environment. The
developer command and every descendant run inside a newly owned Job Object.
"""

import argparse
import ctypes
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parent
VARIABLES = (
    'PATH', 'INCLUDE', 'LIB', 'LIBPATH', 'VCINSTALLDIR', 'VCTOOLSINSTALLDIR',
    'VSINSTALLDIR', 'WINDOWSSDKDIR', 'WINDOWSSDKVERSION', 'VSCMD_ARG_HOST_ARCH',
    'VSCMD_ARG_TGT_ARCH', 'UNIVERSALCRTSDKDIR', 'UCRTVERSION', 'WindowsLibPath',
    'WindowsSDKLibVersion', 'WindowsSdkBinPath', 'WindowsSdkVerBinPath',
    'ExtensionSdkDir', 'NETFXSDKDir', 'FrameworkDir', 'FrameworkVersion', 'VSCMD_VER',
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=('x64', 'arm64'), required=True)
    parser.add_argument('--dev-command', type=Path, help='Explicit VsDevCmd.bat for controlled local checks')
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('Windows is required')
    if not os.environ.get('GITHUB_ENV'):
        parser.error('GITHUB_ENV is required; this tool exports to subsequent workflow steps')
    developer = args.dev_command
    if developer is None:
        # vswhere is the vendor-supported installation API, not a drive scan.
        locator = shutil.which('vswhere.exe')
        if not locator:
            locator = Path(os.environ['ProgramFiles(x86)']) / 'Microsoft Visual Studio/Installer/vswhere.exe'
        installation = subprocess.run([str(locator), '-latest', '-products', '*', '-prerelease',
                                       '-property', 'installationPath'],
                                      capture_output=True, encoding='utf-8', check=True, timeout=10).stdout.strip()
        if not installation:
            raise RuntimeError('vswhere found no Visual Studio installation')
        developer = Path(installation) / 'Common7/Tools/VsDevCmd.bat'
    if not developer.is_file():
        raise RuntimeError('The selected Visual Studio developer command is missing')
    spec = importlib.util.spec_from_file_location('owned_msvc_setup', ROOT / 'goal-opencode-e2e.py')
    support = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(support)
    with tempfile.TemporaryDirectory(prefix='fintwind-msvc-', dir=os.environ.get('RUNNER_TEMP') or os.environ.get('TEMP')) as temporary:
        directory = Path(temporary)
        wrapper = directory / 'initialize.cmd'
        output = directory / 'toolchain.env'
        wrapper.write_text(
            '@echo off\n'
            'call "%FINTWIND_DEV_COMMAND%" -no_logo -arch=%FINTWIND_NATIVE_ARCH% -host_arch=%FINTWIND_NATIVE_ARCH% >nul\n'
            'if errorlevel 1 exit /b %errorlevel%\n'
            '(for %%N in (' + ' '.join(VARIABLES) + ') do set %%N 2>nul) > "%FINTWIND_TOOLCHAIN_ENV%"\n'
            'exit /b 0\n', encoding='ascii')
        environment = {**os.environ, 'FINTWIND_DEV_COMMAND': str(developer.resolve()),
                       'FINTWIND_NATIVE_ARCH': args.arch, 'FINTWIND_TOOLCHAIN_ENV': str(output)}
        child = support.OwnedProcess(environment['COMSPEC'], ['/u', '/d', '/c', str(wrapper)], directory, environment)
        try:
            deadline = time.monotonic() + 45
            while child.alive() and time.monotonic() < deadline:
                time.sleep(0.05)
            if child.alive():
                raise TimeoutError('MSVC initialization exceeded 45 seconds')
            status = ctypes.c_ulong()
            child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
            if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
                raise ctypes.WinError(ctypes.get_last_error())
            if status.value:
                raise RuntimeError(f'Developer command failed with exit code {status.value}')
        finally:
            child.close()
        selected = {}
        allowed = {name.upper() for name in VARIABLES}
        for line in output.read_text(encoding='utf-16-le').splitlines():
            name, separator, value = line.partition('=')
            if separator and name.upper() in allowed:
                selected[name.upper()] = value
        linker = shutil.which('link.exe', path=selected.get('PATH', ''))
        toolset = selected.get('VCTOOLSINSTALLDIR')
        if (not linker or not toolset or not Path(linker).resolve().is_relative_to(Path(toolset).resolve())
                or not selected.get('LIB') or selected.get('VSCMD_ARG_TGT_ARCH', '').lower() != args.arch
                or selected.get('VSCMD_ARG_HOST_ARCH', '').lower() != args.arch):
            raise RuntimeError('Developer command did not select a native MSVC linker, libraries and matching host/target')
        with open(os.environ['GITHUB_ENV'], 'a', encoding='utf-8') as destination:
            for name, value in selected.items():
                destination.write(f'{name}={value}\n')
        print(f'[INFO] Native MSVC ready: host={args.arch}, target={args.arch}', flush=True)


if __name__ == '__main__':
    main()
