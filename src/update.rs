//! Release discovery and the in-app update path.
//!
//! A release is published by `scripts/bundle-windows.ts` and carries an
//! installer for each architecture. That installer is what makes an in-app
//! update possible at all: the app downloads the one matching its own
//! architecture, checks it against the digest the API published with the
//! asset, and hands it to the same silent install a user would trigger by
//! hand. Everything here blocks and must run off the UI thread.

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context as _, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const RELEASES_LATEST_URL: &str = "https://github.com/roketskiy/fintwind/releases/latest";

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/roketskiy/fintwind/releases/latest";

/// The architecture token the release file names carry, which is the Rust
/// target's own name — exactly what `bundle-windows.ts` writes into them.
const INSTALLER_ARCH: &str = std::env::consts::ARCH;

/// The version check is a small JSON document; the installer is tens of
/// megabytes and deserves a budget a slow link can still finish inside.
const RELEASE_API_MAX_TIME_SECS: u64 = 15;
const INSTALLER_MAX_TIME_SECS: u64 = 600;

#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes: String,
    pub url: String,
    /// Every file attached to the release, so picking the installer needs no
    /// second round trip.
    pub assets: Vec<ReleaseAsset>,
}

/// One file attached to the release.
#[derive(Clone, Debug)]
pub struct ReleaseAsset {
    pub name: String,
    /// The download URL. It answers with a redirect to the object store, which
    /// `http_download` follows.
    pub url: String,
    pub size: u64,
    /// `sha256:<hex>`, as the API spells it. A release published by tooling
    /// that predates the `digest` field carries none.
    pub sha256: Option<String>,
}

impl ReleaseAsset {
    /// The asset's size as a label. A release installer is megabytes, so the
    /// byte count is only ever noise to the person waiting on it.
    pub fn size_label(&self) -> String {
        const MEGABYTE: f64 = 1024.0 * 1024.0;
        format!("{:.1} MB", self.size as f64 / MEGABYTE)
    }
}

impl ReleaseInfo {
    /// The installer for this build's architecture, or `None` when the release
    /// does not carry one: a fork that ships only archives, or a release from
    /// before installers were attached. Either way the manual download row is
    /// the way forward, which is why the caller offers no button instead of
    /// offering one that cannot work.
    pub fn installer(&self) -> Option<&ReleaseAsset> {
        let expected = format!("fintwind-{}-{INSTALLER_ARCH}-Setup.exe", self.version);
        self.assets.iter().find(|asset| asset.name == expected)
    }
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<LatestAsset>,
}

#[derive(Deserialize)]
struct LatestAsset {
    name: String,
    #[serde(default)]
    browser_download_url: Option<String>,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

pub fn fetch_newer_release() -> Option<ReleaseInfo> {
    let headers = [
        format!("User-Agent: fintwind/{APP_VERSION}"),
        "Accept: application/vnd.github+json".to_string(),
    ];
    let (status, body) =
        fintwind_protocol::http::http_get(LATEST_RELEASE_API, &headers, RELEASE_API_MAX_TIME_SECS)
            .ok()?;
    if status != 200 {
        return None;
    }
    let release = serde_json::from_str::<LatestRelease>(&body).ok()?;
    if release.tag_name.is_empty() || !is_newer(&release.tag_name, APP_VERSION) {
        return None;
    }

    Some(ReleaseInfo {
        version: release.tag_name.trim_start_matches(['v', 'V']).to_string(),
        notes: release.body.unwrap_or_default(),
        url: release
            .html_url
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| RELEASES_LATEST_URL.to_string()),
        assets: release
            .assets
            .into_iter()
            .filter_map(|asset| {
                Some(ReleaseAsset {
                    name: asset.name,
                    url: asset.browser_download_url?,
                    size: asset.size,
                    sha256: asset
                        .digest
                        .as_deref()
                        .and_then(sha256_digest)
                        .map(str::to_owned),
                })
            })
            .collect(),
    })
}

/// The hex half of the API's `"sha256:<hex>"` digest. Anything else — another
/// algorithm, an empty value, a release that published none — reads as "no
/// digest", which leaves the size check as the only verification.
fn sha256_digest(digest: &str) -> Option<&str> {
    digest.strip_prefix("sha256:").filter(|hex| !hex.is_empty())
}

