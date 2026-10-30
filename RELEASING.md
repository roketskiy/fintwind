# Releasing Fintwind

Users download a per-user **`fintwind-<version>-<arch>-Setup.exe`** (or the
portable zip) from the
[GitHub Releases](https://github.com/roketskiy/fintwind/releases) page. The app
selects the most recently published full release, regardless of numeric version
ordering, and can install it itself. Drafts and prereleases are excluded; the
installed release is not offered again. An installed
copy offers **Update now** on the update notice, which downloads the installer
for the running architecture, verifies it against the asset's `digest`, and runs
it silently. That makes both release files load-bearing — a release without its
installer leaves installed copies with no in-app update, only the manual
download row.

Fintwind versions live in the root `Cargo.toml` and are **independent of
waku**. Development happens on `main`. Never reuse a published `v*` tag, and
do not bump `Cargo.toml` until you are cutting the release — unreleased work
stays under `## [unreleased]` in [`CHANGELOG.md`](CHANGELOG.md).

Cutting a release is: push a `v*` tag (or run the **Release** workflow). CI
builds both Windows architectures in parallel with checking the existing
**Tests** result for that exact commit. Both must succeed before CI opens a
draft GitHub release. Review the draft, then publish it.

- Windows packaging: [`scripts/bundle-windows.ts`](scripts/bundle-windows.ts)
- Release notes: [`scripts/changelog.ts`](scripts/changelog.ts)
- GitHub Actions: [`.github/workflows/release.yml`](.github/workflows/release.yml)
  builds Windows (x86_64, arm64) on a `v*` tag — or on a manual **Run workflow**,
  which takes the version from `Cargo.toml` — and opens a draft GitHub release.

---

## Channels

| Channel | Built by | Data directory | In-app updates |
| --- | --- | --- | --- |
| stable | CI on a `v*` tag, or a manual **Release** run | `Fintwind` (production) | checks GitHub releases |
| beta | **local builds only** (`bun scripts/bundle-windows.ts --channel=beta`) | `Fintwind` — the same directory as stable | disabled — there is no beta feed |
| dev | plain `cargo build` (debug), for agent development | `Fintwind Debug` (isolated) | disabled |

Every channel packages only `fintwind.exe` and `fintwind-daemon.exe`.

GitHub builds and publishes the **stable channel only**: the Release workflow
passes `--channel=stable` explicitly, and there is no beta job, input, tag,
prerelease, or artifact. Do not add one — beta exists as a local convenience.

Sharing the production directory is deliberate (beta is a feature-ahead
stable, not a second profile), and it carries two consequences. Beta and
stable must not run at the same time, and data a newer beta has already
migrated is not guaranteed to reopen in an older stable — schema compatibility
runs forward only. A beta updates by being rebuilt; the app skips its update
check outside the stable channel so a personal beta is never "updated" into
the published stable over it.

Stable and beta builds write the same `target/release` executables, so
building one after the other replaces them in place. Each
`bundle-windows.ts` run packages immediately after its own build, so that is
expected, not a conflict.

---

## One-time setup

The release runs on [Bun](https://bun.sh).

### Windows Authenticode (optional)

Set `WINDOWS_CERTIFICATE` (base64 `.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD`
to Authenticode-sign the executables and installer. Without those, CI
packages unsigned binaries and SmartScreen will warn.

---

## Cutting a release

1. On `main`, set `version` in the root `Cargo.toml` to the intended unused
   Fintwind version and update the root package entry in `Cargo.lock`.
2. Rename `## [unreleased]` in [`CHANGELOG.md`](CHANGELOG.md) to
   `## [<version>]` and add a fresh empty `## [unreleased]` above it.
3. Update both README feature lists and state the OpenCode compatibility scope
   in the release notes. Keep release notes focused on user-visible changes;
   put verification procedures and results in the linked developer documents.
   Use recorded results to establish compatibility; an installed CLI version
   alone is not evidence. Commit, for example
   `chore: release <version>`.
4. Tag that commit `v<version>` (`git tag v<version>`) and push the branch
   plus that exact tag (`git push --atomic origin main v<version>`), or run **Release** from
   Actions without a tag (it reads `Cargo.toml`).
5. Review the draft GitHub release, then publish it.

### Release safeguards and recovery

Before building, CI checks that `Cargo.toml`, `Cargo.lock`, the pushed tag,
and a non-empty CHANGELOG section agree. It refuses to replace a published
release or reuse a tag pointing to another commit. An existing draft can be
retried only when its tag, or its exact `target_commitish` if untagged, identifies
the commit being built. CI repeats these checks and the same-commit Tests gate
immediately before updating the draft.

The release gate accepts the latest **Tests** run for the exact commit from a
push to `main` or a manual run. A PR run checks a synthetic merge and does not
authorize a release. Existing running checks are awaited for up to 90 minutes;
failed, cancelled, or skipped checks block the draft. CI allows 30 seconds for
a newly pushed commit's Tests run to appear, then stops if none exists. It
never triggers a second Tests run or requires Actions write permission.

If checks are missing or cancelled, run **Tests** manually on a branch or tag
that still points to the release commit, then rerun **Release**. Never move an
existing release tag to repair a failed check. A retry of a completed published
release requires a new version instead.

### CI caches and diagnostics

`.github/rust-version` is the CI toolchain source. The shared setup action
separates Cargo dependency downloads from compiler artifacts. Test,
release-check, and release-build caches have disjoint namespaces; build keys
include the source commit. Only successful jobs on `main` save compiler
artifacts, so PRs and version tags do not create large, low-reuse build caches.
Manual Release runs on `main` can seed the release-build cache; tag builds can
restore it but do not save a tag-specific copy. Old cache entries remain subject
to GitHub's normal eviction policy.

CI disables incremental compilation, and disables debug symbols for debug
tests only. Local development and shipped release settings are unchanged.
Compiler caches exclude incremental state, top-level binaries, installers,
and archives. The first run with the new cache scheme is cold; assess the speed
and cache size from subsequent successful runs rather than assuming a speedup.

Failed Rust tests upload available recovery E2E `result.json` files for seven
days. The Workflow tools job checks release automation against an isolated
loopback API and always uploads its JSON verification report.

### OpenCode compatibility

Fintwind uses OpenCode 2 (`@opencode/cli`), not OpenCode 1.x. The optional
scheduled task MCP requires OpenCode 2.1.4+ and uses an experimental client-hosted
MCP interface. Keep that requirement in both READMEs and the release notes.
The General setting starts the connection automatically; OpenCode 2.1.4 has no
additional experimental configuration flag for it.

The [scheduled task MCP reference](docs/scheduled-tasks-mcp.md) records the
verified version and scope. Run its isolated black-box check when changing this
integration or the supported OpenCode version. Check the requirements of other
features separately before claiming a wider compatibility range.

### What CI produces

- `fintwind-<version>-x86_64-pc-windows-msvc.zip`
- `fintwind-<version>-aarch64-pc-windows-msvc.zip`
- `fintwind-<version>-x86_64-Setup.exe`
- `fintwind-<version>-aarch64-Setup.exe`
- `latest-windows.txt` — the version string, attached to the GitHub release

[`scripts/bundle-windows.ts`](scripts/bundle-windows.ts) builds the zip and
the installer, driving
[`resources/windows/fintwind.iss`](resources/windows/fintwind.iss) through Inno
Setup's `ISCC`. The installer is **per-user** (`PrivilegesRequired=lowest`,
`%LOCALAPPDATA%\Programs\Fintwind`) — no elevation required.

**Never change `AppId` in `fintwind.iss`.** It is how Windows recognizes an
existing install; a new one turns every reinstall into a second copy in
Add/Remove Programs.

Configure these repository secrets first:

| Secret | Purpose |
| --- | --- |
| `WINDOWS_CERTIFICATE` | optional; base64-encoded Authenticode `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | optional; password for that `.pfx` |

---

## Notes

- **Two artifacts per architecture:** the Setup.exe (what people download)
  and a portable `.zip`.
- **Platform artifacts:** Windows CI produces `fintwind-<v>-<target>.zip` and
  `fintwind-<v>-<arch>-Setup.exe`.
