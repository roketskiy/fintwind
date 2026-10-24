//! HTTPS GET through the system curl, the workspace's one HTTP helper. The
//! daemon's usage fetch, the models.dev catalog fetch, and the provider
//! model-list request all go through here. Everything blocks and must run on
//! a background executor.

use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};

/// Windows 10 17063 and later ship curl in System32; macOS and Linux ship it
/// conventionally. The absolute path keeps a shadowed `curl` on `PATH` out of
/// the fetch.
#[cfg(not(windows))]
const CURL_PATH: &str = "/usr/bin/curl";
#[cfg(windows)]
const CURL_PATH: &str = r"C:\Windows\System32\curl.exe";

/// The process-wide HTTP proxy every fetch in this module rides through, as a
/// normalized URL like `http://127.0.0.1:7897`. The daemon installs it at
/// startup and again whenever settings change; the desktop app does the same
/// for its own process. A process-level value means neither the fetch
/// signatures nor their many call sites grow a proxy parameter.
static ACTIVE_PROXY: std::sync::RwLock<Option<std::sync::Arc<str>>> = std::sync::RwLock::new(None);

/// Install (or clear, with `None`) the proxy every fetch in this module rides
/// through. The value passes through [`normalize_proxy_url`], so a malformed
/// address degrades to "no proxy" rather than failing every later fetch.
pub fn set_active_proxy(proxy: Option<String>) {
    let normalized = proxy.as_deref().and_then(normalize_proxy_url);
    *ACTIVE_PROXY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = normalized.map(std::sync::Arc::from);
}

/// The proxy currently installed for this process, if any. Also the read side
/// for consumers outside this module that must point other programs at the
/// same proxy: the daemon's `opencode serve` children read it to set the
/// conventional proxy environment variables.
pub fn active_proxy() -> Option<String> {
    ACTIVE_PROXY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|proxy| proxy.to_string())
}

/// Validate a user-supplied proxy address and return it in the form curl's
/// `proxy` option expects. An empty input reads as "no proxy"; a bare
/// `host:port` gains the `http://` scheme. Characters that cannot ride a curl
/// config value — quotes, whitespace, control characters — are refused rather
/// than escaped, so a bad address fails validation instead of corrupting the
/// request.
pub fn normalize_proxy_url(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        let (_host, port) = trimmed.rsplit_once(':')?;
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        // A bare `host:port` is an HTTP proxy by convention; curl assumes the
        // scheme too.
        format!("http://{trimmed}")
    };
    if candidate
        .chars()
        .any(|character| character.is_whitespace() || character == '"' || character.is_control())
    {
        return None;
    }
    let (scheme, host) = candidate.split_once("://")?;
    if !matches!(scheme, "http" | "https") || host.is_empty() || host.contains('/') {
        return None;
    }
    Some(candidate)
}

