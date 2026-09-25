use serde::Deserialize;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const RELEASES_LATEST_URL: &str = "https://github.com/roketskiy/fintwind/releases/latest";

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/roketskiy/fintwind/releases/latest";

#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes: String,
    pub url: String,
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

pub fn fetch_newer_release() -> Option<ReleaseInfo> {
    let headers = [
        format!("User-Agent: fintwind/{APP_VERSION}"),
        "Accept: application/vnd.github+json".to_string(),
    ];
    let (status, body) =
        fintwind_protocol::http::http_get(LATEST_RELEASE_API, &headers, 15).ok()?;
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
    })
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
