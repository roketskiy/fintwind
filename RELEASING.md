# Releasing Fintwind

Fintwind auto-updates from a **Cloudflare R2** bucket served at
**`https://releases.fintwind.sh`**. New users download a per-user
**`fintwind-<version>-<arch>-Setup.exe`**; existing users get in-app updates
via [`src/updater.rs`](src/updater.rs), which reads the architecture-specific
appcast, verifies each build's EdDSA signature, and hands the installer to
Inno Setup with `/SILENT`.

Once set up, cutting a release is: push a `v*` tag (or run the **Release**
workflow). CI builds both Windows architectures, signs the update feeds, and
opens a draft GitHub release. Publishing that draft syncs the assets to R2.

- Updater code: [`src/updater.rs`](src/updater.rs)
- Public key: [`resources/sparkle-public-ed-key.txt`](resources/sparkle-public-ed-key.txt)
- Windows packaging: [`scripts/bundle-windows.ts`](scripts/bundle-windows.ts)
- Update feeds: [`scripts/appcast-windows.ts`](scripts/appcast-windows.ts)
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

### 1. EdDSA signing keys

Updates are signed with an ed25519 key. The private half lives in the
`SPARKLE_PRIVATE_KEY` repository secret; the public half ships in
[`resources/sparkle-public-ed-key.txt`](resources/sparkle-public-ed-key.txt)
and is compiled into the app by `build.rs`.

> Lose the private key and existing installs can never update again. Keep
> the backup current.

`scripts/appcast-windows.ts` refuses to sign when the private key does not
derive that public key — signing with the wrong key ships a feed the app
rejects.

### 2. Windows Authenticode (optional)

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
- `appcast-windows-x86_64.xml`
- `appcast-windows-aarch64.xml`
- `latest-windows.txt` — the version download pages resolve "latest" to

[`scripts/bundle-windows.ts`](scripts/bundle-windows.ts) builds the zip and
the installer, driving
[`resources/windows/fintwind.iss`](resources/windows/fintwind.iss) through Inno
Setup's `ISCC`. The installer is **per-user** (`PrivilegesRequired=lowest`,
`%LOCALAPPDATA%\Programs\Fintwind`) — no elevation, which is exactly what lets
the updater re-run it silently.

**Never change `AppId` in `fintwind.iss`.** It is how Windows recognizes an
existing install; a new one turns every update into a second copy in
Add/Remove Programs.

### The Windows update feed

[`src/updater.rs`](src/updater.rs) fetches the appcast, compares versions,
downloads, verifies the EdDSA signature, and hands the installer to Inno Setup
with `/SILENT`. The installer closes Fintwind, replaces it, and starts it again.

- **One feed per architecture.** A Sparkle-format appcast cannot say which
  binary an item is for, and the client picks its feed at compile time.
- [`scripts/appcast-windows.ts`](scripts/appcast-windows.ts) signs the feeds in
  the draft-release job — the only one holding both installers. It signs with
  Node's Ed25519 over `SPARKLE_PRIVATE_KEY`.
- The step pulls the live feeds down first and merges, so previously published
  releases keep their entries.

`appcast-windows-*.xml` and `latest-windows.txt` are the bucket's mutable
pointers and upload with a short cache lifetime; everything else is versioned
and cached forever.

Publishing that GitHub release (or running **Sync release** from Actions)
uploads the assets to the `fintwind-releases` R2 bucket. Configure these
repository secrets first:

| Secret | Purpose |
| --- | --- |
| `SPARKLE_PRIVATE_KEY` | EdDSA private key for the Windows appcast |
| `WINDOWS_CERTIFICATE` | optional; base64-encoded Authenticode `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | optional; password for that `.pfx` |
| `FINTWIND_ANALYTICS_ENDPOINT` | optional; embedded in the Windows CI build |
| `FINTWIND_ANALYTICS_WEBSITE_ID` | optional; embedded in the Windows CI build |
| `R2_ACCOUNT_ID` | Cloudflare account id for the R2 API |
| `R2_ACCESS_KEY_ID` | R2 Object Read & Write token |
| `R2_SECRET_ACCESS_KEY` | matching secret |
| `R2_BUCKET` | optional; defaults to `fintwind-releases` |

### Options

| Flag / Env | Default | Purpose |
| --- | --- | --- |
| `FINTWIND_DOWNLOAD_URL_PREFIX` | `https://releases.fintwind.sh/` | base URL in the appcast |
| `WINDOWS_CERTIFICATE` | unset | Authenticode `.pfx` (base64) |
| `WINDOWS_CERTIFICATE_PASSWORD` | unset | password for that `.pfx` |

---

## Notes

- **Two artifacts per architecture:** the Setup.exe (what people download and
  what the updater installs) and a portable `.zip`.
- **Debug builds never update themselves.** `Updater::init` returns `None`
  under `debug_assertions`, so the dev watcher's app can't offer to replace
  itself with a production Fintwind. Set `FINTWIND_FORCE_UPDATER=1` to exercise the
  real flow from a debug binary anyway. For UI-only testing, start the watcher
  with `FINTWIND_PREVIEW_UPDATE=1`; the sidebar immediately shows an available
  update and clicking it changes to the spinner without installing anything.
- **Automatic and explicit checks have separate presentation.** Scheduled
  checks stay silent until the sidebar update button appears. Choosing
  **Check for Updates…** starts a user-initiated check.
- **Old archives stay in R2** so far-behind users can still be served.
- **Platform artifacts:** keep the bucket layout flat and architecture-tagged
  by artifact name. Windows CI produces `fintwind-<v>-<target>.zip` and
  `fintwind-<v>-<arch>-Setup.exe`, then updates itself from
  `appcast-windows-<arch>.xml`.
