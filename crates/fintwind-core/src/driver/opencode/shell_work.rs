//! Background shell commands the OpenCode agent detached from its turn.
//!
//! The `shell` tool runs every command through the location's shell service:
//! combined output streams to a file the client pages by cursor, and a
//! command the agent backgrounds (`background: true`, or a foreground one the
//! user moves off) returns immediately with a `shellID`. The capsule's
//! background section is that registry projected onto
//! [`BackgroundWorkKind::Process`], so the driver tracks each shell's output
//! cursor here and the app never re-reads output it already has.
//!
//! A foreground shell and a detached one are indistinguishable on the wire —
//! the shell service lists and streams both the same way, and its lifecycle
//! events name no session. Detachment is therefore only ever *confirmed*,
//! never inferred: a tool result that carries a `shellID`, or a stored
//! transcript part whose result did. Everything else this module sees is
//! somebody else's foreground command, and the registry's own foreground
//! rules keep those in the transcript where they belong.
//!
//! The shells themselves are the server's: Fintwind only observes, pages,
//! and stops them through `GET /api/shell`, `GET /api/shell/:id`,
//! `GET /api/shell/:id/output`, and `DELETE /api/shell/:id`.
//!
//! Two lifecycle rules complete the picture. A shell named only by history
//! (the app restarted under a shared server) becomes visible only while the
//! server still runs it, so a command that finished before this driver
//! attached never appears at all. And a shell that ends leaves the capsule
//! the moment it settles: the transcript card and the completion
//! notification are its record, and the capsule's background section lists
//! live work rather than a command history.

use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use parking_lot::Mutex;
use serde_json::Value;

use crate::driver::{DriverEvent, DriverEventSink};
use crate::model::{
    BackgroundWorkEvent, BackgroundWorkItem, BackgroundWorkKey, BackgroundWorkKind,
    BackgroundWorkStatus, unix_time_millis,
};
use crate::opencode_session::{encode_path_segment, request_json_on_port_with_directory};

/// How much one output page pulls. The registry bounds the stored tail
/// itself; a generous page keeps a noisy build from falling behind the
/// refresh cadence.
const OUTPUT_PAGE_BYTES: u64 = 256 * 1024;

/// Every shell read runs off the UI thread on the shared refresh or the
/// event thread; a hung server must not hold either for long.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One background shell the driver has observed, with how far its output has
/// been paged. Presence in the map means the shell is this session's work;
/// the cursor is what makes output paging incremental.
#[derive(Clone)]
pub(super) struct TrackedShell {
    pub(super) item: BackgroundWorkItem,
    pub(super) cursor: u64,
    /// Sticky: this shell left its turn. Only a tool result that named the
    /// shell (or a stored transcript part that did) can set it, and a row
    /// rebuilt from the server never clears it. Foreground shells are not
    /// tracked at all, so an unset flag means "not our background work".
    detached: bool,
    /// An output read is in flight. The claim keeps a slower refresh round
    /// from paging, and re-emitting, the same cursor.
    reading: bool,
}

impl TrackedShell {
    fn new(item: BackgroundWorkItem, detached: bool) -> Self {
        Self {
            item,
            cursor: 0,
            detached,
            reading: false,
        }
    }

    /// Projects the item with the sticky flag applied. Every emit and every
    /// reconcile list goes through here, so the registry's notion of
    /// "background" can never drift from the tracker's.
    fn projected(&self) -> BackgroundWorkItem {
        let mut item = self.item.clone();
        item.background = self.detached;
        item
    }
}

/// The shells this session's driver tracks, shared between the event thread
/// (which learns about them from the wire) and the background refresh (which
/// reconciles their status and pages their output).
#[derive(Default)]
pub(super) struct ShellWorkTracker {
    shells: Mutex<HashMap<String, TrackedShell>>,
}

impl ShellWorkTracker {
    /// Starts tracking a shell. An already-tracked shell keeps its cursor —
    /// a rediscovered shell is the same process, and restarting the cursor
    /// would duplicate its output.
    pub(super) fn track(&self, item: BackgroundWorkItem) -> u64 {
        let id = item.key.provider_id.clone();
        let mut shells = self.shells.lock();
        if let Some(tracked) = shells.get_mut(&id) {
            tracked.item = item;
            tracked.detached = true;
            tracked.cursor
        } else {
            shells.insert(id, TrackedShell::new(item, true));
            0
        }
    }

    /// Replaces a tracked shell's row-sourced item, keeping its cursor and
    /// its detached flag. An untracked shell is not adopted here: only a
    /// tool result or a stored transcript part makes a shell this session's
    /// background work, so a foreground shell the server happens to list is
    /// never pulled into the capsule.
    pub(super) fn refresh_item(&self, item: BackgroundWorkItem) -> bool {
        let id = item.key.provider_id.clone();
        let mut shells = self.shells.lock();
        match shells.get_mut(&id) {
            Some(tracked) => {
                tracked.item = item;
                true
            }
            None => false,
        }
    }

