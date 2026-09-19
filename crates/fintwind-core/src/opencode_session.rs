//! OpenCode server lifecycle and native-session helpers.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::model::ProviderResumeCursor;

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Forking copies every retained message and part into a new native session.
/// A long task can legitimately take longer than the ordinary request budget;
/// this operation already runs off the UI thread.
const FORK_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// A revert snapshots the worktree before restoring the boundary, so it can
/// also exceed the ordinary request budget on a large repository.
const REVERT_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// The server binds its port about a second before the app behind it starts
/// answering, and a request accepted in that window is never answered at all.
/// A startup probe caught there must give up quickly and retry — at the full
/// `HTTP_TIMEOUT` one hung probe would eat the whole start budget.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// OpenCode 2.0.5 replaced `/api/health` with `/api/status`; 2.0.6 renamed
/// that to `/api/info`. Probe newest first, keep older paths for earlier CLIs.
const HEALTH_PROBE_PATHS: [&str; 3] = ["/api/info", "/api/status", "/api/health"];
/// How many messages one request of the native transcript may return before
/// the page boundary is hit; the batch keeps going with the cursor.
const MESSAGE_PAGE_LIMIT: usize = 200;

/// Port → Basic-auth password, remembered so requests that hold only a port
/// (the event-stream reader and the usage-metadata poll) can authenticate
/// against the credentials the owning server was started with. Ports are
/// unique per process, so the map cannot alias two servers.
fn server_passwords() -> &'static Mutex<HashMap<u16, String>> {
    static PASSWORDS: OnceLock<Mutex<HashMap<u16, String>>> = OnceLock::new();
    PASSWORDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The `Authorization: Basic ...` header for the server on `port`, if a
/// password was registered for it. opencode serves Basic-auth every request
/// and generate their own random password when none is injected, so Fintwind
/// supplies one and must present it on every connection, including the SSE
/// stream.
pub(crate) fn basic_authorization(port: u16) -> Option<String> {
    server_passwords().lock().get(&port).map(|password| {
        let credentials = BASE64.encode(format!("opencode:{password}"));
        format!("Authorization: Basic {credentials}")
    })
}

pub fn fork_session_at_turn(
    binary: &Path,
    cwd: &Path,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    // Shares the workspace's resident server when one is live; a transient
    // one is started and killed with the handle otherwise.
    let server = crate::opencode_pool::acquire(binary, cwd)?;
    fork_session_at_turn_on_server(&server, session_id, retained_turns)
}

/// Forks through the task's resident OpenCode server.
///
/// Starting a second `opencode serve` against the same workspace can contend
/// with the live process for OpenCode's local resources. Rewinds with a live
/// driver use this path instead, while cold sessions still use the standalone
/// helper above.
pub(crate) fn fork_session_at_turn_on_server(
    server: &OpenCodeServer,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let native = native_messages(server, session_id)?;
    fork_session_with_message_ids(server, session_id, &native, retained_turns)
}

pub(crate) fn fork_session_removing_turns_on_server(
    server: &OpenCodeServer,
    session_id: &str,
    turns_to_remove: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let native = native_messages(server, session_id)?;
    let retained_turns = retained_turn_count(native.user_ids.len(), turns_to_remove)?;
    fork_session_with_message_ids(server, session_id, &native, retained_turns)
}

/// Whether the native session carries an active revert marker — the user
/// rewound and has not sent the replacement prompt yet.
pub(crate) fn native_session_has_revert(
    server: &OpenCodeServer,
    session_id: &str,
) -> anyhow::Result<bool> {
    let session = server
        .request_with_timeout(
            "GET",
            &format!("/api/session/{}", encode_path_segment(session_id)),
            None,
            FORK_HTTP_TIMEOUT,
        )
        .context("could not read the OpenCode session")?;
    let session = session.get("data").unwrap_or(&session);
    Ok(session
        .get("revert")
        .and_then(|revert| revert.get("messageID"))
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty()))
}

