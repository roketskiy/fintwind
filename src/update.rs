use serde::Deserialize;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const RELEASES_LATEST_URL: &str = "https://github.com/roketskiy/fintwind/releases/latest";

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/roketskiy/fintwind/releases/latest";

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
}

pub fn fetch_newer_release() -> Option<String> {
    let headers = [
        format!("User-Agent: fintwind/{APP_VERSION}"),
        "Accept: application/vnd.github+json".to_string(),
    ];
    let (status, body) = fintwind_protocol::http::http_get(LATEST_RELEASE_API, &headers, 15).ok()?;
    if status != 200 {
        return None;
    }
    let remote = serde_json::from_str::<LatestRelease>(&body)
        .ok()?
        .tag_name;
    if remote.is_empty() || !is_newer(&remote, APP_VERSION) {
        return None;
    }
    Some(remote.trim_start_matches(['v', 'V']).to_string())
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