    /// Claims the right to page a tracked shell's output. `None` when
    /// another reader already holds the claim, so one page is never read
    /// and emitted twice by two overlapping refresh rounds.
    pub(super) fn begin_output_read(&self, id: &str) -> Option<u64> {
        let mut shells = self.shells.lock();
        let tracked = shells.get_mut(id)?;
        if tracked.reading {
            return None;
        }
        tracked.reading = true;
        Some(tracked.cursor)
    }

    /// Releases the claim and advances the cursor. Passing the cursor the
    /// read started from releases without moving, so a failed page is
    /// retried on the next round instead of skipped.
    pub(super) fn end_output_read(&self, id: &str, cursor: u64) {
        if let Some(tracked) = self.shells.lock().get_mut(id) {
            tracked.reading = false;
            tracked.cursor = tracked.cursor.max(cursor);
        }
    }

    pub(super) fn untrack(&self, id: &str) {
        self.shells.lock().remove(id);
    }

    pub(super) fn ids(&self) -> Vec<String> {
        self.shells.lock().keys().cloned().collect()
    }

    pub(super) fn is_tracked(&self, id: &str) -> bool {
        self.shells.lock().contains_key(id)
    }

    /// Every tracked shell, projected for the registry with its sticky
    /// background flag applied.
    pub(super) fn items(&self) -> Vec<BackgroundWorkItem> {
        self.shells
            .lock()
            .values()
            .map(TrackedShell::projected)
            .collect()
    }

    pub(super) fn get(&self, id: &str) -> Option<TrackedShell> {
        self.shells.lock().get(id).cloned()
    }

    /// A tracked shell's item with a new status, still carrying its sticky
    /// flag and everything the tracker knows. `None` when untracked.
    pub(super) fn settled_item(
        &self,
        id: &str,
        status: BackgroundWorkStatus,
        exit_code: Option<i32>,
    ) -> Option<BackgroundWorkItem> {
        let mut shells = self.shells.lock();
        let tracked = shells.get_mut(id)?;
        tracked.item.status = status;
        if let Some(exit_code) = exit_code {
            tracked.item.exit_code = Some(exit_code);
        }
        Some(tracked.projected())
    }
}

/// One page of a shell's captured output.
struct ShellOutputPage {
    text: String,
    cursor: u64,
    size: u64,
}

/// Whether a shell row belongs to `session_id`. The service stamps the
/// creating session on every shell; another client's shells share the
/// server but never belong to this conversation.
pub(super) fn shell_belongs_to(info: &Value, session_id: &str) -> bool {
    info.pointer("/metadata/sessionID").and_then(Value::as_str) == Some(session_id)
}