/// The `-K` config lines that point curl at the active proxy. Every request
/// builder appends these next to the headers, so the proxy address rides the
/// same stdin channel and never appears in a process list.
fn proxy_config_lines() -> Vec<String> {
    match active_proxy() {
        Some(proxy) => vec![format!(r#"proxy = "{proxy}""#)],
        None => Vec::new(),
    }
}

/// One HTTPS GET through curl, returning `(status, body)`. Headers ride the
/// same `-K` config-file mechanism, so credentials never appear in a process
/// list. `max_time_secs` bounds the whole request. Blocking.
pub fn http_get(
    url: &str,
    headers: &[String],
    max_time_secs: u64,
) -> anyhow::Result<(u16, String)> {
    let mut child = curl_command()
        .arg("-sS")
        .arg("--max-time")
        .arg(max_time_secs.to_string())
        .args(["-D", "-", "-K", "-"])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl")?;
    take_stdin(&mut child, headers)?;
    let output = child.wait_with_output().context("curl did not finish")?;
    if !output.status.success() {
        return Err(curl_error(&output.stderr));
    }
    split_status_and_body(&String::from_utf8_lossy(&output.stdout))
}

/// A bounded JSON request. Headers and the optional body ride stdin, never argv.
/// Redirects are deliberately not followed: an account credential must stay on
/// the exact allowlisted origin its caller selected. Blocking, no automatic retry.
pub fn http_json(
    method: &str,
    url: &str,
    headers: &[String],
    body: Option<&str>,
    max_time_secs: u64,
) -> anyhow::Result<(u16, String, Option<u64>)> {
    anyhow::ensure!(matches!(method, "GET" | "POST"), "unsupported JSON method");
    // curl config values cannot contain raw line breaks, quotes, or escapes.
    // Reject malformed credential headers instead of interpreting them as config.
    anyhow::ensure!(
        headers
            .iter()
            .all(|h| !h.chars().any(|c| c.is_control() || matches!(c, '"' | '\\'))),
        "invalid JSON header"
    );
    let mut child = curl_command()
        .args(["-q", "-sS", "--max-filesize", "1048576", "--max-time"])
        .arg(max_time_secs.to_string())
        .args(["-X", method, "-D", "-", "-K", "-"])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl")?;
    // Both JSON and headers are encoded as curl config strings. -q disables
    // user curlrc files, including verbose/trace options that could log secrets.
    let configured = (|| -> anyhow::Result<()> {
        let mut stdin = child.stdin.take().context("curl stdin is unavailable")?;
        for header in headers {
            writeln!(stdin, "header = \"{header}\"")?;
        }
        for line in proxy_config_lines() {
            writeln!(stdin, "{line}")?;
        }
        if let Some(body) = body {
            let escaped = body
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\r', "\\r")
                .replace('\n', "\\n");
            writeln!(stdin, "data = \"{escaped}\"")?;
        }
        Ok(())
    })();
    if configured.is_err() {
        // This exact Child was created above; never search for other processes.
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("could not configure JSON request");
    }
    let output = child.wait_with_output().context("curl did not finish")?;
    // stderr can contain upstream-controlled text; callers get no raw body/error.
    anyhow::ensure!(output.status.success(), "JSON request failed");
    let raw = String::from_utf8_lossy(&output.stdout);
    let (status, body) = split_status_and_body(&raw)?;
    let header_bytes = raw.len().saturating_sub(body.len());
    let retry = raw[..header_bytes].lines().rev().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("retry-after") {
            return None;
        }
        let value = value.trim();
        value.parse::<u64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|at| (at.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64)
        })
    });
    Ok((status, body, retry))
}

