//! Bounded project-scoped file reads. The caller resolves `project_root` from
//! host task state, never from a device-supplied directory.
//!
//! Failure modes: absolute/drive/UNC/ADS paths, traversal, missing files,
//! intermediate links escaping the project, a link replacement between check
//! and open, directories/devices, invalid UTF-8, growing files, oversized pages,
//! stale offsets after a file changes, and opening a FIFO blocking the host.
//! An in-project alias to a denied credential path must also be refused.
//! Hard links have no unique resolved target, so multiple-name files are not
//! remotely readable even when the requested alias itself looks harmless.
//! Validate the opened handle's target,
//! bound the actual read (not just metadata), and never return absolute paths.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use fintwind_remote_protocol::RejectCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_PAGE_BYTES: usize = 48 * 1024;
pub const MAX_RELATIVE_PATH_BYTES: usize = 1024;

pub type ReadError = (RejectCode, String);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileReadRequest {
    pub project_id: uuid::Uuid,
    pub path: String,
    #[serde(default)]
    pub offset_bytes: usize,
    #[serde(default = "default_page_bytes")]
    pub page_bytes: usize,
    /// A later page must name the previous page's revision, or it is refused.
    pub revision: Option<String>,
}

fn default_page_bytes() -> usize {
    MAX_PAGE_BYTES
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReadPage {
    pub project_id: uuid::Uuid,
    pub path: String,
    pub content: String,
    pub offset_bytes: usize,
    pub next_offset_bytes: Option<usize>,
    pub total_bytes: usize,
    pub revision: String,
}

fn invalid(message: &'static str) -> ReadError {
    (RejectCode::Malformed, message.to_owned())
}

fn unavailable() -> ReadError {
    (RejectCode::UnknownRoute, "This file is unavailable in the selected project.".to_owned())
}

pub(crate) fn relative_path(value: &str) -> Result<PathBuf, ReadError> {
    if value.is_empty() || value.len() > MAX_RELATIVE_PATH_BYTES
        || value.chars().any(|character| character.is_control())
        || value.contains(':') || value.contains('\\') || value.starts_with('/')
    {
        return Err(invalid("Choose a relative file path within the selected project."));
    }
    let path = PathBuf::from(value);
    if path.components().any(|part| !matches!(part, Component::Normal(_)))
        || value.split('/').any(|part| part.is_empty() || part == "." || part == ".."
            || part.ends_with(' ') || part.ends_with('.'))
    {
        return Err(invalid("Choose a relative file path within the selected project."));
    }
    // Configuration/credential storage is not a remote read interface. This
    // denylist is supplementary; it cannot guarantee source files lack secrets.
    if value.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        matches!(part.as_str(), ".git" | ".ssh" | ".aws" | ".azure" | ".gnupg"
            | ".opencode" | ".fintwind" | "credentials" | "credentials.json"
            | "auth.json" | "opencode.json" | "opencode.jsonc" | "id_rsa"
            | "id_ed25519") || part == ".env" || part.starts_with(".env.")
            || part.ends_with(".pem") || part.ends_with(".pfx") || part.ends_with(".key")
    }) {
        return Err(invalid("Credential and private configuration files are not available remotely."));
    }
    Ok(path)
}

fn validate_resolved_relative(root: &Path, target: &Path) -> Result<(), ReadError> {
    let relative = target.strip_prefix(root).map_err(|_| unavailable())?;
    let relative = relative.to_str().ok_or_else(unavailable)?;
    // The denylist follows the actual link destination too. Display continues
    // to use only the requested project-relative label, never this full path.
    relative_path(&relative.replace('\\', "/")).map(|_| ())
}

/// Resolve the opened file rather than trusting a pre-open symlink check.
#[cfg(windows)]
pub(crate) fn opened_path(file: &File) -> Result<PathBuf, ReadError> {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    let mut buffer = vec![0_u16; 32768];
    // SAFETY: `file` owns a valid handle for the entire call and the buffer
    // remains writable for its declared length. The default mode resolves all
    // links to a normalized volume path, including any replaced intermediate.
    let length = unsafe {
        GetFinalPathNameByHandleW(file.as_raw_handle(), buffer.as_mut_ptr(), buffer.len() as u32, 0)
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(unavailable());
    }
    let name = std::ffi::OsString::from_wide(&buffer[..length]);
    Ok(PathBuf::from(name))
}

#[cfg(target_os = "linux")]
pub(crate) fn opened_path(file: &File) -> Result<PathBuf, ReadError> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(|_| unavailable())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn opened_path(_file: &File) -> Result<PathBuf, ReadError> {
    // Fail closed on platforms without an implemented handle-target verifier.
    Err(invalid("Remote file reads are unavailable on this platform."))
}

