# Cargo cache diagnostics

## Scope

The Tests workflow records Cargo fingerprint decisions for debug tests and the
stable release compile check, and streams real progress while they run. The
shared setup action uses content-checked timestamps for the complete audited
workspace build-input set. It does not edit Cargo fingerprints or disable
freshness checks. CI alone sets workspace dev/test opt-level to zero; the
manifest's dependency overrides retain optimized GPUI. Local development and
shipping release profiles are unchanged.

`scripts/cargo-cache-diagnostics.ps1` executes the existing Cargo commands and
preserves their exit codes. The workflow uploads diagnostics on success and
failure, retaining them for seven days:

- `cargo-cache-diagnostics-test`
- `cargo-cache-diagnostics-release-check`

The initial instrumentation was verified in
[run 41163358066](https://github.com/roketskiy/fintwind/actions/runs/41163358066).
The GPUI-only implementation was verified warm in
[run 41176189468](https://github.com/roketskiy/fintwind/actions/runs/41176189468):
both profiles reused GPUI, while local packages and missing example outputs
still rebuilt. The v3 whole-workspace implementation needs its own cold/warm
remote acceptance; local verification is not a CI speed measurement.

## Artifact contents

| File | Contents |
| --- | --- |
| `cargo.stderr.log` | Full Cargo progress, compiler diagnostics, and fingerprint trace |
| `cargo.stdout.log` | Test output |
| `fingerprint-summary.log` | Dirty/stale reasons, fingerprint errors, completion, and compiler errors |
| `diagnostics.json` | Cargo/rustc versions, command, profile, selected compiler environment, timing, exit code, and pre-build timestamps |
| `phases.json` | Compile/test timing, timeout, exit code and progress seen before child exit |
| `source-inputs-prepare.json` | Content and build-context hashes, file/byte counts |
| `source-inputs-restore.json` | Restored file/directory counts and changed inputs |
| `source-inputs-capture.json` | Pre-Cargo snapshot status and scan time |
| `source-inputs-verify.json` | Whether the post-build inputs still match the pre-build snapshot and cache key |

The metadata includes hashes and timestamps of selected project and vendored
GPUI inputs, plus restored GPUI fingerprint timestamps. It does not dump the
full process environment. Fingerprint-only filters cover Cargo's old and new
module names without enabling HTTP or registry-authentication tracing.

Compiler progress, warnings/errors and test output appear live as files grow.
The raw fingerprint trace stays in the artifact rather than flooding the live
console. A heartbeat reports the current phase and elapsed seconds every 30
seconds. Both streams are drained in bounded chunks; long lines and UTF-8
boundaries are supported. Stream order is preserved within each stream, not
invented across stdout and stderr. A short final summary includes compile and
post-compile time. `GITHUB_STEP_SUMMARY` receives a concise timing table.

The PowerShell entry point delegates to `cargo-cache-live.py`, which launches
the redirection-only `cargo-cache-command.ps1` inside a newly owned Windows
Job Object. The default deadline is 3000 seconds (`-TimeoutSeconds` overrides
it, up to 5400). Expiry returns 124 and closes only that job, including its
descendants. No PID scan, port scan or name-based termination is used. Runner
job timeouts remain the outer bound. For local execution, use an isolated
data directory and a controlled environment as the black-box runners do.

## How to identify the first invalidation

1. Confirm the job restored a compiler cache in the setup action's log. A cold
   cache naturally reports missing fingerprints and is not evidence of a bug.
2. Read `fingerprint-summary.log` for the first direct GPUI invalidation.
3. Follow `StaleDepFingerprint` back to its dependency. This is normally a
   downstream consequence, not the original cause.
4. For `ChangedFile`, compare the source's `stale_mtime` with the cached
   fingerprint's `reference_mtime`. Use the full trace to identify the file.
5. Check other reasons, including changed environment/profile, build-script
   inputs, and missing outputs, before changing any cache policy.

Do not treat a cache download hit as proof that Cargo reused every artifact.
Do not suppress Cargo freshness checks or backdate all checkout files: either
can hide genuine source changes.

## Isolated local reproduction

Prerequisites: Windows, Python 3, PowerShell 7, and an installed MSVC Rust
toolchain. Obtain the toolchain directory with `rustc --print sysroot`, then
pass that path explicitly:

```powershell
python scripts/cargo-cache-diagnostics-blackbox.py --sysroot '<rustc sysroot>' --output 'C:/Users/Public/Temp/opencode/cargo-cache-diagnosis/result.json'
```

Allow up to five minutes for each offline suite. Each compiler
invocation has a 90-second deadline and runs in an exclusively owned Windows
Job Object, including its descendants. The runner uses a UUID-owned fixture,
private Cargo home and target directory, and an environment whitelist. It
does not build Fintwind, start providers, use the project's target directory,
or download dependencies. Logs and the JSON report remain available for review.

The fixture contains a minimal path dependency named `gpui-pre`; it is not the
real GPUI implementation. The checks cover:

- Cold build, then a fresh build in the same checkout.
- Restoring only `deps`, `build`, and `.fingerprint`, as CI does, after changing
  source timestamps without changing source bytes.
- Restoring original timestamps as a control, only inside the disposable
  fixture after confirming the dependency's content hash is unchanged.
- A genuine source edit that must rebuild.
- A compiler error whose original nonzero exit code must survive the wrapper.
- Cold and warm stable release compile-check commands.
- Actual snapshot capture/restoration for both pinned vendor roots, including
  a default-scanning Windows build script and a watched resource directory.
- Source, build-script, resource, file-addition/deletion, and environment changes
  that must still rebuild; application-only changes that must not rebuild GPUI.
- Missing, malformed, profile-mismatched, and traversal-bearing snapshots, plus
  a junction pointing outside the package, without touching outside timestamps.

The independent Windows `Offline cache and logging verification` job runs this
fixture and `cargo-cache-workspace-blackbox.py` in parallel with the real Rust
job, with five-minute step deadlines. It uploads
`cargo-cache-verification-reports` even on failure. Its fixture data, Cargo
home, target and owned process trees are isolated from the real compilation.
The original fixture deliberately retains the legacy three-directory cache
layout to reproduce the original defect; the new fixture restores the complete
v3 layout, including examples.

```powershell
python scripts/cargo-cache-workspace-blackbox.py --sysroot '<rustc sysroot>' --output 'C:/Users/Public/Temp/opencode/cargo-workspace-check/result.json'
python scripts/cargo-cache-retention-blackbox.py --output 'C:/Users/Public/Temp/opencode/cargo-retention-check/result.json'
```

The workspace suite checks all-local/examples freshness, docs-only identity,
edited bytes with deliberately old mtimes, locale/SQL/resource/embedded
changelog changes, file additions/deletions/renames, post-build edits, declared
environment inputs, corrupt/traversal/context/profile/timestamp snapshots,
junctions, original compiler errors, live progress, output pressure and owned
timeouts. The retention suite calls a fake `gh` executable in a private fixture;
it never authenticates to or deletes from the user's GitHub account.

CI uses `cargo-cache-msvc.py` to activate the native MSVC environment through
Visual Studio's `vswhere`/`VsDevCmd.bat`, without a Node action dependency. It
selects matching x64/x64 or arm64/arm64 host/target, captures only SDK/compiler
metadata and verifies the linker and library paths before exporting to
`GITHUB_ENV`. The developer command runs in its own bounded Windows job. The
workspace black-box suite checks both architecture argument/export paths with
fake developer commands; these are NOT an actual ARM64 compilation test.

The x64 offline fixture then passes `--require-msvc-env`. The runner preserves only allowlisted
SDK/toolchain variables (`PATH`, `LIB`, `INCLUDE`, `LIBPATH`, and installation
metadata); user data and Cargo home remain private. The preflight checks that
`link.exe` comes from `VCToolsInstallDir` and that library paths are configured,
and records that information in the JSON report. Otherwise it fails before
compiling. This prevents accidentally invoking Git's unrelated GNU `link.exe`,
which reports `link: extra operand` for MSVC arguments.

For the same strict preflight locally, start an x64 Visual Studio Developer
Command Prompt and add `--require-msvc-env` to the command above. A normal
local invocation without that flag can still use rustc's automatic MSVC
discovery, but does not establish that the CI developer environment is ready.

## Confirmed cause

The controlled reproduction confirms that unchanged path-dependency source
bytes with newer modification times can produce:

```text
stale: changed ".../vendor/gpui-pre/gpui-pre-0.3.8/build.rs"
dirty: FsStatusOutdated(StaleItem(ChangedFile { ... }))
```

The application then becomes dirty through `StaleDepFingerprint`. With the
same cached artifacts and the original timestamps restored, the fixture stays
fresh. Genuine source changes still rebuild.

Run 41163358066 confirmed the same mechanism in both real Windows jobs. The
compiler caches were restored from commit `de089a8`, while checkout gave both
GPUI `build.rs` files newer modification times. The trace reported
`ChangedFile` / `FsStatusOutdated`, followed by dependent crate invalidations.
Neither vendored GPUI package changed between `de089a8` and that run's commit
`dd8d61f`. The debug Windows build script also reported
`PrecalculatedComponentsChanged`: without rerun directives it fingerprints the
package's file timestamps, so restoring only `build.rs` would be insufficient.

## Legacy GPUI-only timestamp restoration

The following describes the v2 diagnosis/control fixture, NOT the active
whole-workspace CI policy described in the next section.

`scripts/cargo-cache-source-stamps.py` handles only:

- `vendor/gpui-pre/gpui-pre-0.3.8`
- `vendor/gpui-pre/gpui-pre-windows-0.3.8`

After restoring compiler artifacts, the setup action compares each package's
complete file/directory list, SHA-256 file hashes, sizes, and modes with the
snapshot in the cache. It restores file and directory modification times only
if the entire package matches. A changed package is left alone; Cargo still
detects changes in source, resources, build scripts, dependencies, profiles,
features, and declared environment inputs. The application, configuration,
migrations, and other workspace sources are never backdated.

The action then captures a new snapshot BEFORE compiling. It is stored at
`target/<profile>/.fingerprint/fintwind-gpui-source-stamps.json`, alongside the
compiler artifacts, and saved only by the existing successful-main-job cache
step. Do not generate a snapshot from sources after compilation and pair it
with older outputs. The helper is for private, immutable CI checkouts, not a
live development directory being edited by another process.

Missing or invalid snapshots, mismatched package trees, unsupported filesystem
entries, and links/junctions cause restoration to be skipped. Snapshot paths
and timestamps are validated before any source metadata is changed. The scan
and timestamp restoration do not follow source links or reparse points. I/O
failures while applying validated timestamps fail the job rather than silently
continuing after a partial restore.

The helper's code participates in compiler-cache keys. The first run after
adoption therefore builds a new compiler cache; dependency-download caches
remain reusable. A later unchanged run must report both packages as
`restored`, and its Cargo trace must show no timestamp-driven GPUI rebuild.
Real GPUI input capture was measured locally over 188 files at about 0.11
seconds; this does not predict total CI build time.

## V3 whole-workspace input protocol

`cargo-cache-workspace.py` scans complete trees, without extension filters:
`src`, `crates`, `vendor`, `assets`, `resources`, `locales`, `db`, `tests`,
`examples`, and `.cargo`. It also hashes root manifests/lockfile/build script,
Rust toolchain files, and **CHANGELOG.md**, which `src/app/help.rs` embeds.
Root documentation and `docs/` are excluded; new compiler inputs outside these
audited roots must be added before relying on their cache identity.

Each file is compared by path, SHA-256 bytes, size and mode. Only unchanged
files regain their own cached mtimes. A directory regains its mtime only if
its whole subtree matches, so directory watchers see additions, removals and
renames. Changed inputs receive a fresh mtime even when an editor deliberately
preserved an old one. No links or reparse points are followed. A malformed or
context-mismatched snapshot is skipped. Unsupported trees disable saving.
I/O failures during timestamp application fail instead of hiding a partial
restore. This is for immutable CI checkouts, not live editing worktrees.

The protocol is `prepare -> restore artifacts -> restore stamps -> capture ->
compile/test -> verify -> save`. The snapshot lives at
`target/<profile>/.fingerprint/fintwind-workspace-inputs.json`. Verification
compares the full input list/bytes/modes/mtimes, original input hash and
compatibility hash. Source edits during the build therefore prevent saving.

Keys are `cargo-build-v3-<OS>-<arch>-<kind>-<context-hash>-<input-hash>`.
The context includes the rustc identity, absolute workspace/target/Cargo home,
selected profile/compiler/SDK environment and CI cache automation. A compatible
prefix allows reuse when source changes; Cargo still determines dependent
rebuilds. Different architectures, test/release modes and incompatible contexts
do not share a prefix. Source commit SHA is NOT the identity: docs-only commits
can reuse an exact key, but Tests still runs and release gating still requires
successful Tests for the exact release commit.

Compiler caches contain `deps`, `build`, `.fingerprint`, and **examples**.
They exclude incremental state, installers, archives and top-level app binaries.
The dependency download cache also contains registry sources and Git checkouts,
avoiding repeated extraction/checkout before Cargo's fingerprint stage. No
credentials or Cargo config files are added to the dependency cache.

Only successful, verified main-branch builds save immutable compiler keys.
PRs and tags restore but do not save compiler snapshots. Manual Release runs
on main can seed release-build caches. Adoption of v3 is one cold migration;
do not infer steady-state performance from it.

## Retention and free-storage budget

GitHub includes 10 GB of cache per repository, separately from artifact storage.
See [GitHub's cache limits](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).
No billing limit or runner class is changed here. The operational soft budget
is 7.5 GB, leaving headroom for uploads before GitHub's automatic eviction.

After all Tests jobs succeed on main (or a manual main Release finishes its
Windows builds and same-commit checks), a read/write-permission maintenance job
calls `cargo-cache-retention.py --apply`. It paginates through the real `gh`
API, preserves the newest two v3 generations per architecture/profile and the
newest v3 dependency-download cache per architecture. Retired v2 caches are
removed only once a v3 cache exists for the matching group. Unknown cache keys
and non-main refs are never deleted. Another active workflow run defers all
deletion; activity is checked again during cleanup. A new reader may race a
check, but cache misses are safe and no downloaded/live build directory is
touched. No hard promise to protect every future reader is possible with the
GitHub cache API.

Without `--apply`, maintenance is read-only. API failures do not become empty
inventories. The maintenance job is non-blocking housekeeping: failures remain
visible with their JSON report, but do not falsify passing product checks.
Protected latest generations are not deleted to force the 7.5 GB target; an
over-budget report means the next measured adjustment is needed. Oversized
protected caches and existing unrelated caches can still invoke GitHub's own
10 GB eviction. Initial migration may also evict old caches before cleanup.

## Acceptance before claiming a speedup

Run cold once, then repeat the SAME commit with the same inputs and context.
Confirm no local, GPUI or example compilation in the warm logs; tests must
still execute. Then compare docs-only and one-crate changes, and verify real
resource/locale/migration changes rebuild their consumers. Compare full job
elapsed time INCLUDING restore/save and parallel verification, not only Cargo.
Record ignored/externally authenticated E2E cases separately from executed
tests. Cache hits are not proof of full E2E coverage. The 3-5 minute warm target
is a goal to measure remotely, not an observed result or a promise for feature
changes. CI opt-level=0 must be evaluated against total compile plus test time.