/// One HTTPS download written straight to `destination`, returning the number
/// of bytes curl reported. [`http_get`] decodes its body into a `String`, which
/// is both lossy and memory-hungry for a binary this size, so an installer
/// needs a helper of its own. `-L` follows the redirect a release asset URL
/// answers with, and `--fail` turns an HTTP error status into a non-zero exit
/// so an error page can never be mistaken for the installer. The parent of
/// `destination` has to exist. Reports written bytes on this background thread
/// every 100 ms when they change, plus the initial zero and final size. Blocking.
pub fn http_download(
    url: &str,
    headers: &[String],
    destination: &Path,
    max_time_secs: u64,
    mut on_progress: impl FnMut(u64),
) -> anyhow::Result<u64> {
    // curl writes the file itself, so a transfer that dies halfway would leave
    // something that looks like a complete installer. Download beside it and
    // rename only once curl reports success.
    let partial = partial_path(destination);
    std::fs::File::create(&partial).context("could not create download file")?;
    on_progress(0);
    let mut child = curl_command()
        .arg("-sS")
        .arg("-L")
        .arg("--fail")
        .arg("--max-time")
        .arg(max_time_secs.to_string())
        .args(["-K", "-"])
        .arg("-o")
        .arg(&partial)
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl")?;
    take_stdin(&mut child, headers)?;
    let mut last_bytes = 0;
    while child
        .try_wait()
        .context("could not check download status")?
        .is_none()
    {
        let bytes = std::fs::metadata(&partial)
            .map(|file| file.len())
            .unwrap_or(0);
        if bytes > last_bytes {
            on_progress(bytes);
            last_bytes = bytes;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = child.wait_with_output().context("curl did not finish")?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(curl_error(&output.stderr));
    }
    std::fs::rename(&partial, destination).with_context(|| {
        format!(
            "could not move the downloaded file to {}",
            destination.display()
        )
    })?;
    let bytes = std::fs::metadata(destination)?.len();
    on_progress(bytes);
    Ok(bytes)
}

fn partial_path(destination: &Path) -> PathBuf {
    let mut partial = destination.as_os_str().to_owned();
    partial.push(".part");
    PathBuf::from(partial)
}

/// What one probe of a URL measured.
pub struct Probed {
    /// How long the first bytes took to arrive.
    pub elapsed: Duration,
    /// How many bytes arrived, so a caller can tell a real transfer from a
    /// short answer that merely carried a 200.
    pub bytes: u64,
}

/// Measures how long the first `range` bytes of `url` take. A caller choosing
/// between sources uses this instead of committing a whole download to a host
/// that only answers slowly.
///
/// The body goes to `destination` because it is binary, which also leaves
/// stdout carrying nothing but the `-w` report — written to the same stream the
/// two would concatenate and the time could not be read back. curl formats its
/// report with the current locale's decimal separator, so a comma is accepted
/// too. Blocking.
pub fn http_probe(
    url: &str,
    headers: &[String],
    range: &str,
    destination: &Path,
    max_time_secs: u64,
) -> anyhow::Result<Probed> {
    let mut child = curl_command()
        .arg("-sS")
        .arg("-L")
        .arg("--fail")
        .arg("--max-time")
        .arg(max_time_secs.to_string())
        .arg("--range")
        .arg(range)
        .args(["-K", "-"])
        .arg("-o")
        .arg(destination)
        .arg("-w")
        .arg("%{time_total} %{size_download}")
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl")?;
    take_stdin(&mut child, headers)?;
    let output = child.wait_with_output().context("curl did not finish")?;
    if !output.status.success() {
        let _ = std::fs::remove_file(destination);
        return Err(curl_error(&output.stderr));
    }
    let report = String::from_utf8_lossy(&output.stdout);
    let (seconds, bytes) = parse_probe_report(&report)?;
    Ok(Probed {
        elapsed: Duration::from_secs_f64(seconds),
        bytes,
    })
}

/// The `-w` report is `<seconds> <bytes>` on a line of its own, because the
/// body went to a file and not to stdout. curl formats the time with the
/// current locale's decimal separator, so a comma is accepted too. Pure, so the
/// shape is testable without a network.
fn parse_probe_report(report: &str) -> anyhow::Result<(f64, u64)> {
    let mut fields = report.split_whitespace();
    let seconds = fields.next().unwrap_or_default();
    let bytes = fields.next().unwrap_or_default();
    let seconds: f64 = seconds
        .replace(',', ".")
        .parse()
        .map_err(|_| anyhow!("curl reported an unreadable transfer time {seconds:?}"))?;
    let bytes: u64 = bytes
        .parse()
        .map_err(|_| anyhow!("curl reported an unreadable transfer size {bytes:?}"))?;
    Ok((seconds, bytes))
}

/// Hand the child its `-K -` config, which is what keeps a credential out of a
/// process list. curl starts the transfer only once the config reaches EOF, so
/// taking the pipe and dropping it here is what releases the request rather
/// than a detail of the call site.
fn take_stdin(child: &mut std::process::Child, headers: &[String]) -> anyhow::Result<()> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("curl stdin is unavailable"))?;
    for header in headers {
        writeln!(stdin, "header = \"{header}\"").context("could not configure curl")?;
    }
    for line in proxy_config_lines() {
        writeln!(stdin, "{line}").context("could not configure curl")?;
    }
    Ok(())
}