fn validate_single_link(file: &File) -> Result<(), ReadError> {
    #[cfg(windows)]
    let count = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
        // SAFETY: this Win32 output type consists only of integer fields and
        // FILETIME integer pairs; an all-zero value is a valid empty buffer.
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the file owns a live handle and `information` is a correctly
        // sized writable Win32 output structure for the duration of this call.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
            return Err(unavailable());
        }
        u64::from(information.nNumberOfLinks)
    };
    #[cfg(unix)]
    let count = {
        use std::os::unix::fs::MetadataExt;
        file.metadata().map_err(|_| unavailable())?.nlink()
    };
    #[cfg(not(any(windows, unix)))]
    let count = 0_u64;
    if count != 1 { return Err(unavailable()); }
    Ok(())
}

pub fn read_file(project_root: &Path, request: &FileReadRequest) -> Result<FileReadPage, ReadError> {
    if request.page_bytes == 0 || request.page_bytes > MAX_PAGE_BYTES {
        return Err(invalid("The requested file page is too large."));
    }
    if request.offset_bytes > 0 && request.revision.is_none() {
        return Err(invalid("Refresh the file before reading another page."));
    }
    let content = read_text_snapshot(project_root, &request.path)?;
    let revision = format!("{:x}", Sha256::digest(content.as_bytes()));
    if request.revision.as_ref().is_some_and(|expected| expected != &revision) {
        return Err(invalid("This file changed. Refresh it before reading another page."));
    }
    if request.offset_bytes > content.len() || !content.is_char_boundary(request.offset_bytes) {
        return Err(invalid("The requested file position is invalid. Refresh the file."));
    }
    let mut end = request.offset_bytes.saturating_add(request.page_bytes).min(content.len());
    while end > request.offset_bytes && !content.is_char_boundary(end) { end -= 1; }
    if end == request.offset_bytes && end < content.len() {
        return Err(invalid("The requested page is too small to display this text."));
    }
    Ok(FileReadPage {
        project_id: request.project_id,
        path: request.path.clone(),
        content: content[request.offset_bytes..end].to_owned(),
        offset_bytes: request.offset_bytes,
        next_offset_bytes: (end < content.len()).then_some(end),
        total_bytes: content.len(),
        revision,
    })
}

/// Capture only the file actually opened under the trusted project root.
/// Diff reads reuse this verifier instead of letting Git follow a mutable
/// worktree pathname between a symlink check and its own open.
pub(crate) fn read_text_snapshot(project_root: &Path, path: &str) -> Result<String, ReadError> {
    let bytes = read_bytes_snapshot(project_root, path)?;
    let content = std::str::from_utf8(&bytes)
        .map_err(|_| invalid("This file is not UTF-8 text."))?;
    if content.contains('\0') {
        return Err(invalid("Binary files are not available in the text viewer."));
    }
    Ok(content.to_owned())
}

/// Internal binary snapshot for byte comparison. Remote file reads still only
/// expose UTF-8 text; unchanged binary assets must not block a text diff.
pub(crate) fn read_bytes_snapshot(project_root: &Path, path: &str) -> Result<Vec<u8>, ReadError> {
    let relative = relative_path(path)?;
    let root = project_root.canonicalize().map_err(|_| unavailable())?;
    if !root.is_dir() {
        return Err(unavailable());
    }
    let target = root.join(&relative).canonicalize().map_err(|_| unavailable())?;
    if !target.starts_with(&root) || target == root {
        return Err(unavailable());
    }
    validate_resolved_relative(&root, &target)?;
    if !target.metadata().map_err(|_| unavailable())?.is_file() {
        return Err(unavailable());
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        // A local writer can replace a regular file with a FIFO between the
        // metadata check and open. Nonblocking open cannot hang the host; the
        // opened-handle check below still rejects every non-regular object.
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK)
            .open(&target).map_err(|_| unavailable())?
    };
    #[cfg(not(unix))]
    let file = File::open(&target).map_err(|_| unavailable())?;
    let actual_target = opened_path(&file)?;
    if !actual_target.starts_with(&root) || actual_target == root {
        return Err(unavailable());
    }
    validate_resolved_relative(&root, &actual_target)?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file() {
        return Err(unavailable());
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(invalid("This file is too large to view remotely."));
    }
    validate_single_link(&file)?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(MAX_FILE_BYTES));
    (&file).take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| unavailable())?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid("This file is too large to view remotely."));
    }
    let after = file.metadata().map_err(|_| unavailable())?;
    if metadata.len() != after.len() || metadata.modified().ok() != after.modified().ok() {
        return Err(invalid("This file changed while loading. Refresh it."));
    }
    validate_single_link(&file)?;
    Ok(bytes)
}
