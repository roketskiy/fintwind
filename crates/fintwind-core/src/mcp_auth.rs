//! Remote MCP OAuth through OpenCode's CLI.
//!
//! `opencode serve` on current V2 does not expose MCP OAuth over HTTP — POST
//! `/mcp/{name}/auth/authenticate` falls through to the static UI (HTTP 405),
//! and configured remotes have no Integration id. The CLI runs the PKCE
//! flow in-process, prints the authorization URL, and waits on its own
//! loopback callback. Fintwind starts that command, opens the URL as soon as
//! it appears, and waits for the process to finish.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

const AUTHENTICATE_TIMEOUT: Duration = Duration::from_secs(360);

/// In-flight `opencode mcp auth` child processes keyed by server name, so a
/// cancel request from the UI can kill the CLI that a login request is
/// waiting on. The daemon handles requests on independent threads, so the
/// cancel never queues behind the long-running login.
fn pending() -> &'static Mutex<HashMap<String, ChildHandle>> {
    static PENDING: OnceLock<Mutex<HashMap<String, ChildHandle>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ChildHandle {
    child: Arc<Mutex<Option<std::process::Child>>>,
    cancelled: Arc<AtomicBool>,
    generation: u64,
}

/// Kill the pending `opencode mcp auth` child for `name`, if any. Returns
/// false when no login is running for that server.
pub(crate) fn cancel(name: &str) -> bool {
    let handle = pending()
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(name));
    let Some(handle) = handle else {
        return false;
    };
    handle.cancelled.store(true, Ordering::SeqCst);
    kill_child(&handle.child);
    true
}

/// Remove `name`'s entry, but only when it belongs to `generation`; a stale
/// flow finishing late must not drop a newer flow's registration.
fn take_pending(name: &str, generation: u64) -> Option<ChildHandle> {
    pending()
        .lock()
        .ok()
        .and_then(|mut pending| match pending.get(name) {
            Some(handle) if handle.generation == generation => pending.remove(name),
            _ => None,
        })
}

fn kill_child(child_slot: &Mutex<Option<std::process::Child>>) {
    if let Ok(mut slot) = child_slot.lock()
        && let Some(mut child) = slot.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

pub(crate) fn authenticate(binary: &Path, directory: &Path, name: &str) -> anyhow::Result<()> {
    let mut command = crate::command_env::command(binary);
    command
        .args(["mcp", "auth", name])
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if pending()
        .lock()
        .is_ok_and(|pending| pending.contains_key(name))
    {
        bail!("a browser sign-in for {name} is already in progress");
    }
    let mut child = crate::command_env::spawn(&mut command)
        .with_context(|| format!("could not start `{} mcp auth {name}`", binary.display()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cancelled = Arc::new(AtomicBool::new(false));
    let child_slot = Arc::new(Mutex::new(Some(child)));
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst);
    if !try_register(name, child_slot.clone(), cancelled.clone(), generation) {
        // A concurrent request won the race; drop the fresh CLI so the two
        // flows do not fight over the same OAuth callback.
        kill_child(&child_slot);
        bail!("a browser sign-in for {name} is already in progress");
    }
    let _guard = PendingGuard {
        name: name.to_string(),
        generation,
    };
    let browser = Arc::new(BrowserLaunch::default());
    run_flow(name, stdout, stderr, cancelled, &child_slot, &browser)
}

fn try_register(
    name: &str,
    child_slot: Arc<Mutex<Option<std::process::Child>>>,
    cancelled: Arc<AtomicBool>,
    generation: u64,
) -> bool {
    pending()
        .lock()
        .ok()
        .is_some_and(|mut pending| match pending.entry(name.to_string()) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ChildHandle {
                    child: child_slot,
                    cancelled,
                    generation,
                });
                true
            }
        })
}

struct PendingGuard {
    name: String,
    generation: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        take_pending(&self.name, self.generation);
    }
}

fn run_flow(
    name: &str,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    cancelled: Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<std::process::Child>>>,
    browser: &Arc<BrowserLaunch>,
) -> anyhow::Result<()> {
    let opened = Arc::new(AtomicBool::new(false));
    let stdout_buf = spawn_output_reader(stdout, opened.clone(), browser.clone());
    let stderr_buf = spawn_output_reader(stderr, opened.clone(), browser.clone());
    let status = wait_for_child(child_slot, &cancelled, browser)?;
    let stdout_text = stdout_buf.join().unwrap_or_default();
    let stderr_text = stderr_buf.join().unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    if cancelled.load(Ordering::SeqCst) {
        bail!("browser sign-in for {name} was cancelled");
    }
    let detail = first_nonempty_line(&stderr_text)
        .or_else(|| first_nonempty_line(&stdout_text))
        .unwrap_or("browser sign-in did not finish");
    bail!("{detail}");
}