/// Where a downloaded installer lands. The temp directory is the right home:
/// the file is run once, and the next update supersedes it.
pub fn installer_download_path(version: &str) -> PathBuf {
    std::env::temp_dir()
        .join("fintwind-update")
        .join(format!("fintwind-{version}-{INSTALLER_ARCH}-Setup.exe"))
}

/// Downloads `asset` to `destination` and refuses to leave a file behind that
/// is not the one the release published, so nothing gets executed without
/// having been checked. Blocking; run on a background executor.
pub fn download_installer(asset: &ReleaseAsset, destination: &Path) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let headers = [
        format!("User-Agent: fintwind/{APP_VERSION}"),
        "Accept: application/octet-stream".to_string(),
    ];
    let written = fintwind_protocol::http::http_download(
        &asset.url,
        &headers,
        destination,
        INSTALLER_MAX_TIME_SECS,
    )
    .with_context(|| format!("could not download {}", asset.name))?;
    // Hashing a 20 MB file is only worth it when the release published a
    // digest to compare it with.
    let digest = match asset.sha256.as_deref().and_then(sha256_digest) {
        Some(_) => Some(sha256_file(destination)?),
        None => None,
    };
    verify_installer(asset, written, digest.as_deref()).inspect_err(|_| {
        let _ = std::fs::remove_file(destination);
    })
}

/// Rejects an installer that is not the one the release published. The byte
/// count catches a truncated or substituted transfer even when no digest rode
/// along; the digest catches one that survived with the right length. Pure, so
/// the failure modes are testable without a network.
fn verify_installer(
    asset: &ReleaseAsset,
    written: u64,
    digest: Option<&str>,
) -> anyhow::Result<()> {
    if asset.size > 0 && written != asset.size {
        bail!(
            "the downloaded installer is {written} bytes, the release reports {}",
            asset.size
        );
    }
    match (asset.sha256.as_deref().and_then(sha256_digest), digest) {
        (Some(expected), Some(actual)) if actual != expected => {
            bail!("the downloaded installer does not match the digest published with the release")
        }
        // The digest was published but the file could not be hashed, so it
        // stays unverified — and unverified is not installable.
        (Some(_), None) => bail!("the downloaded installer could not be hashed"),
        _ => Ok(()),
    }
}

/// Streamed, because an installer is large enough that holding it in memory to
/// hash it would be the one pointless cost in this flow.
fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("could not read {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Whether this copy of fintwind was installed rather than unpacked from the
/// portable archive. Inno Setup writes `unins000.exe` beside the executable it
/// installed and nowhere else, which makes that sibling the marker.
///
/// The update installs *through* that installer, so it can only replace an
/// installed copy: pointed at a portable one it would quietly create a second
/// install in `{autopf}` and leave the original in place. Callers ask before
/// offering the button. Blocking, so it belongs on a background executor.
pub fn is_installed_copy() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|executable| {
            executable
                .parent()
                .map(|directory| directory.join("unins000.exe"))
        })
        .is_some_and(|uninstaller| uninstaller.is_file())
}