/// Reverts the native conversation to just before one of its user turns.
///
/// OpenCode marks the boundary instead of deleting anything: the messages
/// from that turn onward stay in storage but are excluded from the next
/// model call, and its own snapshot machinery restores the worktree files
/// the dropped turns had changed. The session id never changes. The marked
/// messages are only removed from storage when the next prompt arrives, so
/// a revert must be followed by the replacement prompt in the same
/// rewind-and-resend flow.
pub(crate) fn revert_session_at_message(
    server: &OpenCodeServer,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<()> {
    // A previous rewind that never got its replacement prompt leaves a
    // revert marker behind. Clear it first so this boundary move starts
    // from the whole conversation, and so the fresh marker's snapshot
    // describes the worktree the user sees right now.
    if native_session_has_revert(server, session_id)? {
        unrevert_session(server, session_id)?;
    }
    let native = native_messages(server, session_id)?;
    let Some(message_id) = fork_message_id(&native.user_ids, retained_turns)? else {
        // No native user turn sits after the boundary — the conversation is
        // empty or every turn is already retained — so there is nothing to
        // hide. Keep the session: the stored cursor still points at it.
        return Ok(());
    };
    // The revert/unrevert routes exist only without the /api prefix: the
    // prefixed variants fall through to the SPA fallback and return HTML
    // (verified against the bundled SDK, which posts to /session/{id}/revert).
    server.request_with_timeout(
        "POST",
        &format!("/session/{}/revert", encode_path_segment(session_id)),
        Some(&json!({"messageID": message_id})),
        REVERT_HTTP_TIMEOUT,
    )?;
    Ok(())
}

/// Undoes a previous revert: OpenCode restores its snapshot, so the
/// reverted turns' file changes are back on disk and the transcript is
/// whole again.
pub(crate) fn unrevert_session(server: &OpenCodeServer, session_id: &str) -> anyhow::Result<()> {
    server.request_with_timeout(
        "POST",
        // See revert_session_at_message: no /api prefix on these routes.
        &format!("/session/{}/unrevert", encode_path_segment(session_id)),
        None,
        REVERT_HTTP_TIMEOUT,
    )?;
    Ok(())
}

fn retained_turn_count(total_turns: usize, turns_to_remove: usize) -> anyhow::Result<usize> {
    total_turns.checked_sub(turns_to_remove).ok_or_else(|| {
        anyhow!(
            "OpenCode has only {total_turns} native turns, but fintwind needs to remove {turns_to_remove}"
        )
    })
}

/// The native transcript as opencode stores it: separated user turns and the
/// id of the newest message of any kind (used to fork "keep everything").
struct NativeMessages {
    user_ids: Vec<String>,
    last_id: Option<String>,
}

fn native_messages(server: &OpenCodeServer, session_id: &str) -> anyhow::Result<NativeMessages> {
    let mut user_ids = Vec::new();
    let mut last_id = None;
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(cursor) => format!(
                "/api/session/{}/message?limit={}&cursor={}",
                encode_path_segment(session_id),
                MESSAGE_PAGE_LIMIT,
                encode_path_segment(cursor)
            ),
            None => format!(
                "/api/session/{}/message?limit={}",
                encode_path_segment(session_id),
                MESSAGE_PAGE_LIMIT
            ),
        };
        let messages = server.request_with_timeout("GET", &path, None, FORK_HTTP_TIMEOUT)?;
        let data = messages
            .pointer("/data")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("OpenCode returned an invalid message list"))?;
        // The wire lists messages newest first, so the first page's first
        // entry is the newest message of the whole conversation.
        if last_id.is_none() {
            if let Some(id) = data
                .first()
                .and_then(|message| message.get("id").and_then(Value::as_str))
            {
                last_id = Some(id.to_owned());
            }
        }
        for message in data {
            if is_native_user_turn(message) {
                if let Some(id) = message.get("id").and_then(Value::as_str) {
                    user_ids.push(id.to_owned());
                }
            }
        }
        // The next cursor repeats the current one when the paged list has
        // already been fully scanned, which would loop forever.
        match messages
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            Some(next) if cursor.as_deref() != Some(next.as_str()) => cursor = Some(next),
            _ => break,
        }
    }
    // Pages arrive newest first; oldest-first ordering keeps `fork_message_id`
    // counting the same "first user turn" the v1 driver did.
    user_ids.reverse();
    Ok(NativeMessages { user_ids, last_id })
}

