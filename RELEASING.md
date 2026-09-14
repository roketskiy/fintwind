# Releasing Fintwind

Users download a per-user **`fintwind-<version>-<arch>-Setup.exe`** (or the
portable zip) from the
[GitHub Releases](https://github.com/roketskiy/fintwind/releases) page. The app
does not update itself.

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

## One-time setup

The release runs on [Bun](https://bun.sh).

### Windows Authenticode (optional)

Set `WINDOWS_CERTIFICATE` (base64 `.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD`
to Authenticode-sign the executables and installer. Without those, CI
packages unsigned binaries and SmartScreen will warn.

---

## Cutting a release

1. On `main`, bump `version` in the root `Cargo.toml` (next unused Fintwind
   number — currently the line is `0.1.x`).
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