/// The shell id a tool result detached its command into, if it did.
/// Foreground results carry no `shellID` — only the background result does.
pub(super) fn tool_shell_id(metadata: Option<&Value>) -> Option<&str> {
    metadata
        .and_then(|metadata| metadata.get("shellID"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// Maps the service's shell status onto the registry's work status.
pub(super) fn work_status_from_shell(status: &str, exit: Option<i64>) -> BackgroundWorkStatus {
    match status {
        "running" => BackgroundWorkStatus::Running,
        "timeout" => BackgroundWorkStatus::Failed,
        "killed" => BackgroundWorkStatus::Stopped,
        _ => match exit {
            Some(0) | None => BackgroundWorkStatus::Completed,
            Some(_) => BackgroundWorkStatus::Failed,
        },
    }
}

/// Projects one `Shell.Info` row onto a background-work item. `None` for a
/// row that names no shell id.
///
/// The row alone never marks the item background: a foreground shell and a
/// detached one look identical on the wire, and only the tracker knows which
/// shells this session actually backgrounded. The flag is applied when the
/// item is projected for the registry.
pub(super) fn shell_work_item(info: &Value) -> Option<BackgroundWorkItem> {
    let id = info.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let command = info
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut item = BackgroundWorkItem::new(
        BackgroundWorkKind::Process,
        id,
        command.clone(),
        work_status_from_shell(
            info.get("status")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            info.get("exit").and_then(Value::as_i64),
        ),
    );
    item.command = Some(command);
    item.cwd = info.get("cwd").and_then(Value::as_str).map(str::to_owned);
    item.exit_code = info
        .get("exit")
        .and_then(Value::as_i64)
        .map(|code| code as i32);
    item.control_id = Some(id.to_owned());
    if let Some(started) = info.pointer("/time/started").and_then(Value::as_u64) {
        item.started_at_ms = started;
    }
    if let Some(completed) = info.pointer("/time/completed").and_then(Value::as_u64) {
        item.updated_at_ms = completed;
    }
    item.can_stop = item.status.is_stoppable();
    Some(item)
}

/// The `shell.exited` event carries only `{id, exit?, status}` — enough to
/// settle an item the tracker already holds.
pub(super) fn shell_exit_work_status(payload: &Value) -> Option<(String, BackgroundWorkStatus)> {
    let id = payload.get("id").and_then(Value::as_str)?.trim().to_owned();
    let status = work_status_from_shell(
        payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("exited"),
        payload.get("exit").and_then(Value::as_i64),
    );
    Some((id, status))
}

fn shell_path(id: &str, suffix: &str) -> String {
    format!("/api/shell/{}{}", encode_path_segment(id), suffix)
}

/// Pages one shell's captured output forward from `cursor`.
fn poll_shell_output(port: u16, directory: &str, id: &str, cursor: u64) -> Option<ShellOutputPage> {
    let path = format!(
        "{}?cursor={}&limit={}&directory={}",
        shell_path(id, "/output"),
        cursor,
        OUTPUT_PAGE_BYTES,
        encode_path_segment(directory)
    );
    let response = request_json_on_port_with_directory(
        port,
        "GET",
        &path,
        None,
        REQUEST_TIMEOUT,
        Some(directory),
    )
    .ok()?;
    let data = response.get("data")?;
    Some(ShellOutputPage {
        text: data.get("output").and_then(Value::as_str)?.to_owned(),
        cursor: data.get("cursor").and_then(Value::as_u64).unwrap_or(cursor),
        size: data.get("size").and_then(Value::as_u64).unwrap_or(cursor),
    })
}

/// Reads one shell's current row. `Ok(None)` means the server no longer
/// knows the shell (it exited past the retention window, or was removed);
/// `Err` means the read itself failed or answered nothing usable, which
/// proves nothing and must not retire a live shell.
pub(super) fn fetch_shell_info(
    port: u16,
    directory: &str,
    id: &str,
) -> anyhow::Result<Option<Value>> {
    let path = format!(
        "{}?directory={}",
        shell_path(id, ""),
        encode_path_segment(directory)
    );
    match request_json_on_port_with_directory(
        port,
        "GET",
        &path,
        None,
        REQUEST_TIMEOUT,
        Some(directory),
    ) {
        Ok(response) => match response.get("data") {
            Some(data) if !data.is_null() => Ok(Some(data.clone())),
            _ => Err(anyhow!("OpenCode returned no shell row for `{id}`")),
        },
        Err(error) if error.to_string().contains("HTTP 404") => Ok(None),
        Err(error) => Err(error),
    }
}

/// Pages a tracked shell's new output and emits it as one delta. Returns
/// the page's end cursor and the shell's captured size; `None` when the
/// read itself failed, which leaves the cursor where it was.
pub(super) fn emit_shell_output(
    port: u16,
    directory: &str,
    id: &str,
    cursor: u64,
    events: &impl DriverEventSink,
) -> Option<(u64, u64)> {
    let page = poll_shell_output(port, directory, id, cursor)?;
    if !page.text.is_empty() {
        let _ = events.send(DriverEvent::BackgroundWork(
            BackgroundWorkEvent::OutputDelta {
                key: BackgroundWorkKey::new(BackgroundWorkKind::Process, id),
                delta: page.text,
            },
        ));
    }
    Some((page.cursor, page.size))
}

/// Publishes a settled item's final status and stops tracking it.
pub(super) fn settle_tracked_shell(
    tracker: &ShellWorkTracker,
    id: &str,
    status: BackgroundWorkStatus,
    exit_code: Option<i32>,
    events: &impl DriverEventSink,
) {
    let Some(mut item) = tracker.settled_item(id, status, exit_code) else {
        return;
    };
    item.can_stop = false;
    item.updated_at_ms = unix_time_millis();
    tracker.untrack(id);
    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
        item,
    )));
}

/// `shell.exited`: the event carries the terminal row, so the capsule does
/// not wait for the next refresh to settle. The output tail is paged by the
/// refresh, which keeps the shell tracked until it is caught up. Only a
/// tracked shell is settled: the event names no session, so an untracked id
/// is some other client's foreground command.
pub(super) fn observe_exited_shell(
    tracker: &ShellWorkTracker,
    payload: &Value,
    events: &impl DriverEventSink,
) {
    let Some((id, status)) = shell_exit_work_status(payload) else {
        return;
    };
    let exit_code = payload
        .get("exit")
        .and_then(Value::as_i64)
        .map(|code| code as i32);
    let Some(mut item) = tracker.settled_item(&id, status, exit_code) else {
        return;
    };
    item.can_stop = false;
    item.updated_at_ms = unix_time_millis();
    let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
        item,
    )));
}

/// `shell.deleted`: the server removed the shell and its output file. A
/// shell still live becomes Lost; one already settled keeps its recorded
/// outcome and simply stops being tracked.
pub(super) fn observe_deleted_shell(
    tracker: &ShellWorkTracker,
    id: &str,
    events: &impl DriverEventSink,
) {
    let Some(tracked) = tracker.get(id) else {
        return;
    };
    if tracked.item.status.is_live() {
        settle_tracked_shell(tracker, id, BackgroundWorkStatus::Lost, None, events);
    } else {
        tracker.untrack(id);
    }
}