fn fork_session_with_message_ids(
    server: &OpenCodeServer,
    session_id: &str,
    native: &NativeMessages,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let fork_at = fork_message_id(&native.user_ids, retained_turns)?;
    let body = match fork_at {
        // opencode forks at an explicit boundary instead of v1's bare
        // message id: `before` keeps everything up to (not including) the
        // message, which matches the v1 "keep the retained prefix" semantics.
        Some(message_id) => json!({"boundary": {"type": "before", "messageID": message_id}}),
        // Keeping every turn needs a boundary too — `through` the newest
        // message of the conversation copies the whole transcript.
        None => match native.last_id.as_deref() {
            Some(last_id) => json!({"boundary": {"type": "through", "messageID": last_id}}),
            // An empty conversation has nothing to fork; the original session
            // already is the full copy.
            None => {
                return Ok(ProviderResumeCursor::OpenCode {
                    session_id: session_id.to_owned(),
                });
            }
        },
    };
    let fork_path = format!("/api/session/{}/fork", encode_path_segment(session_id));
    let fork = server.request_with_timeout("POST", &fork_path, Some(&body), FORK_HTTP_TIMEOUT)?;
    let fork_id = fork
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| fork.pointer("/data/id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("OpenCode returned no forked session ID"))?;
    Ok(ProviderResumeCursor::OpenCode {
        session_id: fork_id.to_owned(),
    })
}

fn fork_message_id(message_ids: &[String], retained_turns: usize) -> anyhow::Result<Option<&str>> {
    if retained_turns > message_ids.len() {
        bail!(
            "OpenCode has only {} native turns, but fintwind needs {retained_turns}",
            message_ids.len()
        );
    }
    Ok(message_ids.get(retained_turns).map(String::as_str))
}

pub(crate) struct OpenCodeServer {
    child: Mutex<Child>,
    pub(crate) port: u16,
}

impl OpenCodeServer {
    pub(crate) fn start(binary: &Path, cwd: &Path) -> anyhow::Result<Self> {
        Self::start_with_env(binary, cwd, &[])
    }

    /// Starts the server with extra environment, so a caller can hand it the
    /// Computer Use configuration the same way a one-shot invocation got it.
    pub(crate) fn start_with_env(
        binary: &Path,
        cwd: &Path,
        environment: &[(String, String)],
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("could not reserve a local port for OpenCode")?;
        let port = listener.local_addr()?.port();
        drop(listener);

        let mut command = crate::command_env::command(binary);
        for (name, value) in environment {
            command.env(name, value);
        }
        // opencode serves enforce Basic auth and generate their own random
        // password when none is provided (an empty value behaves the same as
        // unset), so Fintwind injects its own random password and authenticates
        // every request against the exact credentials it started.
        let password = uuid::Uuid::new_v4().to_string();
        let command = command
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .env("OPENCODE_SERVER_USERNAME", "opencode")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child =
            crate::command_env::spawn(command).context("failed to start `opencode serve`")?;
        // The health probe below already needs the credentials, and the
        // event-stream reader reaches the server by port alone.
        server_passwords().lock().insert(port, password);
        let server = Self {
            child: Mutex::new(child),
            port,
        };
        let started_at = Instant::now();
        loop {
            if server_is_ready(&server) {
                return Ok(server);
            }
            if let Some(status) = server.child.lock().try_wait()? {
                bail!("OpenCode session server exited during startup ({status})");
            }
            if started_at.elapsed() >= SERVER_START_TIMEOUT {
                bail!("timed out starting the OpenCode session server");
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    pub(crate) fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<Value> {
        self.request_with_timeout(method, path, body, HTTP_TIMEOUT)
    }

    pub(crate) fn request_with_timeout(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        request_json_on_port(self.port, method, path, body, timeout)
    }

    /// Whether the server process is still running. `Child::try_wait` both
    /// observes and reaps an exited child; `kill(pid, 0)` cannot distinguish a
    /// running process from the unreaped zombie owned by this process.
    pub(crate) fn is_alive(&self) -> bool {
        self.child
            .lock()
            .try_wait()
            .is_ok_and(|status| status.is_none())
    }
}

fn server_is_ready(server: &OpenCodeServer) -> bool {
    HEALTH_PROBE_PATHS.iter().any(|path| {
        server
            .request_with_timeout("GET", path, None, HEALTH_PROBE_TIMEOUT)
            .is_ok()
    })
}

fn is_native_user_turn(message: &Value) -> bool {
    // opencode stores the transcript as flat messages: user turns carry
    // their text directly and system turns have their own types
    // (`synthetic`, `agent-switched`, `model-switched`, ...). The current
    // beta keeps the text in `content` parts; older builds used a flat
    // `text` field — accept both so fork boundaries keep working.
    if message.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    if message
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
    {
        return true;
    }
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts.iter().any(|part| {
                part.get("type").and_then(Value::as_str) == Some("text")
                    && part
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty())
            })
        })
}

impl OpenCodeServer {
    /// Terminates and reaps the owned child. The timeout is a graceful-exit
    /// budget; a server that ignores TERM is killed afterward.
    pub(crate) fn shutdown(&self, timeout: Duration) {
        server_passwords().lock().remove(&self.port);
        let mut child = self.child.lock();
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }

        #[cfg(unix)]
        {
            let _ = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        }
        #[cfg(not(unix))]
        {
            kill_process_tree(&mut child);
        }

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }

        kill_process_tree(&mut child);
        let _ = child.wait();
    }
}

