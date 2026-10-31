# Cargo cache diagnostics

## Scope

The Tests workflow records Cargo fingerprint decisions for both debug tests
and the stable release compile check. This is instrumentation, not a cache
policy change: the setup action, cache keys, cached paths, source timestamps,
and compiler settings are unchanged.

`scripts/cargo-cache-diagnostics.ps1` executes the existing Cargo commands and
preserves their exit codes. The workflow uploads diagnostics on success and
failure, retaining them for seven days:

- `cargo-cache-diagnostics-test`
- `cargo-cache-diagnostics-release-check`

These artifacts become available only after this workflow change is pushed
and the corresponding job runs. No remote diagnostic run was performed during
the initial local investigation.

## Artifact contents

| File | Contents |
| --- | --- |
| `cargo.stderr.log` | Full Cargo progress, compiler diagnostics, and fingerprint trace |
| `cargo.stdout.log` | Test output |
| `fingerprint-summary.log` | Dirty/stale reasons, fingerprint errors, completion, and compiler errors |
| `diagnostics.json` | Cargo/rustc versions, command, profile, selected compiler environment, timing, exit code, and pre-build timestamps |

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

## Local finding and remaining boundary

The controlled reproduction confirms that unchanged path-dependency source
bytes with newer modification times can produce:

```text
stale: changed ".../vendor/gpui-pre/src/lib.rs"
dirty: FsStatusOutdated(StaleItem(ChangedFile { ... }))
```

The application then becomes dirty through `StaleDepFingerprint`. With the
same cached artifacts and the original timestamps restored, the fixture stays
fresh. Genuine source changes still rebuild.

This establishes the timestamp mechanism locally, not the first invalidation
in the real GitHub Runner. The previous CI logs showed repeated compilation
of real vendored GPUI but did not include fingerprint reasons. A targeted
production fix remains deferred until the newly recorded Runner trace confirms
the trigger. No timestamps or existing caches were rewritten as part of this
diagnostic change.