/// Starts the installer and returns. The caller must quit right after: Setup
/// cannot replace this executable while it runs, so the app getting out of the
/// way is the whole of its part.
///
/// `resources/windows/fintwind.iss` is what makes this a silent,
/// non-interactive install. `PrivilegesRequired=lowest` keeps it below a UAC
/// prompt, `CloseApplications=force` lets it take over a running fintwind, and
/// its `[Run]` entry launches the freshly installed executable once Setup
/// finishes — so nothing here waits for the child, and a portable copy is never
/// handed to one.
pub fn launch_installer(installer: &Path) -> anyhow::Result<()> {
    let mut command = ProcessCommand::new(installer);
    #[cfg(windows)]
    {
        // Both processes are GUI-subsystem binaries and so get no console
        // either way; the flag keeps that from depending on the child.
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .spawn()
        .with_context(|| format!("could not launch {}", installer.display()))?;
    Ok(())
}

fn is_newer(remote: &str, local: &str) -> bool {
    match (parse_version(remote), parse_version(local)) {
        (Some(remote), Some(local)) => remote > local,
        _ => false,
    }
}

fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    let numeric = trimmed
        .split(['-', '+'])
        .next()
        .filter(|part| !part.is_empty())?;
    let mut parts = numeric.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256: &str = "sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";
    // The same digest as bare hex, which is what `sha256_file` returns.
    const SHA256_HEX: &str = "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";

    fn asset(name: &str, size: u64, sha256: Option<&str>) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            url: "https://example.invalid/fintwind-setup.exe".to_owned(),
            size,
            sha256: sha256.map(str::to_owned),
        }
    }

    fn release(assets: Vec<ReleaseAsset>) -> ReleaseInfo {
        ReleaseInfo {
            version: "0.2.4".to_owned(),
            notes: String::new(),
            url: String::new(),
            assets,
        }
    }

    /// Both architectures ship, because this test has to hold on a machine of
    /// either one.
    fn published_release() -> ReleaseInfo {
        release(vec![
            asset("fintwind-0.2.4-x86_64-pc-windows-msvc.zip", 9, None),
            asset("fintwind-0.2.4-aarch64-pc-windows-msvc.zip", 9, None),
            asset("fintwind-0.2.3-x86_64-Setup.exe", 12, None),
            asset("fintwind-0.2.4-aarch64-Setup.exe", 30, None),
            asset("fintwind-0.2.4-x86_64-Setup.exe", 30, Some(SHA256)),
        ])
    }

    #[test]
    fn the_installer_is_the_asset_for_the_running_architecture() {
        let installer = published_release()
            .installer()
            .expect("this architecture publishes an installer")
            .clone();
        assert_eq!(
            installer.name,
            format!("fintwind-0.2.4-{INSTALLER_ARCH}-Setup.exe")
        );
        // The installer for the other architecture and for the previous
        // version are both sitting in the same release.
        assert_eq!(installer.size, 30);
    }

    #[test]
    fn a_release_without_this_architectures_installer_has_none() {
        let archive_only = release(vec![
            asset("fintwind-0.2.4-x86_64-pc-windows-msvc.zip", 9, None),
            asset("fintwind-0.2.4-aarch64-Setup.exe", 30, None),
            asset("fintwind-0.2.3-x86_64-Setup.exe", 12, None),
        ]);
        assert!(archive_only.installer().is_none());
    }

    #[test]
    fn an_installer_that_matches_its_published_digest_is_accepted() {
        let asset = asset("fintwind-0.2.4-x86_64-Setup.exe", 30, Some(SHA256));
        assert!(verify_installer(&asset, 30, Some(SHA256_HEX)).is_ok());
    }

    #[test]
    fn a_release_that_publishes_no_digest_still_has_to_match_its_size() {
        let asset = asset("fintwind-0.2.4-x86_64-Setup.exe", 30, None);
        assert!(verify_installer(&asset, 30, None).is_ok());
        assert!(verify_installer(&asset, 29, None).is_err());
    }

    #[test]
    fn an_installer_with_the_wrong_bytes_is_refused_even_at_the_right_size() {
        let asset = asset("fintwind-0.2.4-x86_64-Setup.exe", 30, Some(SHA256));
        assert!(verify_installer(&asset, 30, Some(&"0".repeat(64))).is_err());
        // A short transfer is caught by the size alone.
        assert!(verify_installer(&asset, 12, Some(SHA256_HEX)).is_err());
    }

    #[test]
    fn an_installer_that_published_a_digest_but_could_not_be_hashed_is_refused() {
        let asset = asset("fintwind-0.2.4-x86_64-Setup.exe", 30, Some(SHA256));
        assert!(verify_installer(&asset, 30, None).is_err());
    }

    #[test]
    fn a_digest_from_another_algorithm_is_not_a_sha256() {
        assert_eq!(
            sha256_digest("sha512:beef"),
            None,
            "only sha256 is verified, so anything else must read as no digest"
        );
        assert_eq!(sha256_digest(""), None);
        assert_eq!(sha256_digest(SHA256), Some(SHA256_HEX));
    }
}
