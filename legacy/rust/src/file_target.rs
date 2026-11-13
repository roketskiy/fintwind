//! Lexical recognition only: rendering must never probe the filesystem.
use std::path::{Path, PathBuf};

fn positive_number(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<usize>().is_ok_and(|value| value > 0)
}

fn strip_location(target: &str) -> &str {
    if let Some((path, fragment)) = target.rsplit_once('#')
        && let Some(location) = fragment.strip_prefix('L')
        && match location.split_once('C') {
            Some((line, column)) => positive_number(line) && positive_number(column),
            None => positive_number(location),
        }
    {
        return path;
    }
    if let Some((path, last)) = target.rsplit_once(':')
        && positive_number(last)
    {
        return match path.rsplit_once(':') {
            Some((path, line)) if positive_number(line) => path,
            _ => path,
        };
    }
    target
}

/// Recognize explicit paths and bare filenames, but not commands or web URLs.
pub(crate) fn path(target: &str) -> Option<PathBuf> {
    parse(target, false)
}

fn parse(target: &str, allow_spaces: bool) -> Option<PathBuf> {
    let target = target.trim();
    if target
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
    {
        let url = url::Url::parse(target).ok()?;
        if let Ok(path) = url.to_file_path() {
            return Some(PathBuf::from(strip_location(&path.to_string_lossy())));
        }
        // POSIX daemon-host URLs must still be handled locally as file
        // actions (and rejected by the remote-host guard), not as web links.
        if url.host_str().is_none_or(|host| host == "localhost") {
            return Some(PathBuf::from(strip_location(&decode(url.path())?)));
        }
        return None;
    }
    let target = strip_location(target);
    let windows_drive = target
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && target.as_bytes().get(1) == Some(&b':')
        && matches!(target.as_bytes().get(2), Some(b'/' | b'\\'));
    if target.is_empty()
        || target.contains(['\n', '\r', '\0', '"', '<', '>', '|', '*', '?'])
        || (target.contains(':') && !windows_drive)
        || target.contains("://")
    {
        return None;
    }
    let explicit = target.starts_with(['/', '\\'])
        || target.starts_with("./")
        || target.starts_with("../")
        || target.starts_with(".\\")
        || target.starts_with("..\\")
        || windows_drive;
    // Spaces are valid in explicit paths. Unqualified snippets containing
    // spaces are more likely shell commands than filenames.
    if !explicit && !allow_spaces && target.chars().any(char::is_whitespace) {
        return None;
    }
    let filename = target.rsplit(['/', '\\']).next().unwrap_or(target);
    let filename_like = filename.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && !extension.is_empty()
            && extension
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }) || matches!(filename, "Dockerfile" | "Makefile" | "LICENSE" | "README");
    (explicit || target.contains(['/', '\\']) || filename_like).then(|| PathBuf::from(target))
}

pub(crate) fn resolve(target: &str, workspace: Option<&Path>) -> Option<PathBuf> {
    let path = parse(target, true)?;
    if path.has_root() {
        Some(path)
    } else {
        Some(workspace?.join(path))
    }
}

fn decode(value: &str) -> Option<String> {
    let mut output = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        output.push(if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)? as u8;
            let low = (bytes.next()? as char).to_digit(16)? as u8;
            high * 16 + low
        } else {
            byte
        });
    }
    String::from_utf8(output).ok()
}