/// curl's own diagnosis, which is the only explanation there is for a status
/// `0` or a failed transfer: its last stderr line names the cause.
fn curl_error(stderr: &[u8]) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(stderr);
    let error = stderr
        .lines()
        .last()
        .map(str::trim)
        .filter(|error| !error.is_empty())
        .unwrap_or("curl failed");
    anyhow!("{error}")
}

/// What one streaming POST found. `first_data` carries the first SSE `data:`
/// line and when it arrived, measured from just before curl was spawned —
/// the connection setup counts toward a first-token latency. `error_body`
/// holds the response body when the status was not 200, and the body
/// accumulated while waiting when it was 200 but no data line ever came, so
/// the caller can quote the provider's own error sentence in both cases.
pub struct StreamedPost {
    pub status: u16,
    pub first_data: Option<(Duration, String)>,
    pub error_body: String,
    /// curl hit its `--max-time` before answering at all.
    pub timed_out: bool,
}

/// The response body is capped so a chatty error page cannot balloon the
/// probe's memory.
const MAX_STREAMED_ERROR_BYTES: usize = 16 * 1024;

/// One streaming HTTPS POST through curl, read incrementally: the request is
/// how the Providers page measures a model's first-token latency. Returns
/// after the first SSE `data:` line (the connection is torn down then), or
/// when the response ends. Headers ride the same `-K` config-file mechanism
/// as [`http_get`], so credentials never appear in a process list; the body
/// carries no credential and rides the argument list. Blocking.
pub fn http_post_stream(
    url: &str,
    headers: &[String],
    body: &str,
    max_time_secs: u64,
) -> anyhow::Result<StreamedPost> {
    let started = Instant::now();
    let mut child = curl_command()
        .arg("-sS")
        // `-N` disables output buffering, so the reader sees each server-
        // flushed event as it arrives instead of a delayed burst.
        .arg("-N")
        .arg("--max-time")
        .arg(max_time_secs.to_string())
        .args(["-D", "-", "-K", "-", "--data-binary"])
        .arg(body)
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run curl")?;
    // curl starts the transfer only once the config reaches EOF; holding the
    // write end would stall it forever, so `take_stdin` drops it for us.
    take_stdin(&mut child, headers)?;

    let mut reader = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("curl stdout is unavailable"))?,
    );
    let status = read_response_status(&mut reader)?;
    let mut post = StreamedPost {
        status,
        first_data: None,
        error_body: String::new(),
        timed_out: false,
    };
    if status == 200 {
        read_until_first_data(&mut reader, started, &mut post);
    } else {
        read_capped(&mut reader, &mut post.error_body);
    }

    // The stream is only ever cut short on purpose: a first token arrived, or
    // an error body has been read to the cap. An early kill leaves the exit
    // code meaningless, so the timeout check reads the code only when curl
    // was allowed to finish on its own.
    let killed = post.first_data.is_some();
    if killed {
        let _ = child.kill();
    }
    let exit_code = child.wait().ok().and_then(|status| status.code());
    if exit_code == Some(CURL_TIMED_OUT_EXIT_CODE) {
        post.timed_out = true;
    }
    if status == 0 && post.error_body.is_empty() {
        // No HTTP status line: curl's own diagnosis (DNS, TLS, refused,
        // timed out) is the only explanation there is.
        if let Some(stderr) = child.stderr.take() {
            read_capped(&mut BufReader::new(stderr), &mut post.error_body);
        }
    }
    Ok(post)
}

/// curl's exit code when `--max-time` expired.
const CURL_TIMED_OUT_EXIT_CODE: i32 = 28;

/// The status line and headers, consumed through the blank separator so the
/// next read is pure body. Status `0` means no HTTP answer ever arrived
/// (connection failed, curl killed mid-headers).
fn read_response_status(reader: &mut impl BufRead) -> anyhow::Result<u16> {
    let mut final_status = 0;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return Ok(final_status),
            Ok(_) => {}
        }
        let status_line = line.trim_end();
        let (status, reason) = parse_http_status_line(status_line).unwrap_or((0, ""));
        let intermediate = is_intermediate_response(status, reason);

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return Ok(if status == 0 { final_status } else { status }),
                Ok(_) => {}
            }
            if line.trim_end().is_empty() {
                break;
            }
        }

        if status == 0 {
            return Ok(final_status);
        }
        final_status = status;
        if !intermediate {
            return Ok(status);
        }
    }
}

