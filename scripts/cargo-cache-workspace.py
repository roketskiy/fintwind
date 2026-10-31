"""Content-checked CI input stamps and content-addressed compiler-cache identity.

Only immutable CI checkouts are supported. Capture before Cargo, verify before
saving. No compiler artifacts or Cargo fingerprints are rewritten by this tool.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time


ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('source_stamps', ROOT / 'cargo-cache-source-stamps.py')
stamps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stamps)

# Directory trees, not extension filters: include_str!/include_bytes!, proc
# macros and build scripts consume non-Rust files as well. Root-level docs and
# docs/ are not build inputs in this repository. CHANGELOG.md IS embedded by
# src/app/help.rs and is deliberately included below. CI automation is key material.
TREES = ('src', 'crates', 'vendor', 'assets', 'resources', 'locales', 'db',
         'tests', 'examples', '.cargo')
FILES = ('Cargo.toml', 'Cargo.lock', 'build.rs', 'rust-toolchain', 'rust-toolchain.toml', 'CHANGELOG.md')
ENVIRONMENT = (
    'CARGO_INCREMENTAL', 'CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG',
    'CARGO_PROFILE_DEV_OPT_LEVEL', 'CARGO_PROFILE_TEST_OPT_LEVEL',
    'CARGO_PROFILE_DEV_CODEGEN_UNITS', 'CARGO_PROFILE_TEST_CODEGEN_UNITS',
    'CARGO_PROFILE_RELEASE_LTO', 'CARGO_PROFILE_RELEASE_CODEGEN_UNITS',
    'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
    'CARGO_BUILD_TARGET', 'VCTOOLSINSTALLDIR', 'VCINSTALLDIR', 'VSINSTALLDIR',
    'WINDOWSSDKDIR', 'WINDOWSSDKVERSION', 'VSCMD_ARG_HOST_ARCH', 'VSCMD_ARG_TGT_ARCH',
    'INCLUDE', 'LIB', 'LIBPATH', 'GPUI_LEAK_DETECTION', 'RUSTC_BOOTSTRAP',
)
STATE = 'fintwind-workspace-inputs.json'
MAX_ENTRIES = 30_000
MAX_BYTES = 16 * 1024 * 1024


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def identity(entry):
    return {key: value for key, value in entry.items() if key != 'mtime_ns'}


def inputs(workspace):
    entries = []
    for tree in TREES:
        root = workspace / tree
        if not root.exists() and not root.is_symlink():
            continue
        for entry in stamps.scan(workspace, tree):
            entries.append({**entry, 'path': tree + ('/' + entry['path'] if entry['path'] else '')})
    for name in FILES:
        path = workspace / name
        if not path.exists() and not path.is_symlink():
            continue
        metadata = stamps.regular_metadata(path)
        if not stat.S_ISREG(metadata.st_mode):
            raise stamps.UnsupportedTree('non-regular-root-input')
        data = path.read_bytes()
        after = stamps.regular_metadata(path)
        if (metadata.st_mtime_ns, metadata.st_size, metadata.st_ino) != (
                after.st_mtime_ns, after.st_size, after.st_ino):
            raise stamps.UnsupportedTree('root-input-changed-during-read')
        entries.append({'path': name, 'kind': 'file', 'mode': stat.S_IMODE(metadata.st_mode),
                        'mtime_ns': metadata.st_mtime_ns, 'size': len(data),
                        'sha256': hashlib.sha256(data).hexdigest()})
    if len(entries) > MAX_ENTRIES:
        raise stamps.UnsupportedTree('workspace-entry-limit')
    return sorted(entries, key=lambda entry: entry['path'])


def validate(data, profile, context):
    if (not isinstance(data, dict) or type(data.get('schema')) is not int or data['schema'] != 1
            or data.get('profile') != profile or data.get('context') != context
            or data.get('cacheable') is not True):
        raise ValueError('snapshot-context-or-schema-mismatch')
    entries = data.get('entries')
    if not isinstance(entries, list) or not 1 <= len(entries) <= MAX_ENTRIES:
        raise ValueError('snapshot-entry-limit')
    seen = set()
    indexed = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError('invalid-entry')
        name = entry.get('path')
        if (not isinstance(name, str) or len(name) > 1024 or '\\' in name or ':' in name
                or '\0' in name or any(part in ('', '.', '..') for part in name.split('/'))
                or (name not in FILES and name.split('/')[0] not in TREES)):
            raise ValueError('invalid-entry-path')
        if name.casefold() in seen:
            raise ValueError('duplicate-entry')
        seen.add(name.casefold())
        if (type(entry.get('mtime_ns')) is not int or not 0 < entry['mtime_ns'] <= time.time_ns() + 60_000_000_000
                or type(entry.get('mode')) is not int or not 0 <= entry['mode'] <= 0o7777
                or entry.get('kind') not in ('file', 'directory')):
            raise ValueError('invalid-entry-metadata')
        if entry['kind'] == 'file' and (
                type(entry.get('size')) is not int or entry['size'] < 0
                or not isinstance(entry.get('sha256'), str)
                or not re.fullmatch('[a-f0-9]{64}', entry['sha256'])):
            raise ValueError('invalid-file-metadata')
        if name in FILES and entry['kind'] != 'file':
            raise ValueError('root-input-not-file')
        indexed[name] = entry
    for name in indexed:
        if '/' in name and indexed.get(name.rpartition('/')[0], {}).get('kind') != 'directory':
            raise ValueError('missing-parent')
    if digest([identity(entry) for entry in sorted(entries, key=lambda entry: entry['path'])]) != data.get('inputHash'):
        raise ValueError('snapshot-content-hash-mismatch')
    return indexed


def subtree_hashes(entries):
    children = {}
    for name in entries:
        children.setdefault(name.rpartition('/')[0], []).append(name)
    hashes = {}
    for name in sorted(entries, key=lambda name: (-name.count('/'), name)):
        entry = entries[name]
        hashes[name] = digest([identity(entry), [hashes[child] for child in sorted(children.get(name, []))]])
    return hashes


def set_time(workspace, entry, stamp):
    path = workspace
    for part in entry['path'].split('/'):
        path /= part
        metadata = stamps.regular_metadata(path)
    if (metadata.st_mtime_ns != entry['mtime_ns'] or stat.S_IMODE(metadata.st_mode) != entry['mode']
            or (entry['kind'] == 'file' and metadata.st_size != entry['size'])):
        raise OSError('Input changed while restoring: ' + entry['path'])
    os.utime(path, ns=(metadata.st_atime_ns, stamp))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('prepare', 'capture', 'restore', 'verify'))
    parser.add_argument('--profile', choices=('debug', 'release'), required=True)
    parser.add_argument('--kind', choices=('test', 'release-check', 'release-build'), default='test')
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--expected-key', help='For verify: reject a key prepared from different inputs or automation')
    args = parser.parse_args()
    started = time.monotonic()
    workspace = Path.cwd().resolve()
    target = Path(os.environ.get('CARGO_TARGET_DIR', 'target')).resolve()
    state = target / args.profile / '.fingerprint' / STATE
    rustc = subprocess.run(['rustc', '-vV'], capture_output=True, text=True, check=True, timeout=20).stdout.strip()
    selected_environment = {name: os.environ.get(name) for name in ENVIRONMENT}
    selected_environment.update({name: value for name, value in os.environ.items()
                                 if name.startswith('CARGO_PROFILE_') or (
                                     name.startswith('CARGO_TARGET_')
                                     and name.endswith(('_RUSTFLAGS', '_LINKER')))})
    context = {'workspace': str(workspace), 'target': str(target),
               'cargoHome': str(Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')).resolve()),
               'rustc': rustc, 'environment': selected_environment}
    report = {'operation': args.operation, 'profile': args.profile, 'cacheable': True,
              'restoredFiles': 0, 'restoredDirectories': 0, 'changedInputs': 0}
    try:
        current = inputs(workspace)
    except (OSError, stamps.UnsupportedTree) as error:
        current = []
        report.update(cacheable=False, reason=str(error))
    input_hash = digest([identity(entry) for entry in current])
    report['inputHash'] = input_hash
    # Changing automation is a compatibility boundary, not a reason to restore
    # incompatible compiler outputs. SHA is deliberately not part of this key.
    automation = {}
    for name in ('.github/actions/setup-rust/action.yml', '.github/actions/save-rust-cache/action.yml', '.github/rust-version',
                 '.github/workflows/test.yml', '.github/workflows/release.yml'):
        path = workspace / name
        if path.is_file():
            automation[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    # Fixture and housekeeping edits do not invalidate compiler outputs.
    for name in ('cargo-cache-workspace.py', 'cargo-cache-source-stamps.py',
                 'cargo-cache-command.ps1', 'cargo-cache-msvc.py'):
        path = ROOT / name
        if path.is_file():
            automation[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
    if args.kind == 'release-build':
        bundle = ROOT / 'bundle-windows.ts'
        if bundle.is_file():
            automation[bundle.name] = hashlib.sha256(bundle.read_bytes()).hexdigest()
    report['compatibilityHash'] = digest({'context': context, 'automation': automation, 'kind': args.kind})
    report['files'] = sum(entry['kind'] == 'file' for entry in current)
    report['inputBytes'] = sum(entry.get('size', 0) for entry in current)
    snapshot = {'schema': 1, 'profile': args.profile, 'context': context,
                'cacheable': report['cacheable'], 'inputHash': input_hash, 'entries': current}
    if args.operation in ('restore', 'verify') and report['cacheable']:
        try:
            if stamps.regular_metadata(state).st_size > MAX_BYTES:
                raise ValueError('snapshot-byte-limit')
            saved = validate(json.loads(state.read_text(encoding='utf-8')), args.profile, context)
        except (OSError, ValueError, UnicodeError) as error:
            report['reason'] = 'snapshot-unavailable: ' + str(error)
            saved = None
        if args.operation == 'verify':
            report['cacheable'] = saved is not None and current == sorted(saved.values(), key=lambda entry: entry['path'])
            if args.expected_key and not args.expected_key.endswith(
                    '-' + report['compatibilityHash'] + '-' + input_hash):
                report['cacheable'] = False
            if not report['cacheable']:
                report['reason'] = 'inputs-changed-since-capture-or-snapshot-unavailable'
        elif saved is not None:
            indexed = {entry['path']: entry for entry in current}
            before_hashes, after_hashes = subtree_hashes(saved), subtree_hashes(indexed)
            # Source content changes must invalidate Cargo even if a tool kept
            # the old timestamp. Never backdate a changed file/directory.
            dirty_time = time.time_ns()
            for name in sorted(indexed, key=lambda name: (
                    indexed[name]['kind'] == 'directory', -name.count('/'))):
                entry, before = indexed[name], saved.get(name)
                matches = before is not None and before_hashes[name] == after_hashes[name]
                if matches:
                    if entry['mtime_ns'] != before['mtime_ns']:
                        set_time(workspace, entry, before['mtime_ns'])
                    field = 'restoredFiles' if entry['kind'] == 'file' else 'restoredDirectories'
                    report[field] += 1
                else:
                    set_time(workspace, entry, dirty_time)
                    report['changedInputs'] += 1
    if args.operation == 'capture':
        stamps.write_json(state, snapshot)
    report['seconds'] = round(time.monotonic() - started, 3)
    stamps.write_json(args.report, report)
    output = os.environ.get('GITHUB_OUTPUT')
    if output:
        with open(output, 'a', encoding='utf-8') as destination:
            for key, value in (('input-hash', input_hash), ('compatibility-hash', report['compatibilityHash']),
                               ('cacheable', str(report['cacheable']).lower())):
                destination.write(f'{key}={value}\n')
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a', encoding='utf-8') as summary:
            summary.write(f"\nWorkspace {args.operation}: {report['files']} files, "
                          f"{report['inputBytes'] / 1e6:.1f} MB, restored {report['restoredFiles']}, "
                          f"changed {report['changedInputs']}, cacheable {report['cacheable']}, "
                          f"{report['seconds']}s.\n")
    print(f"[INFO] Workspace {args.operation}: restored={report['restoredFiles']}, "
          f"changed={report['changedInputs']}, cacheable={report['cacheable']}, {report['seconds']}s", flush=True)


if __name__ == '__main__':
    main()
