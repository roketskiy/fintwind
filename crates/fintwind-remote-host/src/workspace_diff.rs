//! Bounded, strictly read-only Git diff pages. Failure modes: arbitrary refs
//! or cwd, external diff/textconv/fsmonitor execution, credential paths, Git
//! writes hidden in checkpoint helpers, symlink escape, unbounded output or
//! process lifetime, and mixing pages from different revisions.
//!
//! The desktop's CollectReviewDiff captures synthetic commits. This remote
//! read must not call it: it uses Git's existing pure diff commands instead.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fintwind_remote_protocol::RejectCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::workspace_directory::open_directory;
use crate::workspace_reads::{MAX_FILE_BYTES, MAX_PAGE_BYTES, ReadError, opened_path, read_bytes_snapshot, relative_path};

const MAX_CHANGED_PATHS: usize = 128;
const MAX_TRACKED_PATHS: usize = 4096;
const MAX_TEXT_LINES: usize = 50_000;
const MAX_DIFF_INPUT_BYTES: usize = 32 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffReadSource { Unstaged, Staged, Uncommitted }

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiffReadRequest {
    pub project_id: uuid::Uuid,
    pub source: DiffReadSource,
    #[serde(default)]
    pub offset_bytes: usize,
    #[serde(default = "default_page_bytes")]
    pub page_bytes: usize,
    pub revision: Option<String>,
}

fn default_page_bytes() -> usize { MAX_PAGE_BYTES }

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReadPage {
    pub project_id: uuid::Uuid,
    pub source: DiffReadSource,
    pub content: String,
    pub offset_bytes: usize,
    pub next_offset_bytes: Option<usize>,
    pub total_bytes: usize,
    pub revision: String,
}

