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
use std::time::Duration;

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

/// Mirrors that will relay a GitHub release asset, in the order a probe sees
/// them. They are community-run, which is exactly why the download is verified
/// against the digest the release published: a mirror serving anything else
/// fails that check before the installer is launched, so a mirror can cost time
/// and nothing more.
///
/// Ordered by measured throughput on a mainland connection: gh-proxy.com
/// delivered the 12 MB installer in about a second and ghfast.top in three and
/// a half. A source that answers a probe and then stalls mid-download is left
/// out on purpose — a slow success costs less than a dead one.
const ASSET_MIRRORS: &[&str] = &["https://gh-proxy.com/", "https://ghfast.top/"];

/// How much of the installer a source has to prove it will relay. Enough that
/// the transfer is real, small enough to be cheap on every candidate.
const PROBE_RANGE: &str = "0-65535";
const PROBE_MAX_TIME_SECS: u64 = 6;
/// Below this a 200 carried no installer, only an error page, so the source
/// must not win the choice.
const PROBE_MIN_BYTES: u64 = 4096;

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

/// Removes the installer this version was updated from, and any half-written
/// transfer the last attempt left behind.
///
/// Nothing else can do it: the app hands the installer to Setup.exe and quits,
/// so the process that downloaded it is gone before the install finishes. The
/// version now running is exactly the version that installer carried, which is
/// what makes the path nameable here. Every other leftover — a failed download,
/// a killed process — is either removed by its own failure path or overwritten
/// by the next attempt at the same version, so this closes the only case that
/// accumulates: one installer per successful update.
///
/// A delete of a file Setup.exe still has open fails on a sharing violation,
/// which is left to fail; the next launch tries again. Blocking, so it belongs
/// on a background executor.
pub fn discard_downloaded_installer() {
    let installer = installer_download_path(APP_VERSION);
    let mut partial = installer.as_os_str().to_owned();
    partial.push(".part");
    for path in [PathBuf::from(partial), installer] {
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Downloads `asset` from `source` and refuses to leave a file behind that is
/// not the one the release published, so nothing gets executed without having
/// been checked. Blocking; run on a background executor.
pub fn download_installer(
    asset: &ReleaseAsset,
    source: &DownloadSource,
    destination: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let headers = asset_headers();
    let written = fintwind_protocol::http::http_download(
        &source.url,
        &headers,
        destination,
        INSTALLER_MAX_TIME_SECS,
    )
    .with_context(|| {
        format!(
            "could not download {} via {}",
            asset.name,
            source.describe()
        )
    })?;
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

/// One place the installer can be fetched from.
#[derive(Clone, Debug)]
pub struct DownloadSource {
    /// The URL to fetch.
    pub url: String,
    /// `None` when this is the publisher's own host.
    pub mirror: Option<String>,
}

impl DownloadSource {
    /// Where the download came from, in a form a person can read in an error.
    pub fn describe(&self) -> String {
        self.mirror
            .clone()
            .unwrap_or_else(|| RELEASES_LATEST_URL.to_owned())
    }
}

/// Measures every source and returns the fastest that will actually relay the
/// installer, falling back to the publisher's own host when none answered — a
/// dead network fails there as it would anywhere, and no third party is
/// involved in that attempt.
///
/// The probes run concurrently, so choosing costs about one round trip rather
/// than one per source. Blocking; run on a background executor.
pub fn pick_source(asset: &ReleaseAsset, destination: &Path) -> DownloadSource {
    let candidates = candidate_sources(asset);
    let headers = asset_headers();
    let measured: Vec<Option<(Duration, u64)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let headers = &headers;
                let url = candidate.url.clone();
                let probe = probe_path(destination, index);
                scope.spawn(move || {
                    fintwind_protocol::http::http_probe(
                        &url,
                        headers,
                        PROBE_RANGE,
                        &probe,
                        PROBE_MAX_TIME_SECS,
                    )
                    .ok()
                    .map(|probed| (probed.elapsed, probed.bytes))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or(None))
            .collect()
    });
    // Each probe left its sample on disk; the real download writes its own path.
    for index in 0..candidates.len() {
        let _ = std::fs::remove_file(probe_path(destination, index));
    }
    match fastest_of(&measured) {
        Some(index) => candidates
            .into_iter()
            .nth(index)
            .expect("a measured index is a candidate"),
        None => candidates
            .into_iter()
            .next()
            .expect("the publisher's own host is always a candidate"),
    }
}

/// Every source that could serve `asset`, the publisher's own host first so a
/// tie in the measurement prefers it.
fn candidate_sources(asset: &ReleaseAsset) -> Vec<DownloadSource> {
    let mut candidates = vec![DownloadSource {
        url: asset.url.clone(),
        mirror: None,
    }];
    candidates.extend(ASSET_MIRRORS.iter().map(|mirror| DownloadSource {
        url: format!("{mirror}{}", asset.url),
        mirror: Some((*mirror).to_owned()),
    }));
    candidates
}

fn asset_headers() -> [String; 2] {
    [
        format!("User-Agent: fintwind/{APP_VERSION}"),
        "Accept: application/octet-stream".to_string(),
    ]
}

/// A disposable filename for one source's sample, so candidates measured at the
/// same time do not write the same file.
fn probe_path(destination: &Path, index: usize) -> PathBuf {
    let mut probe = destination.as_os_str().to_owned();
    probe.push(format!(".probe{index}"));
    PathBuf::from(probe)
}

/// The index of the fastest source that answered, or `None` when none did. A
/// tie keeps the earlier candidate, which is the publisher's own host. Pure, so
/// the ranking is testable without a network.
fn fastest_of(measured: &[Option<(Duration, u64)>]) -> Option<usize> {
    measured
        .iter()
        .enumerate()
        .filter_map(|(index, measured)| {
            let (elapsed, bytes) = (*measured)?;
            (bytes >= PROBE_MIN_BYTES).then_some((index, elapsed))
        })
        .min_by(|(_, left), (_, right)| left.cmp(right))
        .map(|(index, _)| index)
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
    fn the_fastest_source_that_answered_wins() {
        let measured = [
            Some((Duration::from_millis(900), PROBE_MIN_BYTES)),
            Some((Duration::from_millis(200), PROBE_MIN_BYTES)),
            None,
        ];
        // Index 1 answered fastest, so the publisher's own host loses to it.
        assert_eq!(fastest_of(&measured), Some(1));
    }

    #[test]
    fn a_source_that_answered_with_no_installer_never_wins() {
        let measured = [
            None,
            // A 200 that carried an error page: fast, but not a transfer.
            Some((Duration::from_millis(10), PROBE_MIN_BYTES - 1)),
            Some((Duration::from_millis(400), PROBE_MIN_BYTES)),
        ];
        assert_eq!(fastest_of(&measured), Some(2));
    }

    #[test]
    fn a_tie_keeps_the_publishers_own_host() {
        let both = Some((Duration::from_millis(300), PROBE_MIN_BYTES));
        assert_eq!(fastest_of(&[both, both]), Some(0));
    }

    #[test]
    fn no_source_that_answered_leaves_the_choice_empty() {
        assert_eq!(fastest_of(&[None, None]), None);
        assert_eq!(fastest_of(&[]), None);
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
