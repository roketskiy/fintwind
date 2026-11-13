# Releasing Fintwind

Users can download a per-user installer or portable archive from
[GitHub Releases](https://github.com/roketskiy/fintwind/releases). The release
contains the Go application and daemon. The Rust source remains in the
repository but is not built or released.

The root `VERSION` file contains the Fintwind version. Keep unreleased changes
under `## [unreleased]` in [CHANGELOG.md](CHANGELOG.md). Never reuse a
published version tag.

CI builds both Windows architectures and checks the existing **Tests** result
for the same commit before creating a draft release. Review the draft before
publishing it.

- Windows packaging: [`scripts/bundle-windows.ts`](scripts/bundle-windows.ts)
- Release notes: [`scripts/changelog.ts`](scripts/changelog.ts)
- GitHub Actions: [`.github/workflows/release.yml`](.github/workflows/release.yml)

## Channels

Stable packages are built by CI. Beta packages are local only:

```sh
bun scripts/bundle-windows.ts --channel=beta
```

Stable and beta use `%LOCALAPPDATA%\Fintwind` and must not run together. Data
migrated by a newer beta may not reopen in an older stable. Both builds write
to `target/release`. Each archive contains `fintwind.exe` and
`fintwind-daemon.exe` in the same directory.

## One-time setup

The release runs on [Bun](https://bun.sh).

### Windows Authenticode (optional)

Set `WINDOWS_CERTIFICATE` (base64 `.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD`
to Authenticode-sign the executables and installer. Without those, CI
packages unsigned binaries and SmartScreen will warn.

---

## Cutting a release

1. Set `VERSION` to an unused version on `main`.
2. Rename `## [unreleased]` in [CHANGELOG.md](CHANGELOG.md) to the version and
   add a new `## [unreleased]` heading.
3. Update both README feature lists and state the verified OpenCode compatibility
   scope in the release notes. Commit the changes.
4. Push the commit and its `v<version>` tag, or run **Release** manually for
   that commit.
5. Review the draft GitHub release, then publish it.

### Release safeguards and recovery

Before building, CI checks that `VERSION`, the pushed tag, and a non-empty
CHANGELOG section agree. It refuses to replace a published
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