fn invalid(message: &'static str) -> ReadError { (RejectCode::Malformed, message.to_owned()) }
fn unavailable() -> ReadError { (RejectCode::UnknownRoute, "Changes are unavailable in the selected project.".to_owned()) }

struct GitProcess(Child);
impl Drop for GitProcess {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

fn git_output(root: &Path, arguments: &[String], limit: usize, deadline: Instant) -> Result<Vec<u8>, ReadError> {
    git_output_with_input(root, arguments, None, limit, deadline)
}

fn git_output_with_input(root: &Path, arguments: &[String], input: Option<Vec<u8>>, limit: usize, deadline: Instant) -> Result<Vec<u8>, ReadError> {
    if Instant::now() >= deadline { return Err(invalid("Loading these changes timed out. Try again.")); }
    let search = std::env::var_os("PATH").ok_or_else(unavailable)?;
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    let executable = std::env::split_paths(&search).filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name)).find(|candidate| candidate.is_file()).ok_or_else(unavailable)?;
    // Never search the requested worktree for an executable named git.
    let mut command = Command::new(executable);
    command.current_dir(root)
        .args(["--no-pager", "--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "gc.auto=0", "-c", "maintenance.auto=false", "-c", "protocol.allow=never"])
        .args(arguments).env("GIT_OPTIONAL_LOCKS", "0").env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1").env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::null());
    // User inherited routing variables must not redirect a project-scoped read
    // to a different repository/index or change its pathspec interpretation.
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_GLOB_PATHSPECS", "GIT_NOGLOB_PATHSPECS", "GIT_ICASE_PATHSPECS", "GIT_EXTERNAL_DIFF"] {
        command.env_remove(name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut process = GitProcess(command.spawn().map_err(|_| unavailable())?);
    let writer = if let Some(input) = input {
        let mut stdin = process.0.stdin.take().ok_or_else(unavailable)?;
        Some(std::thread::spawn(move || stdin.write_all(&input)))
    } else { None };
    let stdout = process.0.stdout.take().ok_or_else(unavailable)?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(limit as u64 + 1).read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    let mut bytes = None;
    let status = loop {
        match receiver.try_recv() {
            Ok(Ok(value)) if value.len() > limit => {
                let _ = process.0.kill(); let _ = process.0.wait(); let _ = reader.join();
                return Err(invalid("These changes are too large to view remotely."));
            }
            Ok(Ok(value)) => bytes = Some(value),
            Ok(Err(_)) => return Err(unavailable()),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) if bytes.is_none() => return Err(unavailable()),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
        if let Some(status) = process.0.try_wait().map_err(|_| unavailable())? { break status; }
        if Instant::now() >= deadline {
            let _ = process.0.kill(); let _ = process.0.wait();
            // The reader is bounded; it can outlive a malicious descendant
            // holding stdout without blocking this request's deadline.
            return Err(invalid("Loading these changes timed out. Try again."));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() { return Err(unavailable()); }
    let bytes = match bytes {
        Some(value) => value,
        None => receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| unavailable())?.map_err(|_| unavailable())?,
    };
    let _ = reader.join();
    if let Some(writer) = writer {
        writer.join().map_err(|_| unavailable())?.map_err(|_| unavailable())?;
    }
    if bytes.len() > limit { return Err(invalid("These changes are too large to view remotely.")); }
    Ok(bytes)
}

fn object_map(root: &Path, index: bool, deadline: Instant) -> Result<HashMap<String, String>, ReadError> {
    let arguments = if index {
        ["ls-files", "--stage", "-z"].into_iter().map(str::to_owned).collect::<Vec<_>>()
    } else {
        ["ls-tree", "-r", "-z", "--full-tree", "HEAD"].into_iter().map(str::to_owned).collect::<Vec<_>>()
    };
    let output = git_output(root, &arguments, 1024 * 1024, deadline)?;
    let mut result = HashMap::new();
    for entry in output.split(|byte| *byte == 0).filter(|entry| !entry.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|_| unavailable())?;
        let (metadata, path) = entry.split_once('\t').ok_or_else(unavailable)?;
        let fields = metadata.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 { return Err(unavailable()); }
        if index && fields[2] != "0" { return Err(invalid("Resolve conflicting files on this computer before viewing changes.")); }
        if !matches!(fields[0], "100644" | "100755") { continue; }
        let object = fields[if index { 1 } else { 2 }];
        if !matches!(object.len(), 40 | 64) || !object.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err(unavailable()); }
        if result.len() >= MAX_TRACKED_PATHS { return Err(invalid("This project has too many tracked files to view changes remotely.")); }
        if relative_path(path).is_ok() { result.insert(path.to_owned(), object.to_owned()); }
    }
    Ok(result)
}

fn blob_snapshots(root: &Path, mut objects: Vec<String>, deadline: Instant) -> Result<HashMap<String, Vec<u8>>, ReadError> {
    objects.sort(); objects.dedup();
    if objects.is_empty() { return Ok(HashMap::new()); }
    let input = (objects.join("\n") + "\n").into_bytes();
    let metadata = git_output_with_input(root, &["cat-file".to_owned(), "--batch-check".to_owned()], Some(input.clone()), 1024 * 1024, deadline)?;
    let metadata = std::str::from_utf8(&metadata).map_err(|_| unavailable())?;
    let mut total_bytes = 0_usize;
    let mut sizes = Vec::new();
    for (line, object) in metadata.lines().zip(&objects) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[0] != object || fields[1] != "blob" { return Err(unavailable()); }
        let size: usize = fields[2].parse().map_err(|_| unavailable())?;
        if size > MAX_FILE_BYTES { return Err(invalid("These files are too large to view changes remotely.")); }
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > MAX_DIFF_INPUT_BYTES { return Err(invalid("This project's files exceed the remote changes reading budget.")); }
        sizes.push(size);
    }
    if sizes.len() != objects.len() || metadata.lines().count() != objects.len() { return Err(unavailable()); }
    let output = git_output_with_input(root, &["cat-file".to_owned(), "--batch".to_owned()], Some(input), total_bytes.saturating_add(objects.len() * 128), deadline)?;
    let mut position = 0_usize;
    let mut snapshots = HashMap::new();
    for (object, size) in objects.into_iter().zip(sizes) {
        let header_end = output.get(position..).and_then(|rest| rest.iter().position(|byte| *byte == b'\n')).map(|offset| position + offset).ok_or_else(unavailable)?;
        let expected = format!("{object} blob {size}");
        if output.get(position..header_end) != Some(expected.as_bytes()) { return Err(unavailable()); }
        let start = header_end + 1;
        let end = start.checked_add(size).ok_or_else(unavailable)?;
        if output.get(end) != Some(&b'\n') { return Err(unavailable()); }
        snapshots.insert(object, output.get(start..end).ok_or_else(unavailable)?.to_vec());
        position = end + 1;
    }
    if position != output.len() { return Err(unavailable()); }
    Ok(snapshots)
}

fn blob_bytes<'a>(objects: &HashMap<String, String>, snapshots: &'a HashMap<String, Vec<u8>>, path: &str) -> Result<&'a [u8], ReadError> {
    let Some(object) = objects.get(path) else { return Ok(&[]); };
    snapshots.get(object).map(Vec::as_slice).ok_or_else(unavailable)
}

fn diff_text(bytes: &[u8]) -> Result<&str, ReadError> {
    if bytes.contains(&0) { return Err(invalid("Binary changes are not available in the text viewer.")); }
    std::str::from_utf8(bytes).map_err(|_| invalid("These changes are not UTF-8 text."))
}

