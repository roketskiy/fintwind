"""Bound Fintwind main-branch caches through gh, without changing billing limits.

Run only after successful cache-producing jobs. Default is a read-only plan.
Any other active workflow run defers all deletions to avoid removing its inputs.
"""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time


BUILD = re.compile(r'^cargo-build-v([23])-(Windows)-(X64|ARM64)-(test|release-check|release-build)-.+$')
DOWNLOAD = re.compile(r'^cargo-downloads-v([23])-(Windows)-(X64|ARM64)-.+$')
# Only this exact retired project's scheme; no generic *rust* deletion rule.
LEGACY = re.compile(r'^windows-latest-rust-1\.97\.0-release-check-[a-f0-9]{64}$')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--run-id', required=True)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--gh', default='gh', help='gh executable, or a .py CLI fixture for offline verification')
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    if not re.fullmatch(r'[\w.-]+/[\w.-]+', args.repo) or not args.run_id.isdigit():
        parser.error('Valid repository and numeric run id are required')
    if args.apply and os.environ.get('GITHUB_REF') != 'refs/heads/main':
        parser.error('Deletion is restricted to main-branch maintenance jobs')
    command = [sys.executable, args.gh] if args.gh.endswith('.py') else [shutil.which(args.gh) or args.gh]
    report = {'applied': args.apply, 'deleted': [], 'planned': [], 'deferred': False}
    started = time.monotonic()

    def api(path, delete=False):
        if time.monotonic() - started > 120:
            raise TimeoutError('Cache maintenance exceeded 120 seconds')
        parameters = ['api', '--method', 'DELETE'] if delete else ['api', '--paginate', '--slurp']
        completed = subprocess.run(command + parameters + [f'repos/{args.repo}/{path}'],
                                   capture_output=True, text=True, timeout=20)
        if completed.returncode:
            # Do not echo authentication headers or a potentially verbose CLI body.
            raise RuntimeError(f'gh cache API failed with exit code {completed.returncode}')
        return None if delete else json.loads(completed.stdout)

    try:
        pages = api('actions/caches?per_page=100')
        caches = [cache for page in pages for cache in page['actions_caches']]
        report['bytesBefore'] = sum(cache['size_in_bytes'] for cache in caches)
        # Restrict deletions to known namespaces on main. Preserve the newest
        # two v3 snapshots per architecture/profile, irrespective of key context.
        groups = {}
        downloads = {}
        for cache in caches:
            if cache.get('ref') != 'refs/heads/main':
                continue
            match = BUILD.fullmatch(cache['key'])
            if match:
                groups.setdefault(tuple(match.groups()[1:]), []).append((int(match[1]), cache))
            match = DOWNLOAD.fullmatch(cache['key'])
            if match:
                downloads.setdefault(tuple(match.groups()[1:]), []).append((int(match[1]), cache))
        removable = []
        for group in groups.values():
            current = sorted((cache for version, cache in group if version == 3),
                             key=lambda cache: cache['created_at'], reverse=True)
            if current:
                protected = {cache['id'] for cache in current[:2]}
                removable += [cache for _, cache in group if cache['id'] not in protected]
        for group in downloads.values():
            current = sorted((cache for version, cache in group if version == 3),
                             key=lambda cache: cache['created_at'], reverse=True)
            if current:
                removable += [cache for _, cache in group if cache['id'] != current[0]['id']]
        has_release_v3 = any(version == 3 for key, group in groups.items()
                             if key[-1] == 'release-check' for version, _ in group)
        if has_release_v3:
            removable += [cache for cache in caches if cache.get('ref') == 'refs/heads/main'
                          and LEGACY.fullmatch(cache['key'])]
        removable.sort(key=lambda cache: cache['created_at'])
        report['planned'] = [{'id': cache['id'], 'key': cache['key'], 'bytes': cache['size_in_bytes']}
                             for cache in removable]
        report['bytesAfterPlan'] = report['bytesBefore'] - sum(cache['size_in_bytes'] for cache in removable)
        report['overSoftBudget'] = report['bytesAfterPlan'] > 7_500_000_000
        if report['overSoftBudget']:
            print('::warning::Protected or unrelated caches exceed the 7.5 GB soft budget; '
                  'review measured cache sizes. Billing settings were not changed.', flush=True)
        active = []
        for status in ('in_progress', 'queued', 'waiting', 'pending', 'requested'):
            active += [run for page in api(f'actions/runs?status={status}&per_page=100')
                       for run in page['workflow_runs'] if str(run['id']) != args.run_id]
        if active:
            report.update(deferred=True, reason='another-workflow-run-is-active')
        elif args.apply:
            # Recheck activity before EACH deletion. New runs can still start
            # after a check; readers tolerate a miss and build normally. This
            # never touches downloaded inputs or live compiler directories.
            for cache in removable:
                running = api('actions/runs?status=in_progress&per_page=100')
                if any(str(run['id']) != args.run_id for page in running for run in page['workflow_runs']):
                    report.update(deferred=True, reason='another-run-started-during-maintenance')
                    break
                api(f"actions/caches/{cache['id']}", delete=True)
                report['deleted'].append(cache['id'])
        report['passed'] = True
    except BaseException as error:
        report.update(passed=False, error=str(error))
        raise
    finally:
        report['seconds'] = round(time.monotonic() - started, 3)
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2), encoding='utf-8')
        print('[INFO] Cache retention: ' + json.dumps(report), flush=True)
        if os.environ.get('GITHUB_STEP_SUMMARY'):
            with open(os.environ['GITHUB_STEP_SUMMARY'], 'a', encoding='utf-8') as summary:
                summary.write(f"\n### Cache retention\n\nBefore: {report.get('bytesBefore', 0) / 1e9:.2f} GB. "
                              f"Planned retained: {report.get('bytesAfterPlan', 0) / 1e9:.2f} GB. "
                              f"Deleted: {len(report['deleted'])}. Deferred: {report['deferred']}.\n")


if __name__ == '__main__':
    main()
