# Releasing Fintwind

Release assets are served from a **Cloudflare R2** bucket at
**`https://releases.fintwind.sh`**. Users download a per-user
**`fintwind-<version>-<arch>-Setup.exe`** (or the portable zip); the app does
not update itself.

Once set up, cutting a release is: push a `v*` tag (or run the **Release**
workflow). CI builds both Windows architectures and opens a draft GitHub
release. Publishing that draft syncs the assets to R2.

- Windows packaging: [`scripts/bundle-windows.ts`](scripts/bundle-windows.ts)
- Release notes: [`scripts/changelog.ts`](scripts/changelog.ts)
- GitHub Actions: [`.github/workflows/release.yml`](.github/workflows/release.yml)
  builds Windows (x86_64, arm64) on a `v*` tag — or on a manual **Run workflow**,
  which takes the version from `Cargo.toml` — and opens a draft GitHub release;
  [`.github/workflows/sync-release.yml`](.github/workflows/sync-release.yml)
  copies published assets into the R2 bucket.

---

## One-time setup

The release runs on [Bun](https://bun.sh) and needs
[rclone](https://rclone.org) on the machine that syncs to R2.

### Windows Authenticode (optional)

Set `WINDOWS_CERTIFICATE` (base64 `.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD`
to Authenticode-sign the executables and installer. Without those, CI
packages unsigned binaries and SmartScreen will warn.

---

## Cutting a release

1. Bump `version` in `Cargo.toml` and add a `## [<version>]` section to
   [`CHANGELOG.md`](CHANGELOG.md).
2. Push a `v<version>` tag, or run **Release** from Actions.
3. Review the draft GitHub release, then publish it.
4. Publishing syncs assets to R2 via **Sync release**.

### What CI produces

- `fintwind-<version>-x86_64-pc-windows-msvc.zip`
- `fintwind-<version>-aarch64-pc-windows-msvc.zip`
- `fintwind-<version>-x86_64-Setup.exe`
- `fintwind-<version>-aarch64-Setup.exe`
- `latest-windows.txt` — the version download pages resolve "latest" to

[`scripts/bundle-windows.ts`](scripts/bundle-windows.ts) builds the zip and
the installer, driving
[`resources/windows/fintwind.iss`](resources/windows/fintwind.iss) through Inno
Setup's `ISCC`. The installer is **per-user** (`PrivilegesRequired=lowest`,
`%LOCALAPPDATA%\Programs\Fintwind`) — no elevation required.

**Never change `AppId` in `fintwind.iss`.** It is how Windows recognizes an
existing install; a new one turns every reinstall into a second copy in
Add/Remove Programs.

`latest-windows.txt` is the bucket's mutable pointer and uploads with a short
cache lifetime; everything else is versioned and cached forever.

Publishing that GitHub release (or running **Sync release** from Actions)
uploads the assets to the `fintwind-releases` R2 bucket. Configure these
repository secrets first:

| Secret | Purpose |
| --- | --- |
| `WINDOWS_CERTIFICATE` | optional; base64-encoded Authenticode `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | optional; password for that `.pfx` |
| `R2_ACCOUNT_ID` | Cloudflare account id for the R2 API |
| `R2_ACCESS_KEY_ID` | R2 Object Read & Write token |
| `R2_SECRET_ACCESS_KEY` | matching secret |
| `R2_BUCKET` | optional; defaults to `fintwind-releases` |

---

## Notes

- **Two artifacts per architecture:** the Setup.exe (what people download)
  and a portable `.zip`.
- **Old releases stay in R2** so links to a specific version keep working.
- **Platform artifacts:** keep the bucket layout flat and architecture-tagged
  by artifact name. Windows CI produces `fintwind-<v>-<target>.zip` and
  `fintwind-<v>-<arch>-Setup.exe`.