impl Drop for OpenCodeServer {
    fn drop(&mut self) {
        server_passwords().lock().remove(&self.port);
        let child = self.child.get_mut();
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        #[cfg(unix)]
        let _ = child.kill();
        #[cfg(not(unix))]
        kill_process_tree(child);
        let _ = child.wait();
    }
}

/// Windows spawns `.cmd` shims (npm installs) through `cmd.exe`, so killing
/// the wrapper leaves the real server running as an orphan reaped by nobody.
/// `taskkill /T` takes the whole tree, wrapper and server alike. No-op on
/// Unix, where the child is the process itself.
#[cfg(not(unix))]
fn kill_process_tree(child: &mut Child) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

#[cfg(unix)]
fn kill_process_tree(_child: &mut Child) {}

/// Sends one request to a server identified by port alone. Readers that must
/// not keep the server alive (they only unblock when it exits) hold the port
/// instead of a handle and request through this.
pub(crate) fn request_json_on_port(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let body = body.map(serde_json::to_vec).transpose()?;
    let response = http_request(port, method, path, body.as_deref(), timeout)?;
    // Some routes answer 204 No Content — `prompt_async` among them — and
    // the status was already checked, so an empty success body is Null.
    if response.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&response)
        .with_context(|| format!("OpenCode returned invalid JSON for {method} {path}"))
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("could not connect to OpenCode on local port {port}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let body = body.unwrap_or_default();
    let mut headers = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(authorization) = basic_authorization(port) {
        headers.push_str(&authorization);
        headers.push_str("\r\n");
    }
    headers.push_str("\r\n");
    write!(stream, "{headers}")?;
    stream.write_all(body)?;
    stream.flush()?;

    let mut response = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        // HTTP/1.1 connections are allowed to stay alive after a complete
        // response, even when the client asks to close. Waiting for EOF made a
        // valid OpenCode response end as macOS EAGAIN once the socket timeout
        // elapsed. Stop at the protocol's own body boundary instead.
        if http_response_is_complete(&response)? {
            break;
        }
        let read = stream
            .read(&mut buffer)
            .with_context(|| format!("failed reading OpenCode response for {method} {path}"))?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
    }
    parse_http_response(&response)
}

