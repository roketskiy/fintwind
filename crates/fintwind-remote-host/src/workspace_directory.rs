//! Project-scoped, non-recursive directory pages. Failure modes are named
//! before implementation: device-supplied roots, traversal, credential names,
//! escaping links, directory replacement, unlimited entries, and stale pages.
//! No absolute paths or link targets are sent to the device.

use std::fs::File;
use std::path::{Path, PathBuf};

use fintwind_remote_protocol::RejectCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::workspace_reads::{ReadError, opened_path, relative_path};

pub const MAX_DIRECTORY_ENTRIES: usize = 4096;
pub const MAX_DIRECTORY_PAGE_ENTRIES: usize = 128;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoryReadRequest {
    pub project_id: uuid::Uuid,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub offset_entries: usize,
    #[serde(default = "default_page_entries")]
    pub page_entries: usize,
    pub revision: Option<String>,
}

fn default_page_entries() -> usize { MAX_DIRECTORY_PAGE_ENTRIES }

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub kind: DirectoryEntryKind,
    pub size_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DirectoryEntryKind { File, Directory }

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryReadPage {
    pub project_id: uuid::Uuid,
    pub path: String,
    pub entries: Vec<DirectoryEntry>,
    pub offset_entries: usize,
    pub next_offset_entries: Option<usize>,
    pub total_entries: usize,
    pub revision: String,
}

fn unavailable() -> ReadError {
    (RejectCode::UnknownRoute, "This directory is unavailable in the selected project.".to_owned())
}

fn malformed(message: &'static str) -> ReadError { (RejectCode::Malformed, message.to_owned()) }

pub(crate) fn open_directory(path: &Path) -> Result<File, ReadError> {
    if !path.metadata().map_err(|_| unavailable())?.is_dir() { return Err(unavailable()); }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let handle = options.open(path).map_err(|_| unavailable())?;
    if !handle.metadata().map_err(|_| unavailable())?.is_dir() { return Err(unavailable()); }
    Ok(handle)
}

pub fn read_directory(root: &Path, request: &DirectoryReadRequest) -> Result<DirectoryReadPage, ReadError> {
    if request.page_entries == 0 || request.page_entries > MAX_DIRECTORY_PAGE_ENTRIES {
        return Err(malformed("The requested directory page is too large."));
    }
    if request.offset_entries > 0 && request.revision.is_none() {
        return Err(malformed("Refresh the directory before reading another page."));
    }
    let root = root.canonicalize().map_err(|_| unavailable())?;
    let relative = if request.path.is_empty() { PathBuf::new() } else { relative_path(&request.path)? };
    let target = root.join(relative).canonicalize().map_err(|_| unavailable())?;
    if !target.starts_with(&root) { return Err(unavailable()); }
    if target != root {
        let resolved = target.strip_prefix(&root).map_err(|_| unavailable())?;
        relative_path(&resolved.to_str().ok_or_else(unavailable)?.replace('\\', "/"))?;
    }
    let handle = open_directory(&target)?;
    let opened = opened_path(&handle)?;
    if opened != target || !opened.starts_with(&root) { return Err(unavailable()); }
    let before = handle.metadata().map_err(|_| unavailable())?;
    let mut entries = Vec::new();
    let mut scanned = 0;
    for child in std::fs::read_dir(&target).map_err(|_| unavailable())? {
        scanned += 1;
        if scanned > MAX_DIRECTORY_ENTRIES {
            return Err(malformed("This directory has too many entries to view remotely."));
        }
        let child = child.map_err(|_| unavailable())?;
        let name = child.file_name();
        let Some(name) = name.to_str() else { continue; };
        let path = if request.path.is_empty() { name.to_owned() } else { format!("{}/{name}", request.path) };
        if relative_path(&path).is_err() { continue; }
        // Links are not traversed by the directory browser. Direct file reads
        // have their own opened-handle target verifier for safe in-root links.
        let metadata = match child.path().symlink_metadata() { Ok(value) => value, Err(_) => continue };
        if metadata.file_type().is_symlink() { continue; }
        let actual = match child.path().canonicalize() { Ok(value) => value, Err(_) => continue };
        if !actual.starts_with(&opened) || actual.parent() != Some(opened.as_path()) { continue; }
        let resolved = match actual.strip_prefix(&root).ok().and_then(Path::to_str) { Some(value) => value, None => continue };
        if relative_path(&resolved.replace('\\', "/")).is_err() { continue; }
        let (kind, size_bytes) = if metadata.is_dir() {
            (DirectoryEntryKind::Directory, None)
        } else if metadata.is_file() {
            (DirectoryEntryKind::File, Some(metadata.len()))
        } else { continue; };
        entries.push(DirectoryEntry { name: name.to_owned(), path, kind, size_bytes });
    }
    let after = handle.metadata().map_err(|_| unavailable())?;
    if target.canonicalize().ok().as_ref() != Some(&opened) || before.modified().ok() != after.modified().ok() {
        return Err(malformed("This directory changed while loading. Refresh it."));
    }
    entries.sort_by(|left, right| {
        let left_directory = matches!(left.kind, DirectoryEntryKind::Directory);
        let right_directory = matches!(right.kind, DirectoryEntryKind::Directory);
        right_directory.cmp(&left_directory).then_with(|| left.name.cmp(&right.name))
    });
    let mut digest = Sha256::new();
    digest.update(before.modified().ok().map(|value| format!("{value:?}")).unwrap_or_default());
    digest.update(serde_json::to_vec(&entries).map_err(|_| unavailable())?);
    let revision = format!("{:x}", digest.finalize());
    if request.revision.as_ref().is_some_and(|expected| expected != &revision) {
        return Err(malformed("This directory changed. Refresh it before reading another page."));
    }
    if request.offset_entries > entries.len() { return Err(malformed("The requested directory position is invalid.")); }
    let total_entries = entries.len();
    let end = request.offset_entries.saturating_add(request.page_entries).min(total_entries);
    let page = entries.drain(request.offset_entries..end).collect();
    Ok(DirectoryReadPage {
        project_id: request.project_id, path: request.path.clone(), entries: page,
        offset_entries: request.offset_entries,
        next_offset_entries: (end < total_entries).then_some(end), total_entries, revision,
    })
}
