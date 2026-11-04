//! Claude Code's interactive `/usage`, translated from Magpie's
//! `internal/claudetui/{tui,screen,child_windows}.go` and
//! `internal/proc/batch_windows.go` (MIT; see
//! `docs/provider-quota-upstream-license.txt`). Only the dedicated process
//! and its terminal are used. As in Magpie, first-run screens and the trust
//! question for our own working directory are answered by the CLI itself.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

#[cfg(windows)]
mod screen;

#[derive(Clone)]
pub struct TuiRequest {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub remove_env: Vec<String>,
    pub extra_env: Vec<(String, String)>,
    pub workdir: PathBuf,
    pub timeout: Duration,
}

/// The captured panel, or a sign-in screen that the user must answer.
#[derive(Clone)]
pub struct TuiTranscript {
    pub text: String,
    pub saw_denied: bool,
}

pub const TUI_COLS: i16 = 200;
pub const TUI_ROWS: i16 = 50;
pub const TUI_WORK_DIR_NAME: &str = "fintwind-claude-work";

static USAGE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(session|week(?: \([^)]*\))?)").unwrap());
static USAGE_USED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([0-9.]+)%\s+used").unwrap());
static USAGE_RESET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Resets (.*?\))").unwrap());

/// Magpie's `usageText`: keep the last reading of each named window in its
/// original order, with resets and the overages notice intact.
pub fn usage_text(screen: &str) -> String {
    let text = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut names = Vec::new();
    let mut said = std::collections::HashMap::new();
    for chunk in text.split("Current ").skip(1) {
        let (Some(name), Some(used)) = (USAGE_NAME.find(chunk), USAGE_USED.captures(chunk)) else {
            continue;
        };
        let name = name.as_str();
        let mut line = format!("Current {name}: {}% used", &used[1]);
        if let Some(reset) = USAGE_RESET.captures(&chunk[used.get(0).unwrap().start()..]) {
            line.push_str(" · resets ");
            line.push_str(&reset[1]);
        }
        if !said.contains_key(name) {
            names.push(name);
        }
        said.insert(name, line);
    }
    let mut lines = names
        .into_iter()
        .map(|name| said.remove(name).unwrap())
        .collect::<Vec<_>>();
    if text.contains("using your overages") {
        lines.push("You are currently using your overages to power your Claude Code usage.".into());
    }
    if lines.is_empty() {
        text
    } else {
        lines.join("\n")
    }
}

#[cfg(windows)]
pub fn spawn_tui_session(req: &TuiRequest) -> Option<TuiTranscript> {
    let session = uuid::Uuid::new_v4().to_string();
    let mut req = req.clone();
    req.args.extend(["--session-id".into(), session.clone()]);
    let reading = windows_driver::run(&req);
    windows_driver::drop_session(&req.workdir, &session);
    reading
}

/// The desktop's ConPTY adapter is Windows-only.
#[cfg(not(windows))]
pub fn spawn_tui_session(_req: &TuiRequest) -> Option<TuiTranscript> {
    None
}

#[cfg(windows)]
mod windows_driver {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows_sys::Win32::System::Console::{
        COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    };

    use super::screen::Grid;
    use super::*;

    const READY_WAIT: Duration = Duration::from_secs(45);
    const COMMAND_WAIT: Duration = Duration::from_secs(20);
    const SETTLE: Duration = Duration::from_secs(1);
    const TRUST_SETTLE: Duration = Duration::from_millis(300);