fn http_response_is_complete(response: &[u8]) -> anyhow::Result<bool> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(false);
    };
    let headers = std::str::from_utf8(&response[..header_end])?;
    let body = &response[header_end + 4..];
    if header_value(headers, "transfer-encoding").is_some_and(|value| {
        value
            .split(',')
            .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
    }) {
        return chunked_body_is_complete(body);
    }
    if let Some(length) = header_value(headers, "content-length") {
        let length = length
            .trim()
            .parse::<usize>()
            .context("OpenCode returned an invalid HTTP content length")?;
        return Ok(body.len() >= length);
    }

    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok());
    Ok(status.is_some_and(|status| matches!(status, 204 | 205 | 304)))
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|line| {
        let (header, value) = line.split_once(':')?;
        header.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn chunked_body_is_complete(mut input: &[u8]) -> anyhow::Result<bool> {
    loop {
        let Some(line_end) = input.windows(2).position(|window| window == b"\r\n") else {
            return Ok(false);
        };
        let size_text = std::str::from_utf8(&input[..line_end])?
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)
            .context("OpenCode returned an invalid HTTP chunk size")?;
        input = &input[line_end + 2..];
        if size == 0 {
            return Ok(true);
        }
        if input.len() < size + 2 {
            return Ok(false);
        }
        if &input[size..size + 2] != b"\r\n" {
            bail!("OpenCode returned an invalid chunked response");
        }
        input = &input[size + 2..];
    }
}

fn parse_http_response(response: &[u8]) -> anyhow::Result<Vec<u8>> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        bail!("OpenCode returned an invalid HTTP response");
    };
    let headers = std::str::from_utf8(&response[..header_end])?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("OpenCode returned an invalid HTTP status"))?;
    let body = &response[header_end + 4..];
    let body = if headers.lines().any(|line| {
        line.eq_ignore_ascii_case("transfer-encoding: chunked")
            || line
                .to_ascii_lowercase()
                .starts_with("transfer-encoding: chunked")
    }) {
        decode_chunked(body)?
    } else {
        body.to_vec()
    };
    if !(200..300).contains(&status) {
        let detail = String::from_utf8_lossy(&body);
        bail!("OpenCode session request failed with HTTP {status}: {detail}");
    }
    Ok(body)
}

fn decode_chunked(mut input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let Some(line_end) = input.windows(2).position(|window| window == b"\r\n") else {
            bail!("OpenCode returned an invalid chunked response");
        };
        let size_text = std::str::from_utf8(&input[..line_end])?
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)
            .context("OpenCode returned an invalid HTTP chunk size")?;
        input = &input[line_end + 2..];
        if size == 0 {
            break;
        }
        if input.len() < size + 2 || &input[size..size + 2] != b"\r\n" {
            bail!("OpenCode returned a truncated HTTP chunk");
        }
        output.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
    Ok(output)
}