/// Read SSE lines until the first `data:` line that could carry a token;
/// meanwhile accumulate the body (capped) so a 200 that turns out to be an
/// error document can still be quoted.
fn read_until_first_data(reader: &mut impl BufRead, started: Instant, post: &mut StreamedPost) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let trimmed = line.trim_end();
        if let Some(data) = trimmed.strip_prefix("data:") {
            let data = data.trim();
            if !data.is_empty() && data != "[DONE]" {
                post.first_data = Some((started.elapsed(), trimmed.to_owned()));
                return;
            }
        }
        append_capped(&mut post.error_body, trimmed);
    }
}

/// Drain the rest of the response into `into`, up to the cap. Blocking until
/// the response ends — bounded by curl's `--max-time`.
fn read_capped(reader: &mut impl Read, into: &mut String) {
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                append_capped_bytes(into, &chunk[..read]);
                if into.len() >= MAX_STREAMED_ERROR_BYTES {
                    return;
                }
            }
        }
    }
}

fn append_capped(into: &mut String, line: &str) {
    if into.len() < MAX_STREAMED_ERROR_BYTES {
        into.push_str(line);
        into.push('\n');
    }
}

fn append_capped_bytes(into: &mut String, bytes: &[u8]) {
    if into.len() < MAX_STREAMED_ERROR_BYTES {
        into.push_str(&String::from_utf8_lossy(bytes));
    }
}

fn curl_command() -> std::process::Command {
    let mut command = std::process::Command::new(CURL_PATH);
    // Fintwind's desktop build is a GUI-subsystem binary with no console of
    // its own; without this flag CreateProcess flashes one for every child.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// `-D -` prefixes the body with response headers. HTTPS proxies and
/// informational responses can add a header block before the final response;
/// discard those blocks so the returned body starts at the actual payload.
fn split_status_and_body(raw: &str) -> anyhow::Result<(u16, String)> {
    let mut response = raw;
    loop {
        let (headers, body) = split_header_block(response).unwrap_or((response, ""));
        let status_line = headers.lines().next().unwrap_or_default();
        let (status, reason) = parse_http_status_line(status_line)
            .ok_or_else(|| anyhow!("the response carries no HTTP status"))?;
        if is_intermediate_response(status, reason)
            && split_header_block(body).is_some_and(|(headers, _)| {
                headers
                    .lines()
                    .next()
                    .and_then(parse_http_status_line)
                    .is_some()
            })
        {
            response = body;
            continue;
        }
        return Ok((status, body.to_owned()));
    }
}

fn split_header_block(raw: &str) -> Option<(&str, &str)> {
    raw.split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))
}

fn parse_http_status_line(line: &str) -> Option<(u16, &str)> {
    let (protocol, remainder) = line.trim().split_once(' ')?;
    if !protocol.starts_with("HTTP/") {
        return None;
    }
    let remainder = remainder.trim_start();
    let (status, reason) = remainder.split_once(' ').unwrap_or((remainder, ""));
    Some((status.parse().ok()?, reason.trim()))
}