/// Reconciles this session's background shells against the server and
/// returns the items for the caller's reconcile.
///
/// Only tracked shells are reconciled, plus any `restore_ids` the caller
/// recovered from the stored transcript: a shell becomes this session's
/// background work when a tool result names it, or when the session's
/// history shows it was backgrounded before this driver attached and the
/// server still runs it. A shell is never adopted from a listing alone —
/// a foreground shell is indistinguishable from a detached one there.
///
/// A tracked shell the listing no longer runs has terminated or been
/// removed: its own row settles the status after one final output page —
/// the server reports a terminal row only once the output file is closed —
/// and the tracker then stops watching it. The registry drops the settled
/// row: a detached command that ended belongs to the transcript.
///
/// A failed listing proves nothing, so the tracked shells then ride the
/// reconcile exactly as they are: a flaky read cannot retire live work.
/// `stale` reports that a newer refresh superseded this one mid-walk, which
/// stops paging further pages.
pub(super) fn reconcile_shells(
    port: u16,
    directory: &str,
    parent_id: &str,
    tracker: &ShellWorkTracker,
    restore_ids: &HashSet<String>,
    stale: impl Fn() -> bool,
    events: &impl DriverEventSink,
) -> Vec<BackgroundWorkItem> {
    let projected = || tracker.items();
    let listing = request_json_on_port_with_directory(
        port,
        "GET",
        &format!("/api/shell?directory={}", encode_path_segment(directory)),
        None,
        REQUEST_TIMEOUT,
        Some(directory),
    );
    let running = match listing {
        Ok(response) => response
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Err(_) => return projected(),
    };
    let live = running
        .iter()
        .filter(|info| shell_belongs_to(info, parent_id))
        .filter_map(|info| info.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect::<HashSet<_>>();
    for info in running
        .iter()
        .filter(|info| shell_belongs_to(info, parent_id))
    {
        if stale() {
            return projected();
        }
        let Some(id) = info.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(item) = shell_work_item(info) else {
            continue;
        };
        if tracker.is_tracked(id) {
            if !tracker.refresh_item(item) {
                continue;
            }
        } else if restore_ids.contains(id) {
            // A tool result (or history) confirmed this shell, and the
            // server still runs it: it is this session's background work.
            tracker.track(item);
        } else {
            // A foreground shell of this session, or another client's: it
            // stays out of the capsule.
            continue;
        }
        page_tracked_output(port, directory, id, tracker, events);
    }
    for id in tracker.ids() {
        if live.contains(&id) {
            continue;
        }
        if stale() {
            return projected();
        }
        match fetch_shell_info(port, directory, &id) {
            Ok(Some(info)) => {
                let Some(item) = shell_work_item(&info) else {
                    continue;
                };
                let settled = item.status;
                let exit_code = item.exit_code;
                if !tracker.refresh_item(item) {
                    continue;
                }
                // The server reports a terminal row only after the output
                // file is closed, so one final page lands the last of the
                // output before the row leaves the capsule.
                page_tracked_output(port, directory, &id, tracker, events);
                if !settled.is_live() {
                    settle_tracked_shell(tracker, &id, settled, exit_code, events);
                }
            }
            // Unknown to the server: a removed or evicted shell. An already
            // settled one keeps the outcome it reported; a live one is Lost.
            Ok(None) => {
                let live_item = tracker
                    .get(&id)
                    .is_some_and(|shell| shell.item.status.is_live());
                if live_item {
                    settle_tracked_shell(tracker, &id, BackgroundWorkStatus::Lost, None, events);
                } else {
                    tracker.untrack(&id);
                }
            }
            Err(_) => {}
        }
    }
    projected()
}

/// Pages one tracked shell's new output, claiming the read so two
/// overlapping refresh rounds never emit the same page.
fn page_tracked_output(
    port: u16,
    directory: &str,
    id: &str,
    tracker: &ShellWorkTracker,
    events: &impl DriverEventSink,
) {
    let Some(cursor) = tracker.begin_output_read(id) else {
        // Another round holds the claim; it will page this cursor.
        return;
    };
    match emit_shell_output(port, directory, id, cursor, events) {
        Some((cursor, _)) => tracker.end_output_read(id, cursor),
        // The read failed: releasing with the cursor it started from retries
        // this page next round rather than skipping it.
        None => tracker.end_output_read(id, cursor),
    }
}

/// How many message pages the restore scan may walk. A background shell is
/// normally recent, but a long build in a busy session must not be missed,
/// so the budget matches the recovery walk rather than a single page.
const RESTORE_SCAN_PAGES: usize = 25;

/// How many messages one restore page asks for.
const RESTORE_PAGE_LIMIT: usize = 200;

/// Pages the session's stored messages for tool parts the provider
/// backgrounded, and names those shells.
///
/// The live wire only tells a driver about shells it observes itself, so a
/// shell still running from before this driver attached (the app restarted
/// under a shared server, or the session was resumed) would otherwise sit in
/// history invisible to the capsule. A stored tool part's result metadata
/// names its shell id, which is the same confirmation the live tool result
/// carries.
///
/// The scan is side-effect free: a candidate becomes background work only
/// when the caller's reconcile finds the server still running it, so a
/// command that finished before this driver attached never appears at all.
pub(super) fn restore_backgrounded_shells(port: u16, session_id: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..RESTORE_SCAN_PAGES {
        let mut path = format!(
            "/api/session/{}/message?limit={}",
            encode_path_segment(session_id),
            RESTORE_PAGE_LIMIT
        );
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let Ok(response) = crate::opencode_session::request_json_on_port(
            port,
            "GET",
            &path,
            None,
            REQUEST_TIMEOUT,
        ) else {
            return found;
        };
        let Some(rows) = response.get("data").and_then(Value::as_array) else {
            return found;
        };
        let empty = rows.is_empty();
        for row in rows {
            let Some(parts) = row.get("content").and_then(Value::as_array) else {
                continue;
            };
            for part in parts {
                if part.get("type").and_then(Value::as_str) != Some("tool") {
                    continue;
                }
                if let Some(shell_id) = tool_shell_id(part.pointer("/state/metadata")) {
                    found.push(shell_id.to_owned());
                }
            }
        }
        let next = response.pointer("/cursor/next").and_then(Value::as_str);
        match next {
            Some(next) if !empty && cursor.as_deref() != Some(next) => {
                cursor = Some(next.to_owned());
            }
            _ => return found,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    /// A loopback stand-in for OpenCode's shell routes. Each connection is
    /// answered by the scripted route whose bare path (query stripped)
    /// matches the request line; anything unmatched answers 404.
    fn serve_shell_routes(routes: Vec<(String, u16, String)>) -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes = Arc::new(routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let routes = Arc::clone(&routes);
                std::thread::spawn(move || {
                    let mut buffer = [0_u8; 8192];
                    let read = std::io::Read::read(&mut stream, &mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                    let bare = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .map(|path| path.split('?').next().unwrap_or(path).to_owned())
                        .unwrap_or_default();
                    let (status, body) = routes
                        .iter()
                        .find(|(prefix, _, _)| &bare == prefix)
                        .map(|(_, status, body)| (*status, body.clone()))
                        .unwrap_or((404, String::from("{\"error\":\"no route\"}")));
                    let reason = if status == 200 { "OK" } else { "Not Found" };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
                });
            }
        });
        port
    }

    fn shell_row(id: &str, status: &str, session: &str) -> Value {
        json!({
            "id": id,
            "status": status,
            "command": "cargo check",
            "cwd": "E:/work/fintwind",
            "shell": "pwsh.exe",
            "file": format!("C:/shell/{id}.out"),
            "metadata": {"sessionID": session},
            "time": {"started": 1_788_253_280_000_u64}
        })
    }

    #[test]
    fn running_and_terminal_shell_rows_project_onto_process_items() {
        let running = shell_work_item(&shell_row("sh_live", "running", "ses_1")).unwrap();
        assert_eq!(running.key.kind, BackgroundWorkKind::Process);
        assert_eq!(running.key.provider_id, "sh_live");
        assert_eq!(running.status, BackgroundWorkStatus::Running);
        // A row alone never claims background: a foreground shell and a
        // detached one are identical on the wire, so only the tracker's
        // sticky flag can mark the item background work.
        assert!(!running.background);
        assert!(running.can_stop);
        assert_eq!(running.control_id.as_deref(), Some("sh_live"));
        assert_eq!(running.command.as_deref(), Some("cargo check"));
        assert_eq!(running.cwd.as_deref(), Some("E:/work/fintwind"));
        assert_eq!(running.started_at_ms, 1_788_253_280_000);

        let mut exited = shell_row("sh_done", "exited", "ses_1");
        exited["exit"] = json!(1);
        let failed = shell_work_item(&exited).unwrap();
        assert_eq!(failed.status, BackgroundWorkStatus::Failed);
        assert_eq!(failed.exit_code, Some(1));
        assert!(!failed.can_stop);

        let mut timed_out = shell_row("sh_slow", "timeout", "ses_1");
        timed_out["time"]["completed"] = json!(1_788_253_290_000_u64);
        assert_eq!(
            shell_work_item(&timed_out).unwrap().status,
            BackgroundWorkStatus::Failed
        );
        assert_eq!(
            shell_work_item(&shell_row("sh_killed", "killed", "ses_1"))
                .unwrap()
                .status,
            BackgroundWorkStatus::Stopped
        );
        // A row that names no shell is nothing this module can track.
        assert!(shell_work_item(&json!({"status": "running"})).is_none());
    }

    #[test]
    fn shell_ownership_and_tool_ids_read_the_wire_shape() {
        assert!(shell_belongs_to(
            &shell_row("sh_live", "running", "ses_1"),
            "ses_1"
        ));
        assert!(!shell_belongs_to(
            &shell_row("sh_other", "running", "ses_other"),
            "ses_1"
        ));
        // A foreground result carries no shellID; a background one always does.
        assert_eq!(
            tool_shell_id(Some(&json!({"shellID": "sh_live", "status": "running"}))),
            Some("sh_live")
        );
        assert_eq!(tool_shell_id(Some(&json!({"status": "completed"}))), None);
        assert_eq!(tool_shell_id(Some(&json!({"shellID": "  "}))), None);
        assert_eq!(tool_shell_id(None), None);
    }

    #[test]
    fn a_tracked_shell_keeps_its_cursor_and_its_detached_flag_across_rows() {
        let tracker = ShellWorkTracker::default();
        let mut confirmed = shell_work_item(&shell_row("sh_live", "running", "ses_1")).unwrap();
        confirmed.background = true;
        tracker.track(confirmed.clone());
        assert!(tracker.begin_output_read("sh_live").is_some());
        tracker.end_output_read("sh_live", 4_096);

        // A row rebuilt from the server carries no background flag; the
        // tracker's sticky one must survive it, or the registry would retire
        // the shell the moment it settles.
        assert!(
            tracker
                .refresh_item(shell_work_item(&shell_row("sh_live", "running", "ses_1")).unwrap())
        );
        let projected = tracker.items();
        assert_eq!(projected.len(), 1);
        assert!(projected[0].background);
        assert_eq!(tracker.get("sh_live").unwrap().cursor, 4_096);
        assert_eq!(
            tracker.track(confirmed),
            4_096,
            "re-tracking keeps the cursor"
        );
        // An untracked shell is never adopted by a row: only a tool result or
        // a stored transcript part confirms background work.
        assert!(
            !tracker
                .refresh_item(shell_work_item(&shell_row("sh_new", "running", "ses_1")).unwrap())
        );
        assert!(!tracker.is_tracked("sh_new"));

        tracker.untrack("sh_live");
        assert!(tracker.get("sh_live").is_none());
        assert!(tracker.ids().is_empty());
    }

    #[test]
    fn an_output_read_claim_keeps_two_rounds_from_paging_one_cursor() {
        let tracker = ShellWorkTracker::default();
        let mut confirmed = shell_work_item(&shell_row("sh_live", "running", "ses_1")).unwrap();
        confirmed.background = true;
        tracker.track(confirmed);

        let first = tracker
            .begin_output_read("sh_live")
            .expect("the first reader claims the page");
        assert_eq!(first, 0);
        // A slower refresh round still walking must not claim the same page.
        assert!(tracker.begin_output_read("sh_live").is_none());
        tracker.end_output_read("sh_live", 12);
        assert_eq!(tracker.begin_output_read("sh_live"), Some(12));
        // The cursor only ever moves forward.
        tracker.end_output_read("sh_live", 4);
        assert_eq!(tracker.get("sh_live").unwrap().cursor, 12);
    }

    #[test]
    fn lifecycle_events_settle_and_retire_only_tracked_shells() {
        let (events, event_rx) = crate::driver::test_event_channel();
        let tracker = ShellWorkTracker::default();

        // A shell becomes tracked only by a confirmation (a tool result, or a
        // stored transcript part); `shell.created` is not a discovery path.
        let mut confirmed = shell_work_item(&shell_row("sh_live", "running", "ses_1")).unwrap();
        confirmed.background = true;
        tracker.track(confirmed.clone());
        let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
            confirmed,
        )));
        let _ = event_rx.try_recv().unwrap();

        // The terminal row settles the item immediately; the shell stays
        // tracked so the refresh can page its final output.
        observe_exited_shell(
            &tracker,
            &json!({"id": "sh_live", "exit": 0, "status": "exited"}),
            &events,
        );
        match event_rx.try_recv().unwrap() {
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) => {
                assert_eq!(item.status, BackgroundWorkStatus::Completed);
                assert_eq!(item.exit_code, Some(0));
                assert!(!item.can_stop);
                assert!(
                    item.background,
                    "a settled background shell stays background"
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(tracker.get("sh_live").is_some());

        // A settled shell that the server removes keeps its recorded outcome;
        // only a live one becomes Lost.
        observe_deleted_shell(&tracker, "sh_live", &events);
        assert!(tracker.get("sh_live").is_none());
        assert!(event_rx.try_recv().is_err());

        let mut live = shell_work_item(&shell_row("sh_gone", "running", "ses_1")).unwrap();
        live.background = true;
        tracker.track(live.clone());
        let _ = events.send(DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(
            live,
        )));
        let _ = event_rx.try_recv().unwrap();
        observe_deleted_shell(&tracker, "sh_gone", &events);
        match event_rx.try_recv().unwrap() {
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) => {
                assert_eq!(item.status, BackgroundWorkStatus::Lost);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(tracker.get("sh_gone").is_none());

        // A terminal event for a shell nobody confirmed is another client's
        // foreground command: it must leave this capsule alone.
        observe_exited_shell(
            &tracker,
            &json!({"id": "sh_theirs", "exit": 0, "status": "exited"}),
            &events,
        );
        assert!(event_rx.try_recv().is_err());
        assert!(!tracker.is_tracked("sh_theirs"));
    }

    #[test]
    fn reconcile_pages_tracked_shells_adopts_running_restores_and_ignores_the_rest() {
        let port = serve_shell_routes(vec![
            (
                "/api/shell".to_owned(),
                200,
                json!({"data": [
                    shell_row("sh_mine", "running", "ses_1"),
                    // Confirmed by history, still running: restored.
                    shell_row("sh_restored", "running", "ses_1"),
                    // This session's own foreground shell: identical on the
                    // wire to a detached one, so it must never be adopted.
                    shell_row("sh_foreground", "running", "ses_1"),
                    shell_row("sh_theirs", "running", "ses_other")
                ]})
                .to_string(),
            ),
            (
                "/api/shell/sh_mine/output".to_owned(),
                200,
                json!({"data": {"output": "Checking fintwind v0.1.0\n", "cursor": 27, "size": 27}})
                    .to_string(),
            ),
            (
                "/api/shell/sh_restored/output".to_owned(),
                200,
                json!({"data": {"output": "restored\n", "cursor": 9, "size": 9}}).to_string(),
            ),
        ]);
        let (events, event_rx) = crate::driver::test_event_channel();
        let tracker = ShellWorkTracker::default();
        let mut confirmed = shell_work_item(&shell_row("sh_mine", "running", "ses_1")).unwrap();
        confirmed.background = true;
        tracker.track(confirmed);
        let restores = HashSet::from([String::from("sh_restored")]);

        let items = reconcile_shells(
            port,
            "E:/work/fintwind",
            "ses_1",
            &tracker,
            &restores,
            || false,
            &events,
        );

        let mut items = items;
        items.sort_by(|left, right| left.key.provider_id.cmp(&right.key.provider_id));
        assert_eq!(items.len(), 2, "only the confirmed and restored shells");
        assert_eq!(items[0].key.provider_id, "sh_mine");
        assert!(items[0].background);
        assert_eq!(items[1].key.provider_id, "sh_restored");
        assert!(items[1].background, "a restored shell is detached work");
        assert_eq!(tracker.get("sh_mine").unwrap().cursor, 27);
        assert_eq!(tracker.get("sh_restored").unwrap().cursor, 9);
        assert!(
            !tracker.is_tracked("sh_foreground"),
            "a foreground shell in the listing must not become background work"
        );
        assert!(!tracker.is_tracked("sh_theirs"));
        let deltas = event_rx
            .try_iter()
            .filter_map(|event| match event {
                DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { delta, .. }) => {
                    Some(delta)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(deltas, ["Checking fintwind v0.1.0\n", "restored\n"]);
    }

    /// A shell the history says was backgrounded but the server no longer
    /// runs finished before this driver attached: it must never appear.
    #[test]
    fn a_restored_candidate_that_already_finished_never_appears() {
        let port = serve_shell_routes(vec![(
            "/api/shell".to_owned(),
            200,
            json!({"data": []}).to_string(),
        )]);
        let (events, event_rx) = crate::driver::test_event_channel();
        let tracker = ShellWorkTracker::default();
        let restores = HashSet::from([String::from("sh_finished")]);

        let items = reconcile_shells(
            port,
            "E:/work/fintwind",
            "ses_1",
            &tracker,
            &restores,
            || false,
            &events,
        );

        assert!(items.is_empty());
        assert!(!tracker.is_tracked("sh_finished"));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn reconcile_settles_a_tracked_shell_the_listing_dropped() {
        let port = serve_shell_routes(vec![
            (
                "/api/shell".to_owned(),
                200,
                json!({"data": []}).to_string(),
            ),
            ("/api/shell/sh_done".to_owned(), 200, {
                let mut row = shell_row("sh_done", "exited", "ses_1");
                row["exit"] = json!(0);
                row["time"]["completed"] = json!(1_788_253_290_000_u64);
                json!({"data": row}).to_string()
            }),
            (
                "/api/shell/sh_done/output".to_owned(),
                200,
                json!({"data": {"output": "Finished\n", "cursor": 9, "size": 9}}).to_string(),
            ),
        ]);
        let (events, event_rx) = crate::driver::test_event_channel();
        let tracker = ShellWorkTracker::default();
        let mut confirmed = shell_work_item(&shell_row("sh_done", "running", "ses_1")).unwrap();
        confirmed.background = true;
        tracker.track(confirmed);
        assert!(tracker.begin_output_read("sh_done").is_some());
        tracker.end_output_read("sh_done", 4);

        let items = reconcile_shells(
            port,
            "E:/work/fintwind",
            "ses_1",
            &tracker,
            &HashSet::new(),
            || false,
            &events,
        );

        // Fully drained and settled: the tracker stops watching it, and the
        // registry keeps the row through the settle upsert instead.
        assert!(tracker.get("sh_done").is_none());
        assert!(items.is_empty());
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert!(seen.iter().any(|event| matches!(
            event,
            DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { delta, .. })
                if delta == "Finished\n"
        )));
        match seen.last().unwrap() {
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) => {
                assert_eq!(item.status, BackgroundWorkStatus::Completed);
                assert_eq!(item.exit_code, Some(0));
                assert!(!item.can_stop);
                assert!(item.background);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn a_removed_shell_settles_lost_only_while_it_was_live() {
        let port = serve_shell_routes(vec![
            (
                "/api/shell".to_owned(),
                200,
                json!({"data": []}).to_string(),
            ),
            ("/api/shell/sh_gone".to_owned(), 404, String::from("{}")),
        ]);
        let (events, event_rx) = crate::driver::test_event_channel();
        let tracker = ShellWorkTracker::default();
        tracker.track(shell_work_item(&shell_row("sh_gone", "running", "ses_1")).unwrap());

        let _ = reconcile_shells(
            port,
            "E:/work/fintwind",
            "ses_1",
            &tracker,
            &HashSet::new(),
            || false,
            &events,
        );

        assert!(tracker.get("sh_gone").is_none());
        match event_rx.try_recv().unwrap() {
            DriverEvent::BackgroundWork(BackgroundWorkEvent::Upsert(item)) => {
                assert_eq!(item.status, BackgroundWorkStatus::Lost);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn a_failed_listing_keeps_tracked_shells_exactly_as_they_are() {
        // A port with nothing listening: the listing itself fails, which
        // must not retire live work as Lost.
        let dead = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.local_addr().unwrap().port()
        };
        let (events, event_rx) = crate::driver::test_event_channel();
        let tracker = ShellWorkTracker::default();
        tracker.track(shell_work_item(&shell_row("sh_live", "running", "ses_1")).unwrap());

        let items = reconcile_shells(
            dead,
            "E:/work/fintwind",
            "ses_1",
            &tracker,
            &HashSet::new(),
            || false,
            &events,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, BackgroundWorkStatus::Running);
        assert_eq!(tracker.get("sh_live").unwrap().cursor, 0);
        assert!(event_rx.try_recv().is_err(), "nothing was emitted");
    }

    #[test]
    fn a_stale_walk_stops_paging_further_output() {
        // One page is paged, then a newer refresh supersedes this walk: the
        // second shell must not be paged by the abandoned round.
        let port = serve_shell_routes(vec![
            (
                "/api/shell".to_owned(),
                200,
                json!({"data": [
                    shell_row("sh_a", "running", "ses_1"),
                    shell_row("sh_b", "running", "ses_1")
                ]})
                .to_string(),
            ),
            (
                "/api/shell/sh_a/output".to_owned(),
                200,
                json!({"data": {"output": "a\n", "cursor": 2, "size": 2}}).to_string(),
            ),
            (
                "/api/shell/sh_b/output".to_owned(),
                200,
                json!({"data": {"output": "b\n", "cursor": 2, "size": 2}}).to_string(),
            ),
        ]);
        let (events, event_rx) = crate::driver::test_event_channel();
        let tracker = ShellWorkTracker::default();
        for id in ["sh_a", "sh_b"] {
            let mut item = shell_work_item(&shell_row(id, "running", "ses_1")).unwrap();
            item.background = true;
            tracker.track(item);
        }
        let checks = std::cell::Cell::new(0_usize);
        let items = reconcile_shells(
            port,
            "E:/work/fintwind",
            "ses_1",
            &tracker,
            &HashSet::new(),
            || {
                checks.set(checks.get() + 1);
                checks.get() > 1
            },
            &events,
        );

        // Both shells still ride the reconcile (nothing is lost), but only the
        // first was paged before the walk abandoned its pages.
        assert_eq!(items.len(), 2);
        assert_eq!(tracker.get("sh_a").unwrap().cursor, 2);
        assert_eq!(tracker.get("sh_b").unwrap().cursor, 0);
        let seen = event_rx.try_iter().collect::<Vec<_>>();
        assert_eq!(
            seen.iter()
                .filter(|event| matches!(
                    event,
                    DriverEvent::BackgroundWork(BackgroundWorkEvent::OutputDelta { .. })
                ))
                .count(),
            1
        );
    }

    /// A loopback stand-in for the message route the restore scan pages.
    fn serve_message_page(body: String) -> u16 {
        serve_shell_routes(vec![("/api/session/ses_1/message".to_owned(), 200, body)])
    }

    #[test]
    fn the_stored_transcript_names_shells_it_backgrounded() {
        let port = serve_message_page(
            json!({"data": [
                {"type": "user", "content": [{"type": "text", "text": "build it"}]},
                {"type": "assistant", "content": [
                    {"type": "tool", "id": "call_1", "name": "shell", "state": {
                        "input": {"command": "cargo check --workspace", "workdir": "E:/work/fintwind"},
                        "metadata": {"status": "running", "shellID": "sh_restored"}
                    }}
                ]},
                // A foreground tool result carries no shellID and is not named.
                {"type": "assistant", "content": [
                    {"type": "tool", "id": "call_2", "name": "shell", "state": {
                        "input": {"command": "ls"},
                        "metadata": {"status": "completed"}
                    }}
                ]}
            ]})
            .to_string(),
        );

        // The scan is a pure query: it names candidates and touches nothing,
        // so a candidate the server no longer runs never appears at all.
        assert_eq!(
            restore_backgrounded_shells(port, "ses_1"),
            ["sh_restored".to_owned()]
        );
    }

    #[test]
    fn a_failed_restore_read_names_nothing() {
        // Nothing listening: the scan proves nothing and names nothing.
        let dead = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.local_addr().unwrap().port()
        };

        assert!(restore_backgrounded_shells(dead, "ses_1").is_empty());
    }
}