pub(crate) fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_fork_message_excludes_the_next_user_turn() {
        let messages = vec!["one".to_owned(), "two".to_owned(), "three".to_owned()];
        assert_eq!(fork_message_id(&messages, 0).unwrap(), Some("one"));
        assert_eq!(fork_message_id(&messages, 2).unwrap(), Some("three"));
        assert_eq!(fork_message_id(&messages, 3).unwrap(), None);
        assert!(fork_message_id(&messages, 4).is_err());
    }

    #[test]
    fn rollback_count_is_converted_to_the_retained_native_prefix() {
        assert_eq!(retained_turn_count(4, 1).unwrap(), 3);
        assert_eq!(retained_turn_count(4, 4).unwrap(), 0);
        assert!(retained_turn_count(4, 5).is_err());
    }

    #[test]
    fn revert_reuses_the_fork_boundary_and_dropping_every_turn_is_out_of_band() {
        // Retaining all four turns reverts at the fifth user message — the
        // same boundary arithmetic the fork path uses, so a revert always
        // marks a real remaining message instead of an off-by-one.
        let messages = vec![
            "u1".to_owned(),
            "u2".to_owned(),
            "u3".to_owned(),
            "u4".to_owned(),
        ];
        assert_eq!(fork_message_id(&messages, 4).unwrap(), None);
        assert_eq!(fork_message_id(&messages, 3).unwrap(), Some("u4"));
    }

    #[test]
    fn native_turn_filter_ignores_system_messages() {
        assert!(is_native_user_turn(&json!({
            "id": "msg_1",
            "type": "user",
            "text": "hello"
        })));
        assert!(!is_native_user_turn(&json!({
            "id": "msg_2",
            "type": "synthetic",
            "text": "system reminder"
        })));
        assert!(!is_native_user_turn(&json!({
            "id": "msg_3",
            "type": "agent-switched",
            "text": ""
        })));
        assert!(!is_native_user_turn(&json!({
            "id": "msg_4",
            "type": "user",
            "text": "   "
        })));
        assert!(!is_native_user_turn(&json!({
            "id": "msg_5",
            "type": "assistant",
            "text": "absent"
        })));
    }

    #[test]
    fn parses_content_length_and_chunked_http_responses() {
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(),
            b"{}"
        );
        assert_eq!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"id\r\n4\r\n\":1}\r\n0\r\n\r\n"
            )
            .unwrap(),
            b"{\"id\":1}"
        );
    }

    #[test]
    fn detects_complete_http_bodies_without_waiting_for_connection_close() {
        assert!(
            !http_response_is_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{").unwrap()
        );
        assert!(
            http_response_is_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap()
        );
        assert!(
            !http_response_is_complete(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{}"
            )
            .unwrap()
        );
        assert!(
            http_response_is_complete(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n"
            )
            .unwrap()
        );
        assert!(
            http_response_is_complete(b"HTTP/1.1 204 No Content\r\nConnection: keep-alive\r\n\r\n")
                .unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn liveness_probe_reaps_an_exited_child() {
        let child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("the probe child should start");
        let server = OpenCodeServer {
            child: Mutex::new(child),
            port: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && server.is_alive() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!server.is_alive(), "the exited child should be reaped");
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_waits_for_and_reaps_the_owned_child() {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("the probe child should start");
        let server = OpenCodeServer {
            child: Mutex::new(child),
            port: 0,
        };
        let started = Instant::now();
        server.shutdown(Duration::from_secs(3));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a TERM-responsive child should not consume the shutdown budget"
        );
        assert!(!server.is_alive());
    }

    /// Exercises the same cold-session path used when an edited message is
    /// submitted after Fintwind has relaunched. The source session is supplied by
    /// the caller so this never creates provider traffic; it only forks the
    /// already-completed native transcript and removes the test fork again.
    #[test]
    #[ignore = "requires an installed opencode and FINTWIND_OPENCODE_TEST_SESSION_ID"]
    fn forks_away_a_real_single_turn_session() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let session_id = std::env::var("FINTWIND_OPENCODE_TEST_SESSION_ID")
            .expect("set FINTWIND_OPENCODE_TEST_SESSION_ID to a completed one-turn session");
        let cwd = std::env::current_dir().expect("the test working directory should exist");
        let server = OpenCodeServer::start(&binary, &cwd).expect("the server should start");
        let ProviderResumeCursor::OpenCode {
            session_id: fork_id,
        } = fork_session_at_turn_on_server(&server, &session_id, 0)
            .expect("the first turn should be excluded from the fork");
        let messages = server
            .request(
                "GET",
                &format!("/api/session/{}/message", encode_path_segment(&fork_id)),
                None,
            )
            .expect("the fork should be readable");
        // A fork taken before the first user turn keeps no user turns; the
        // session's system messages (`model-switched` etc.) are still copied.
        assert!(
            messages
                .pointer("/data")
                .and_then(Value::as_array)
                .is_some_and(|data| {
                    !data
                        .iter()
                        .any(|message| message.get("type").and_then(Value::as_str) == Some("user"))
                }),
            "a fork taken before the first turn should keep no user turns"
        );
        server
            .request(
                "DELETE",
                &format!("/api/session/{}", encode_path_segment(&fork_id)),
                None,
            )
            .expect("the test fork should be removed");
    }
}
