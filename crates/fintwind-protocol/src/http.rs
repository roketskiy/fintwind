//! HTTPS GET through the system curl, the workspace's one HTTP helper. The
//! daemon's usage fetch, the models.dev catalog fetch, and the provider
//! model-list request all go through here. Everything blocks and must run on
//! a background executor.

use std::io::{BufRead, BufReader, Read, Write as _};
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
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("curl stdin is unavailable"))?;
        for header in headers {
            writeln!(stdin, "header = \"{header}\"").context("could not configure curl")?;
        }
    }
    let output = child.wait_with_output().context("curl did not finish")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let error = stderr
            .lines()
            .last()
            .map(str::trim)
            .filter(|error| !error.is_empty())
            .unwrap_or("curl failed");
        return Err(anyhow!("{error}"));
    }
    split_status_and_body(&String::from_utf8_lossy(&output.stdout))
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
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("curl stdin is unavailable"))?;
        for header in headers {
            writeln!(stdin, "header = \"{header}\"").context("could not configure curl")?;
        }
    }
    // curl starts the transfer only once the config reaches EOF; holding the
    // write end would stall it forever.
    drop(child.stdin.take());

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
    let mut line = String::new();
    let mut status = 0u16;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if status == 0
            && let Some(code) = trimmed
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse::<u16>().ok())
        {
            status = code;
        }
    }
    Ok(status)
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

/// `-D -` prefixes the body with the response headers; the status code is on
/// the first line and the body follows the blank separator line.
fn split_status_and_body(raw: &str) -> anyhow::Result<(u16, String)> {
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("the response carries no HTTP status"))?;
    let body = raw
        .split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))
        .map(|(_, body)| body)
        .unwrap_or_default();
    Ok((status, body.to_owned()))
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
}
