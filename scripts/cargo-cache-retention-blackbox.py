"""Exercise retention through a fake gh CLI, never the user's GitHub account.

Failure inventory: unbounded snapshots; deleting another cache namespace/ref;
deleting the newest generations; cleanup during another active run; pagination
ignored; API errors interpreted as empty state; deleting before new saves;
budget pressure deleting protected caches; dry-run sending DELETE requests.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    results = []
    report = {'passed': False, 'cases': results}
    with tempfile.TemporaryDirectory(prefix='cache-retention-', dir=args.output.parent) as directory:
        root = Path(directory)
        fake = root / 'gh.py'
        fake.write_text(
            'import json,os,sys\nfrom pathlib import Path\n'
            'root=Path(__file__).parent\nargs=sys.argv[1:]\n'
            'with (root/"calls.jsonl").open("a") as f: f.write(json.dumps(args)+"\\n")\n'
            'data=json.loads((root/"fixture.json").read_text())\n'
            'if data.get("fail"): sys.exit(9)\n'
            'if "DELETE" in args: print("null"); sys.exit(0)\n'
            'path=args[-1]\n'
            'if "/actions/caches?" in path: print(json.dumps(data["pages"]))\n'
            'elif "/actions/runs?" in path: print(json.dumps([{"workflow_runs":data.get("runs",[])}]))\n'
            'else: sys.exit(10)\n', encoding='utf-8')

        def cache(number, *, version=3, kind='test', ref='refs/heads/main', key=None, size=500_000_000):
            return {'id': number, 'ref': ref, 'key': key or f'cargo-build-v{version}-Windows-X64-{kind}-context-input{number}',
                    'created_at': f'2026-10-{number:02d}T00:00:00Z', 'size_in_bytes': size}

        def run(name, caches, *, apply=True, runs=None, fail=False, expected=None, success=True):
            (root / 'calls.jsonl').write_text('')
            # Multiple API pages exercise the same --paginate/--slurp shape as gh.
            fixture = {'pages': [{'actions_caches': caches[:2]}, {'actions_caches': caches[2:]}],
                       'runs': runs or [], 'fail': fail}
            (root / 'fixture.json').write_text(json.dumps(fixture))
            output = root / 'result.json'
            command = [sys.executable, str(ROOT / 'cargo-cache-retention.py'),
                       '--repo', 'example/fintwind', '--run-id', '42',
                       '--gh', str(fake), '--report', str(output)]
            if apply:
                command.append('--apply')
            completed = subprocess.run(command, capture_output=True, text=True, timeout=30,
                                       env={**os.environ, 'GITHUB_REF': 'refs/heads/main'})
            assert (completed.returncode == 0) == success, (name, completed.stderr)
            calls = [json.loads(line) for line in (root / 'calls.jsonl').read_text().splitlines()]
            deleted = [int(call[-1].rsplit('/', 1)[-1]) for call in calls if 'DELETE' in call]
            assert deleted == (expected or []), (name, deleted)
            results.append({'name': name, 'passed': True, 'deleted': deleted})

        try:
            records = [cache(1), cache(2), cache(3), cache(4), cache(5, version=2),
                       cache(6, key='another-project-cache'), cache(7, ref='refs/pull/1/merge')]
            run('latest-two-and-namespace-protection', records, expected=[1, 2, 5])
            run('dry-run-never-deletes', records, apply=False)
            run('active-other-run-defers-all-cleanup', records, runs=[{'id': 43}])
            run('current-run-is-not-a-conflict', records, runs=[{'id': 42}], expected=[1, 2, 5])
            run('API-error-is-not-empty-inventory', records, fail=True, success=False)
            run('legacy-without-v3-success-is-preserved', [cache(1, version=2), cache(2, version=2)])
            run('independent-profile-generations', [cache(1), cache(2), cache(3),
                cache(4, kind='release-check'), cache(5, kind='release-check'), cache(6, kind='release-check')],
                expected=[1, 4])
            run('budget-never-deletes-last-two', [cache(1, size=5_000_000_000), cache(2, size=5_000_000_000)])
            run('old-download-keeps-newest', [cache(1, key='cargo-downloads-v3-Windows-X64-old'),
                cache(2, key='cargo-downloads-v3-Windows-X64-new')], expected=[1])
            report['passed'] = True
        except BaseException as error:
            report['error'] = str(error)
            raise
        finally:
            args.output.write_text(json.dumps(report, indent=2), encoding='utf-8')
            print(f'[INFO] Retention cases: {len(results)}; report: {args.output}')


if __name__ == '__main__':
    main()