fn is_intermediate_response(status: u16, reason: &str) -> bool {
    (100..200).contains(&status)
        || (status == 200 && reason.eq_ignore_ascii_case("connection established"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn status_and_body_split_like_the_daemon_fetch() {
        assert_eq!(
            split_status_and_body("HTTP/1.1 200 OK\r\ncontent-type: x\r\n\r\nbody").unwrap(),
            (200, "body".to_owned())
        );
        assert_eq!(
            split_status_and_body("HTTP/2 404\n\n").unwrap(),
            (404, String::new())
        );
        assert!(split_status_and_body("garbage").is_err());
    }

    #[test]
    fn status_and_body_split_skips_proxy_connect_headers() {
        let raw = "HTTP/1.1 200 Connection established\r\nProxy-Agent: proxy\r\n\r\n\
                    HTTP/2 200\r\ncontent-type: application/json\r\n\r\n\
                    {\"tag_name\":\"v0.2.8\"}";
        assert_eq!(
            split_status_and_body(raw).unwrap(),
            (200, "{\"tag_name\":\"v0.2.8\"}".to_owned())
        );
    }

    #[test]
    fn streamed_status_uses_the_response_after_proxy_connect() {
        let raw = Cursor::new(
            "HTTP/1.1 200 Connection established\r\n\r\n\
             HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\n\r\n\
             {\"message\":\"rate limit\"}",
        );
        let mut reader = BufReader::new(raw);
        assert_eq!(read_response_status(&mut reader).unwrap(), 403);
    }

    #[test]
    fn streamed_read_stops_at_the_first_token_line() {
        let raw = Cursor::new(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n\
             event: message_start\r\ndata: {\"type\":\"message_start\"}\r\n\
             \r\ndata: [DONE]\r\n",
        );
        let mut reader = BufReader::new(raw);
        assert_eq!(read_response_status(&mut reader).unwrap(), 200);
        let mut post = StreamedPost {
            status: 200,
            first_data: None,
            error_body: String::new(),
            timed_out: false,
        };
        read_until_first_data(&mut reader, Instant::now(), &mut post);
        assert_eq!(
            post.first_data.as_ref().map(|(_, line)| line.as_str()),
            Some("data: {\"type\":\"message_start\"}")
        );
    }

    #[test]
    fn streamed_read_reports_a_body_without_data_lines() {
        let raw = Cursor::new(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"error\":{\"message\":\"streaming unsupported\"}}",
        );
        let mut reader = BufReader::new(raw);
        assert_eq!(read_response_status(&mut reader).unwrap(), 200);
        let mut post = StreamedPost {
            status: 200,
            first_data: None,
            error_body: String::new(),
            timed_out: false,
        };
        read_until_first_data(&mut reader, Instant::now(), &mut post);
        assert!(post.first_data.is_none());
        assert!(post.error_body.contains("streaming unsupported"));
    }

    #[test]
    fn streamed_status_line_reading_consumes_the_whole_header_block() {
        // A failed response: the body must start clean, without the
        // remaining headers, so the caller can parse the error document.
        let raw = Cursor::new(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\n\r\n{\"error\":{\"message\":\"bad key\"}}",
        );
        let mut reader = BufReader::new(raw);
        assert_eq!(read_response_status(&mut reader).unwrap(), 401);
        let mut body = String::new();
        read_capped(&mut reader, &mut body);
        assert_eq!(body, "{\"error\":{\"message\":\"bad key\"}}");
    }

    #[test]
    fn streamed_no_answer_reads_as_status_zero() {
        let raw = Cursor::new("curl: (7) Failed to connect");
        let mut reader = BufReader::new(raw);
        assert_eq!(read_response_status(&mut reader).unwrap(), 0);
    }

    /// The shape the Windows system curl actually wrote for a 64 KB probe, as
    /// observed on the command line.
    #[test]
    fn a_probe_report_carries_the_time_then_the_byte_count() {
        assert_eq!(
            parse_probe_report("0.769708 65536\n").unwrap(),
            (0.769708, 65536)
        );
        assert_eq!(parse_probe_report("  0.120730 0  ").unwrap(), (0.120730, 0));
    }

    #[test]
    fn a_probe_report_in_a_comma_decimal_locale_still_parses() {
        assert_eq!(
            parse_probe_report("1,250500 65536").unwrap(),
            (1.2505, 65536)
        );
    }

    #[test]
    fn a_probe_report_without_both_numbers_is_refused() {
        assert!(parse_probe_report("").is_err());
        assert!(parse_probe_report("0.769708").is_err());
        assert!(parse_probe_report("fast 65536").is_err());
        assert!(parse_probe_report("0.769708 plenty").is_err());
    }
}
