# Cargo cache diagnostics

## Scope

The Tests workflow records Cargo fingerprint decisions for both debug tests
and the stable release compile check. The shared setup action also restores
timestamps for two unchanged vendored GPUI packages using the content-checked
snapshot described below. Compiler settings and Cargo freshness checks remain
unchanged.

`scripts/cargo-cache-diagnostics.ps1` executes the existing Cargo commands and
preserves their exit codes. The workflow uploads diagnostics on success and
failure, retaining them for seven days:

- `cargo-cache-diagnostics-test`
- `cargo-cache-diagnostics-release-check`

The initial instrumentation was verified in
[run 41163358066](https://github.com/roketskiy/fintwind/actions/runs/41163358066).
The timestamp fix still requires a successful cold run followed by a warm run
after being pushed; local verification alone is not a CI speed measurement.

## Artifact contents

| File | Contents |
| --- | --- |
| `cargo.stderr.log` | Full Cargo progress, compiler diagnostics, and fingerprint trace |
| `cargo.stdout.log` | Test output |
| `fingerprint-summary.log` | Dirty/stale reasons, fingerprint errors, completion, and compiler errors |
| `diagnostics.json` | Cargo/rustc versions, command, profile, selected compiler environment, timing, exit code, and pre-build timestamps |
| `source-stamps-restore.json` | Which GPUI packages were restored or skipped, and why |
| `source-stamps-capture.json` | Pre-Cargo snapshot status and scan time |

The metadata includes hashes and timestamps of selected project and vendored
GPUI inputs, plus restored GPUI fingerprint timestamps. It does not dump the
full process environment. Fingerprint-only filters cover Cargo's old and new
module names without enabling HTTP or registry-authentication tracing.

The command's detailed output is written to files while it runs. A short
summary and the last test-output lines are printed when it finishes. The
existing job timeouts still apply.

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

Allow up to three minutes for this small offline reproduction. Each compiler
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

The Rust test job runs this offline check with a three-minute step timeout and
uploads `cargo-cache-freshness-report` even on failure. Its fixtures and Cargo
home are isolated from the real compilation and its cache snapshot.

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

## Content-checked timestamp restoration

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