/// Outcome of trying to open the CLI-printed authorization URL. The reader
/// threads fill it in; the waiter turns a failure into an immediate error
/// (instead of a silent six-minute stall) and a timeout into a message that
/// still carries the URL.
#[derive(Default)]
struct BrowserLaunch {
    url: Mutex<Option<String>>,
    error: Mutex<Option<String>>,
}

impl BrowserLaunch {
    fn record(&self, url: &str, result: io::Result<()>) {
        match result {
            Ok(()) => {
                if let Ok(mut slot) = self.url.lock() {
                    *slot = Some(url.to_string());
                }
            }
            Err(error) => {
                if let Ok(mut slot) = self.error.lock() {
                    *slot = Some(format!("could not open {url}: {error}"));
                }
            }
        }
    }

    fn failure(&self) -> Option<String> {
        self.error.lock().ok().and_then(|slot| slot.clone())
    }

    fn success_url(&self) -> Option<String> {
        self.url.lock().ok().and_then(|slot| slot.clone())
    }
}

fn spawn_output_reader<R>(
    pipe: Option<R>,
    opened: Arc<AtomicBool>,
    browser: Arc<BrowserLaunch>,
) -> thread::JoinHandle<String>
where
    R: io::Read + Send + 'static,
{
    thread::spawn(move || {
        let Some(pipe) = pipe else {
            return String::new();
        };
        read_and_open_urls(BufReader::new(pipe), &opened, &browser)
    })
}

fn read_and_open_urls(
    reader: impl BufRead,
    opened: &AtomicBool,
    browser: &BrowserLaunch,
) -> String {
    let mut collected = String::new();
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        if !collected.is_empty() {
            collected.push('\n');
        }
        collected.push_str(&line);
        if let Some(url) = https_url_in(&line)
            && !opened.swap(true, Ordering::SeqCst)
        {
            browser.record(url, open_browser(url));
        }
    }
    collected
}

fn wait_for_child(
    child_slot: &Arc<Mutex<Option<std::process::Child>>>,
    cancelled: &AtomicBool,
    browser: &BrowserLaunch,
) -> anyhow::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + AUTHENTICATE_TIMEOUT;
    loop {
        let polled = child_slot
            .lock()
            .ok()
            .and_then(|mut slot| slot.as_mut().and_then(|child| child.try_wait().ok()))
            .flatten();
        match polled {
            Some(status) => return Ok(status),
            None => {
                if cancelled.load(Ordering::SeqCst) {
                    kill_child(child_slot);
                    bail!("browser sign-in was cancelled");
                }
                if let Some(message) = browser.failure() {
                    kill_child(child_slot);
                    bail!("{message}");
                }
                if Instant::now() >= deadline {
                    kill_child(child_slot);
                    match browser.success_url() {
                        Some(url) => {
                            bail!("timed out waiting for browser sign-in; open {url} to finish")
                        }
                        None => bail!("timed out waiting for browser sign-in"),
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn https_url_in(line: &str) -> Option<&str> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches(['.', ',', ';', ')']);
    (url.len() > "https://".len()).then_some(url)
}

fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && https_url_in(line) != Some(*line))
}

fn open_browser(url: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let mut command = {
            #[cfg(target_os = "macos")]
            {
                std::process::Command::new("open")
            }
            #[cfg(not(target_os = "macos"))]
            {
                std::process::Command::new("xdg-open")
            }
        };
        command
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_url_in_picks_the_authorize_link() {
        let line = "https://auth.smithery.ai/Tavily/authorize?response_type=code&client_id=https%3A%2F%2Fopencode.ai";
        assert_eq!(https_url_in(line), Some(line));
        assert_eq!(https_url_in("Authorize tavily in your browser."), None);
    }

    fn register_fixture(name: &str) -> u64 {
        let _ = pending().lock().map(|mut pending| pending.remove(name));
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst);
        assert!(try_register(
            name,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            generation,
        ));
        generation
    }

    #[test]
    fn try_register_accepts_the_first_flow() {
        let name = "unit-test-mcp-register-idle";
        let generation = register_fixture(name);
        take_pending(name, generation);
    }

    #[test]
    fn try_register_rejects_a_second_flow() {
        let name = "unit-test-mcp-register-dup";
        let generation = register_fixture(name);
        assert!(!try_register(
            name,
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
            GENERATION.fetch_add(1, Ordering::SeqCst),
        ));
        take_pending(name, generation);
    }
}