    static ANSI: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[P^_][^\x1b]*\x1b\\|\x1b.").unwrap()
    });
    static TRUST_NO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"❯\s*No,\s*exit").unwrap());
    static TRUST_YES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"❯\s*Yes,\s*I trust").unwrap());
    static SIGN_IN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"Select login method|Log in with your Claude account|claude\.ai/oauth/authorize",
        )
        .unwrap()
    });
    static FIRST_RUN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Security notes:.*Press Enter to continue").unwrap());
    static TERMINAL_SETUP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"terminal setup\?.*Esc to skip").unwrap());
    static USAGE_PANEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\bSettings\b.*\bUsage\b").unwrap());
    static USAGE_LOADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bloading\b").unwrap());

    struct Handles {
        process: HANDLE,
        job: HANDLE,
    }

    struct Input {
        handle: usize,
        lock: Mutex<()>,
    }

    impl Input {
        fn write(&self, mut bytes: &[u8]) -> Option<()> {
            let _guard = self.lock.lock().unwrap();
            while !bytes.is_empty() {
                let mut written = 0;
                let ok = unsafe {
                    WriteFile(
                        self.handle as HANDLE,
                        bytes.as_ptr(),
                        bytes.len() as u32,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || written == 0 {
                    return None;
                }
                bytes = &bytes[written as usize..];
            }
            Some(())
        }
    }

    struct Screen {
        raw: Vec<u8>,
        grid: Grid,
        panel: String,
        watch: bool,
    }

    impl Screen {
        fn new() -> Self {
            Self {
                raw: Vec::new(),
                grid: Grid::new(TUI_COLS as usize, TUI_ROWS as usize),
                panel: String::new(),
                watch: false,
            }
        }

        fn write(&mut self, bytes: &[u8]) {
            self.raw.extend_from_slice(bytes);
            if self.raw.len() > 256 << 10 {
                self.raw.drain(..self.raw.len() - (128 << 10));
            }
            self.grid.write(bytes);
            if self.watch {
                let text = self.grid.text();
                if USAGE_USED.is_match(&text) {
                    self.panel = text;
                }
            }
        }

        fn plain(&self) -> String {
            ANSI.replace_all(&String::from_utf8_lossy(&self.raw), " ")
                .into_owned()
        }

        fn clear(&mut self) {
            self.raw.clear();
            self.panel.clear();
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn env_block(req: &TuiRequest) -> Vec<u16> {
        let mut block = Vec::new();
        for (key, value) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if req
                .remove_env
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&key))
                || req
                    .extra_env
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(&key))
                || key.eq_ignore_ascii_case("TERM")
            {
                continue;
            }
            block.extend(wide(&format!("{key}={}", value.to_string_lossy())));
        }
        for (key, value) in &req.extra_env {
            block.extend(wide(&format!("{key}={value}")));
        }
        block.extend(wide("TERM=xterm-256color"));
        block.push(0);
        block
    }

    /// Magpie's `proc/batch_windows.go`: preserve npm shim arguments as
    /// cmd.exe reads them, including the JSON settings and empty values.
    fn quote_batch_arg(arg: &str) -> String {
        let quote = arg.is_empty()
            || arg.ends_with('\\')
            || arg.chars().any(|c| {
                if c.is_ascii() {
                    !c.is_ascii_alphanumeric() && !r"#$*+-./:?@\_".contains(c)
                } else {
                    c.is_control()
                }
            });
        let mut out = String::new();
        if quote {
            out.push('"');
        }
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => {
                    backslashes += 1;
                    out.push(c);
                    continue;
                }
                '"' => {
                    out.push_str(&"\\".repeat(backslashes));
                    out.push('"');
                }
                '%' => out.push_str("%%cd:~,"),
                _ => {}
            }
            backslashes = 0;
            out.push(c);
        }
        if quote {
            out.push_str(&"\\".repeat(backslashes));
            out.push('"');
        }
        out
    }

    /// Windows' native argument quoting, as used by ComposeCommandLine.
    fn quote_arg(arg: &str) -> String {
        let mut out = String::from("\"");
        let mut backslashes = 0;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
                continue;
            }
            out.push_str(&"\\".repeat(backslashes * if c == '"' { 2 } else { 1 }));
            if c == '"' {
                out.push('\\');
            }
            out.push(c);
            backslashes = 0;
        }
        out.push_str(&"\\".repeat(backslashes * 2));
        out.push('"');
        out
    }

    fn command_line(req: &TuiRequest) -> Option<(Vec<u16>, Vec<u16>)> {
        let program = req.program.to_string_lossy();
        if program.contains(['"', '\r', '\n', '\0'])
            || req.args.iter().any(|arg| arg.contains(['\r', '\n', '\0']))
        {
            return None;
        }
        let extension = req
            .program
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
            let shell = std::env::var_os("ComSpec")
                .map(PathBuf::from)
                .filter(|path| {
                    path.is_absolute()
                        && path.file_name().is_some_and(|name| {
                            name.to_string_lossy().eq_ignore_ascii_case("cmd.exe")
                        })
                })
                .unwrap_or_else(|| {
                    Path::new(
                        &std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
                    )
                    .join("System32")
                    .join("cmd.exe")
                });
            let shell = shell.to_string_lossy();
            let mut line = format!("\"{shell}\" /d /e:ON /v:OFF /s /c \"\"{program}\"");
            for arg in &req.args {
                line.push(' ');
                line.push_str(&quote_batch_arg(arg));
            }
            line.push('"');
            Some((wide(&shell), wide(&line)))
        } else {
            let mut line = quote_arg(&program);
            for arg in &req.args {
                line.push(' ');
                line.push_str(&quote_arg(arg));
            }
            Some((wide(&program), wide(&line)))
        }
    }

    pub(super) fn run(req: &TuiRequest) -> Option<TuiTranscript> {
        std::fs::create_dir_all(&req.workdir).ok()?;
        let (mut in_read, mut in_write, mut out_read, mut out_write) = (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        unsafe {
            if CreatePipe(&mut in_read, &mut in_write, std::ptr::null(), 0) == 0 {
                return None;
            }
            if CreatePipe(&mut out_read, &mut out_write, std::ptr::null(), 0) == 0 {
                CloseHandle(in_read);
                CloseHandle(in_write);
                return None;
            }
            let mut console = 0;
            let result = CreatePseudoConsole(
                COORD {
                    X: TUI_COLS,
                    Y: TUI_ROWS,
                },
                in_read,
                out_write,
                0,
                &mut console,
            );
            CloseHandle(in_read);
            CloseHandle(out_write);
            if result != 0 {
                CloseHandle(in_write);
                CloseHandle(out_read);
                return None;
            }
            let Some(handles) = setup_process(req, console) else {
                ClosePseudoConsole(console);
                CloseHandle(in_write);
                CloseHandle(out_read);
                return None;
            };
            run_session(req, handles, console, in_write, out_read)
        }
    }

    fn setup_process(req: &TuiRequest, console: HPCON) -> Option<Handles> {
        let (application, mut line) = command_line(req)?;
        let workdir = wide(&req.workdir.to_string_lossy());
        let env = env_block(req);
        unsafe {
            let mut size = 0;
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
            if size == 0 {
                return None;
            }
            let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            let attrs = storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
            if InitializeProcThreadAttributeList(attrs, 1, 0, &mut size) == 0 {
                return None;
            }
            struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);
            impl Drop for Attributes {
                fn drop(&mut self) {
                    unsafe { DeleteProcThreadAttributeList(self.0) };
                }
            }
            let _attributes = Attributes(attrs);
            if UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const core::ffi::c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.lpAttributeList = attrs;
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            struct Job(HANDLE);
            impl Drop for Job {
                fn drop(&mut self) {
                    unsafe { CloseHandle(self.0) };
                }
            }
            let job = Job(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return None;
            }
            let mut process: PROCESS_INFORMATION = std::mem::zeroed();
            // Contain the tree before any child code can run. Cleanup only
            // ever uses this call's own process and exclusive job handles.
            if CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                env.as_ptr() as *const _,
                workdir.as_ptr(),
                &startup.StartupInfo,
                &mut process,
            ) == 0
            {
                return None;
            }
            if AssignProcessToJobObject(job.0, process.hProcess) == 0
                || ResumeThread(process.hThread) == u32::MAX
            {
                TerminateProcess(process.hProcess, 1);
                CloseHandle(process.hThread);
                CloseHandle(process.hProcess);
                return None;
            }
            CloseHandle(process.hThread);
            let handles = Handles {
                process: process.hProcess,
                job: job.0,
            };
            std::mem::forget(job);
            Some(handles)
        }
    }

    fn process_alive(process: HANDLE) -> bool {
        unsafe { WaitForSingleObject(process, 0) == WAIT_TIMEOUT }
    }

    fn run_session(
        req: &TuiRequest,
        handles: Handles,
        console: HPCON,
        in_write: HANDLE,
        out_read: HANDLE,
    ) -> Option<TuiTranscript> {
        let screen = Arc::new(Mutex::new(Screen::new()));
        let input = Arc::new(Input {
            handle: in_write as usize,
            lock: Mutex::new(()),
        });
        let reader_screen = screen.clone();
        let reader_input = input.clone();
        let reader_handle = out_read as usize;
        let reader = std::thread::spawn(move || {
            let mut bytes = [0u8; 64 << 10];
            loop {
                let mut read = 0;
                let ok = unsafe {
                    ReadFile(
                        reader_handle as HANDLE,
                        bytes.as_mut_ptr(),
                        bytes.len() as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || read == 0 {
                    break;
                }
                let bytes = &bytes[..read as usize];
                if bytes.windows(4).any(|b| b == b"\x1b[6n") {
                    let _ = reader_input.write(b"\x1b[1;1R");
                }
                if bytes.windows(3).any(|b| b == b"\x1b[c")
                    || bytes.windows(4).any(|b| b == b"\x1b[0c")
                {
                    let _ = reader_input.write(b"\x1b[?62;c");
                }
                reader_screen.lock().unwrap().write(bytes);
            }
        });
        let outcome = drive(req, &handles, &input, &screen);
        // Magpie closes the console asynchronously: on older Windows the
        // close waits for output to drain, so keep the reader alive for it.
        let closer = std::thread::spawn(move || unsafe { ClosePseudoConsole(console) });
        unsafe {
            if WaitForSingleObject(handles.process, 3000) == WAIT_TIMEOUT {
                TerminateProcess(handles.process, 1);
            }
            // Close the exclusive job before joining: descendants must not
            // keep the terminal pipes open while the reader waits for EOF.
            CloseHandle(handles.job);
            WaitForSingleObject(handles.process, 5000);
        }
        let _ = closer.join();
        let _ = reader.join();
        unsafe {
            CloseHandle(handles.process);
            CloseHandle(in_write);
            CloseHandle(out_read);
        }
        outcome
    }

    fn onboard(
        name: &'static str,
        key: &[u8],
        shown: &mut HashMap<&'static str, Instant>,
        pressed: &mut HashMap<&'static str, usize>,
        input: &Input,
        screen: &Mutex<Screen>,
    ) -> Option<()> {
        if shown.entry(name).or_insert_with(Instant::now).elapsed() < TRUST_SETTLE {
            return Some(());
        }
        let count = pressed.entry(name).or_default();
        if *count >= 3 {
            return None;
        }
        *count += 1;
        shown.remove(name);
        screen.lock().unwrap().clear();
        input.write(key)
    }

    fn drive(
        req: &TuiRequest,
        handles: &Handles,
        input: &Input,
        screen: &Mutex<Screen>,
    ) -> Option<TuiTranscript> {
        let started = Instant::now();
        let mut answered = 0;
        let mut yes_at: Option<Instant> = None;
        let mut down_at: Option<Instant> = None;
        let mut shown = HashMap::new();
        let mut pressed = HashMap::new();
        loop {
            let plain = screen.lock().unwrap().plain();
            let words = plain.split_whitespace().collect::<Vec<_>>().join(" ");
            if SIGN_IN.is_match(&plain) {
                return Some(TuiTranscript {
                    text: plain,
                    saw_denied: true,
                });
            } else if FIRST_RUN.is_match(&words) {
                onboard("notes", b"\r", &mut shown, &mut pressed, input, screen)?;
            } else if TERMINAL_SETUP.is_match(&words) {
                onboard("terminal", b"\x1b", &mut shown, &mut pressed, input, screen)?;
            } else if plain.contains("trust this folder") && answered < 3 {
                let no = TRUST_NO.find_iter(&plain).last().map(|m| m.start());
                let yes = TRUST_YES.find_iter(&plain).last().map(|m| m.start());
                if yes > no {
                    if yes_at.get_or_insert_with(Instant::now).elapsed() >= TRUST_SETTLE {
                        answered += 1;
                        yes_at = None;
                        screen.lock().unwrap().clear();
                        input.write(b"\r")?;
                    }
                } else if no.is_some() {
                    yes_at = None;
                    if down_at.is_none_or(|at| at.elapsed() >= SETTLE) {
                        down_at = Some(Instant::now());
                        input.write(b"\x1b[B")?;
                    }
                }
            } else if plain.contains("keep browser tools off") {
                screen.lock().unwrap().clear();
                input.write(b"\x1b")?;
            } else if words.contains("Choose the text style") {
                onboard("theme", b"\r", &mut shown, &mut pressed, input, screen)?;
            } else if plain.contains("❯\u{a0}") {
                // This usage-only session has no MCP tools to wait for.
                std::thread::sleep(TRUST_SETTLE);
                break;
            }
            if !process_alive(handles.process) || started.elapsed() >= READY_WAIT.min(req.timeout) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        for c in "/usage".chars() {
            input.write(c.to_string().as_bytes())?;
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(SETTLE);
        screen.lock().unwrap().clear();
        input.write(b"\r")?;
        screen.lock().unwrap().watch = true;

        let panel_started = Instant::now();
        let mut quiet = Instant::now();
        let mut last = String::new();
        let mut retried = false;
        let mut drawn = false;
        let (plain, now, panel) = loop {
            let (plain, now, panel) = {
                let screen = screen.lock().unwrap();
                (screen.plain(), screen.grid.text(), screen.panel.clone())
            };
            let current = format!("{plain}{now}");
            if current != last {
                last = current;
                quiet = Instant::now();
            }
            let still = quiet.elapsed();
            let loading = USAGE_LOADING.is_match(&now);
            let failed = now.contains("Failed to load usage data");
            let figures = !panel.is_empty() || USAGE_USED.is_match(&plain);
            if loading {
                drawn = figures && still >= 3 * SETTLE;
            } else if failed && !figures && !retried && still >= SETTLE {
                input.write(b"r")?;
                retried = true;
                quiet = Instant::now();
            } else if (figures && still >= SETTLE)
                || ((failed || usage_drawn(&plain) || usage_drawn(&now)) && still >= 3 * SETTLE)
            {
                drawn = true;
            }
            if drawn || panel_started.elapsed() >= COMMAND_WAIT || started.elapsed() >= req.timeout
            {
                break (plain, now, panel);
            }
            if !process_alive(handles.process) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        screen.lock().unwrap().watch = false;
        let _ = input.write(b"\x1b");
        if !drawn
            || (!USAGE_USED.is_match(&format!("{panel}{plain}"))
                && (USAGE_LOADING.is_match(&now) || now.contains("Failed to load usage data")))
        {
            return None;
        }
        Some(TuiTranscript {
            text: if panel.is_empty() { plain } else { panel },
            saw_denied: false,
        })
    }

    fn usage_drawn(screen: &str) -> bool {
        let words = screen.split_whitespace().collect::<Vec<_>>().join(" ");
        USAGE_PANEL.is_match(&words) && !USAGE_LOADING.is_match(&words)
    }

    /// Magpie's `claudeSessions` / `removeSession`: remove only this run's
    /// fresh session ID, never another CLI session or the shared project.
    pub(super) fn drop_session(workdir: &Path, session: &str) {
        let Some(config) = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        else {
            return;
        };
        let mut paths = vec![workdir.to_owned()];
        if let Ok(canonical) = std::fs::canonicalize(workdir) {
            let canonical = canonical.to_string_lossy();
            let path = PathBuf::from(canonical.strip_prefix(r"\\?\").unwrap_or(&canonical));
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        for path in paths {
            let project = path
                .to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>();
            let directory = config.join("projects").join(project);
            let _ = std::fs::remove_file(directory.join(format!("{session}.jsonl")));
            let _ = std::fs::remove_dir_all(directory.join(session));
        }
    }
}