fn append_bounded(target: &mut String, text: &str) -> Result<(), ReadError> {
    if target.len().saturating_add(text.len()) > MAX_FILE_BYTES { return Err(invalid("These changes are too large to view remotely.")); }
    target.push_str(text);
    Ok(())
}

/// One correct, conservative hunk over two captured strings. Common prefix
/// and suffix are omitted except for three context lines; separated edits may
/// share a hunk. This linear pass cannot explode like an unrestricted diff
/// algorithm, and it never re-opens a worktree pathname to obtain its bytes.
fn append_patch(output: &mut String, path: &str, before: &str, after: &str) -> Result<(), ReadError> {
    if before == after { return Ok(()); }
    if [before, after].iter().any(|text| text.bytes().filter(|byte| *byte == b'\n').count() > MAX_TEXT_LINES) {
        return Err(invalid("These files have too many lines to view changes remotely."));
    }
    let old = before.split_inclusive('\n').collect::<Vec<_>>();
    let new = after.split_inclusive('\n').collect::<Vec<_>>();
    let mut prefix = 0;
    while prefix < old.len().min(new.len()) && old[prefix] == new[prefix] { prefix += 1; }
    let mut suffix = 0;
    while suffix < old.len().saturating_sub(prefix).min(new.len().saturating_sub(prefix))
        && old[old.len() - suffix - 1] == new[new.len() - suffix - 1] { suffix += 1; }
    let context_start = prefix.saturating_sub(3);
    let old_end = old.len().saturating_sub(suffix).saturating_add(3.min(suffix));
    let new_end = new.len().saturating_sub(suffix).saturating_add(3.min(suffix));
    // JSON quoting keeps unusual local names from forging patch headers.
    let old_name = serde_json::to_string(&format!("a/{path}")).map_err(|_| unavailable())?;
    let new_name = serde_json::to_string(&format!("b/{path}")).map_err(|_| unavailable())?;
    let old_start = if old.is_empty() { 0 } else { context_start + 1 };
    let new_start = if new.is_empty() { 0 } else { context_start + 1 };
    append_bounded(output, &format!("diff --git {old_name} {new_name}\n--- {old_name}\n+++ {new_name}\n@@ -{old_start},{} +{new_start},{} @@\n", old_end - context_start, new_end - context_start))?;
    let mut line = |prefix: char, value: &str| -> Result<(), ReadError> {
        append_bounded(output, &prefix.to_string())?;
        append_bounded(output, value)?;
        if !value.ends_with('\n') { append_bounded(output, "\n\\ No newline at end of file\n")?; }
        Ok(())
    };
    for value in &old[context_start..prefix] { line(' ', value)?; }
    for value in &old[prefix..old.len() - suffix] { line('-', value)?; }
    for value in &new[prefix..new.len() - suffix] { line('+', value)?; }
    for value in &old[old.len() - suffix..old_end] { line(' ', value)?; }
    Ok(())
}

