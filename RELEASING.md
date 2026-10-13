# Releasing Fintwind

Users download a per-user **`fintwind-<version>-<arch>-Setup.exe`** (or the
portable zip) from the
[GitHub Releases](https://github.com/roketskiy/fintwind/releases) page. The app
checks that page for a newer version and can install it itself: an installed
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
builds both Windows architectures and opens a draft GitHub release. Review the
draft, then publish it.

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

1. On `main`, bump `version` in the root `Cargo.toml` (next unused Fintwind
   number — currently the line is `0.2.x`).
2. Rename `## [unreleased]` in [`CHANGELOG.md`](CHANGELOG.md) to
   `## [<version>]` and add a fresh empty `## [unreleased]` above it.
3. Commit, for example `chore: release <version>`.
4. Tag that commit `v<version>` (`git tag v<version>`) and push the branch
   plus the tag (`git push origin main --tags`), or run **Release** from
   Actions without a tag (it reads `Cargo.toml`).
5. Review the draft GitHub release, then publish it.

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