/// A whole tracked subtree can be deleted. Verify its nearest remaining
/// directory instead of requiring an immediate parent that no longer exists.
/// Existing escaping aliases are still rejected, never climbed past.
fn deleted_path_is_scoped(root: &Path, path: &Path) -> bool {
    let mut ancestor = path.parent();
    while let Some(directory) = ancestor {
        if !directory.starts_with(root) { return false; }
        match directory.canonicalize() {
            Ok(resolved) => {
                if !resolved.starts_with(root) { return false; }
                if resolved != root {
                    let Some(relative) = resolved.strip_prefix(root).ok().and_then(Path::to_str) else { return false; };
                    if relative_path(&relative.replace('\\', "/")).is_err() { return false; }
                }
                let Ok(handle) = open_directory(&resolved) else { return false; };
                return opened_path(&handle).ok().as_ref() == Some(&resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ancestor = directory.parent(),
            Err(_) => return false,
        }
    }
    false
}

pub fn read_diff(root: &Path, request: &DiffReadRequest) -> Result<DiffReadPage, ReadError> {
    let deadline = Instant::now() + GIT_TIMEOUT;
    if request.page_bytes == 0 || request.page_bytes > MAX_PAGE_BYTES {
        return Err(invalid("The requested changes page is too large."));
    }
    if request.offset_bytes > 0 && request.revision.is_none() {
        return Err(invalid("Refresh the changes before reading another page."));
    }
    let root = root.canonicalize().map_err(|_| unavailable())?;
    let directory = open_directory(&root)?;
    if opened_path(&directory)? != root { return Err(unavailable()); }
    // Never let `git diff` silently climb to a parent repository and disclose
    // its unrelated files. A project subdirectory fails closed for now.
    let top = git_output(&root, &["rev-parse".to_owned(), "--show-toplevel".to_owned()], 32 * 1024, deadline)?;
    let top = std::str::from_utf8(&top).map_err(|_| unavailable())?.trim();
    if Path::new(top).canonicalize().ok().as_ref() != Some(&root) { return Err(unavailable()); }
    // Git diff can invoke a configured clean filter even in a name-only mode.
    // Read only index/tree plumbing, then compare captured bytes ourselves.
    let index = object_map(&root, true, deadline)?;
    let head = if matches!(request.source, DiffReadSource::Unstaged) { HashMap::new() } else { object_map(&root, false, deadline)? };
    let mut names = index.keys().chain(head.keys()).cloned().collect::<Vec<_>>();
    names.sort(); names.dedup();
    if names.len() > MAX_TRACKED_PATHS { return Err(invalid("This project has too many tracked files to view changes remotely.")); }
    let mut paths = Vec::new();
    for name in names {
        let Ok(relative) = relative_path(&name) else { continue; };
        let path = root.join(relative);
        match path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => continue,
            Ok(_) => {
                let actual = path.canonicalize().map_err(|_| unavailable())?;
                if !actual.starts_with(&root) { continue; }
                let relative = actual.strip_prefix(&root).ok().and_then(Path::to_str).ok_or_else(unavailable)?;
                if relative_path(&relative.replace('\\', "/")).is_err() { continue; }
            }
            // A tracked deletion is a valid diff, but its parents must still
            // be the project, not an alias to somewhere else.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !deleted_path_is_scoped(&root, &path) { continue; }
            }
            Err(_) => continue,
        }
        if matches!(request.source, DiffReadSource::Staged) && index.get(&name) == head.get(&name) { continue; }
        paths.push(name);
    }
    let mut content = String::new();
    if !paths.is_empty() {
        let baseline = if matches!(request.source, DiffReadSource::Unstaged) { &index } else { &head };
        let objects = paths.iter().filter_map(|path| baseline.get(path)).chain(
            paths.iter().filter(|_| matches!(request.source, DiffReadSource::Staged)).filter_map(|path| index.get(path))
        ).cloned().collect();
        let snapshots = blob_snapshots(&root, objects, deadline)?;
        let mut changed_paths = 0;
        let mut scanned_bytes = 0_usize;
        for path in &paths {
            let before = blob_bytes(baseline, &snapshots, path)?;
            let worktree;
            let after = if matches!(request.source, DiffReadSource::Staged) {
                blob_bytes(&index, &snapshots, path)?
            } else if root.join(path).try_exists().map_err(|_| unavailable())? {
                worktree = read_bytes_snapshot(&root, path)?;
                worktree.as_slice()
            } else { &[] };
            scanned_bytes = scanned_bytes.saturating_add(before.len()).saturating_add(after.len());
            if scanned_bytes > MAX_DIFF_INPUT_BYTES { return Err(invalid("This project's files exceed the remote changes reading budget.")); }
            if Instant::now() >= deadline { return Err(invalid("Loading these changes timed out. Try again.")); }
            if before != after {
                changed_paths += 1;
                if changed_paths > MAX_CHANGED_PATHS { return Err(invalid("There are too many changed files to view remotely.")); }
                append_patch(&mut content, path, diff_text(before)?, diff_text(after)?)?;
            }
        }
    }
    if opened_path(&directory)? != root || root.canonicalize().ok().as_ref() != Some(&root) { return Err(unavailable()); }
    let revision = format!("{:x}", Sha256::digest(content.as_bytes()));
    if request.revision.as_ref().is_some_and(|expected| expected != &revision) {
        return Err(invalid("The changes were updated. Refresh them before reading another page."));
    }
    if request.offset_bytes > content.len() || !content.is_char_boundary(request.offset_bytes) {
        return Err(invalid("The requested changes position is invalid."));
    }
    let mut end = request.offset_bytes.saturating_add(request.page_bytes).min(content.len());
    while end > request.offset_bytes && !content.is_char_boundary(end) { end -= 1; }
    if end == request.offset_bytes && end < content.len() { return Err(invalid("The requested page is too small to display this text.")); }
    Ok(DiffReadPage {
        project_id: request.project_id, source: request.source,
        content: content[request.offset_bytes..end].to_owned(), offset_bytes: request.offset_bytes,
        next_offset_bytes: (end < content.len()).then_some(end), total_bytes: content.len(), revision,
    })
}
