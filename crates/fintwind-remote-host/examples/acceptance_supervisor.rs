//! Controlled process supervisor for the acceptance chain — the ONLY owner
//! of every process an acceptance run needs.
//!
//! Containment contract (incident rules, AGENTS.md both repos):
//!
//! * Every child is created with `CreateProcessW(CREATE_SUSPENDED)` and is
//!   assigned to this process's exclusive job object **by the creation
//!   handle** (`hProcess`), before its first instruction runs. There is no
//!   window in which a child — or any grandchild it spawns — exists
//!   outside the job, and no `OpenProcess(pid)` anywhere: a reused pid can
//!   never be mistaken for ownership.
//! * `ResumeThread` starts the child only after containment succeeded.
//!   Any failure before that terminates the child through the same
//!   creation handle and waits for it. Nothing is ever scanned, matched or
//!   adopted: a process not created into the job is nobody's to kill.
//! * Teardown is handle-based: the deadline path calls
//!   `TerminateJobObject` on the job this process owns; every other path
//!   drops the job, whose `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` does the
//!   same. `childrenTerminatedByJob` is only reported true after the job
//!   reports zero active processes **and** the root child's handle has
//!   been waited on within a strict deadline — closing a handle is not
//!   proof that anything exited.
//! * Child output is never mirrored to disk. Readiness is parsed in
//!   memory from the exact pipe this process holds (bounded), verified
//!   against the child's own pid, the loopback address and the protocol
//!   version, and only booleans reach the status file. stderr is counted
//!   in memory and discarded.
//! * Status writes are part of the verdict: a status file that cannot be
//!   written fails the run. Any plan, spawn or readiness failure writes a
//!   final failure event through the same teardown and stops — later
//!   side-effect steps never run after a failed step.
//!
//! Input: a plan file (JSON), frozen at start.
//!
//! ```json
//! {"steps": [
//!   {"tag": "daemon", "mode": "spawn", "program": "daemon.exe",
//!    "args": ["--bind", "{listen}"], "cwd": "C:/run",
//!    "env": {"PATH": "..."}, "readyFromLog": true},
//!   {"tag": "seed", "mode": "run", "program": "seed.exe",
//!    "args": ["{daemonAddress}"],
//!    "outputSubstitute": {"file": "fixture.json",
//!                         "placeholder": "seedProjectId",
//!                         "field": "projectId"}}
//! ]}
//! ```
//!
//! Placeholders: `{daemonAddress}` is resolved from the daemon's readiness
//! line (in memory), `{<placeholder>}` entries from a step's
//! `outputSubstitute` file. A step may also carry `stdinText`.
//!
//! Output: a status JSONL — tags, booleans, exit codes, durations, the
//! containment verdict. No addresses, no tokens, no raw child text.
//!
//! Windows-only by design: the containment guarantee is a job object, and
//! a degraded fallback would be the unlocked cleanup the incident forbids.
//!
//! Usage:
//!   acceptance_supervisor --plan plan.json --status status.jsonl \
//!     --total-deadline-ms 240000
//!   [--observer-handle V --observer-nonce HEX
//!    --observation-request V --observation-response V]
//!
//! The observation arguments are optional. With none, the supervisor's
//! behaviour is exactly what it was before observation existed. With them,
//! the supervisor hands the observer a real handle for every root child —
//! after containment, before the child's first instruction — and refuses to
//! resume a child whose hand-off the observer refused, and it reports the
//! frozen topology (the exact job query handle, then the freeze marker)
//! before anything is torn down. A broken channel — an observer that died
//! mid-run — is treated as a failure: the job is terminated explicitly,
//! because a job handle copy the observer may still hold would otherwise
//! delay kill-on-close past this process's teardown.

#[cfg(windows)]
#[path = "support/acceptance_observation.rs"]
mod observation;

#[cfg(windows)]
#[path = "support/acceptance_debug_tree.rs"]
mod debug_tree;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

#[cfg(windows)]
mod job {
    use anyhow::{Context as _, Result};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
    };

    /// The acceptance's exclusive job. Termination and verification are
    /// handle-based end to end.
    pub struct Job(HANDLE);

    // SAFETY: job object handles are usable from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error())
                        .context("could not create the acceptance job object");
                }
                let job = Self(handle);
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error())
                        .context("could not configure the acceptance job object");
                }
                Ok(job)
            }
        }

        /// Assign by the creation handle — never by opening a pid.
        pub fn assign_handle(&self, process: HANDLE) -> Result<()> {
            unsafe {
                if AssignProcessToJobObject(self.0, process) == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not assign an acceptance child to the job");
                }
                Ok(())
            }
        }

        /// The job's own handle, for duplication to the observer. The copy
        /// the observer receives is narrowed at the duplication site; this
        /// handle never leaves this process.
        pub fn handle(&self) -> HANDLE {
            self.0
        }

        pub fn terminate(&self) -> Result<()> {
            unsafe {
                if TerminateJobObject(self.0, 1) == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not terminate the acceptance job");
                }
                Ok(())
            }
        }

        /// True only when the job reports no active processes.
        pub fn active_processes_is_zero(&self) -> Result<bool> {
            unsafe {
                let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
                if QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    &mut accounting as *mut _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                ) == 0
                {
                    return Err(std::io::Error::last_os_error())
                        .context("could not query the acceptance job accounting");
                }
                Ok(accounting.ActiveProcesses == 0)
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Wait helpers over raw creation handles.
    pub mod wait {
        use anyhow::{Context as _, Result};
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{GetExitCodeProcess, WaitForSingleObject},
        };

        pub const WAIT_TIMEOUT: u32 = 0x0000_0102;

        /// Wait a bounded time for the process to exit; `None` on timeout.
        pub fn wait_handle(process: HANDLE, timeout_ms: u32) -> Result<Option<u32>> {
            unsafe {
                let wait = WaitForSingleObject(process, timeout_ms);
                if wait == WAIT_OBJECT_0 {
                    let mut code: u32 = 0;
                    if GetExitCodeProcess(process, &mut code) == 0 {
                        return Err(std::io::Error::last_os_error())
                            .context("could not read an acceptance child exit code");
                    }
                    Ok(Some(code))
                } else if wait == WAIT_TIMEOUT {
                    Ok(None)
                } else {
                    Err(std::io::Error::last_os_error())
                        .context("could not wait for an acceptance child")
                }
            }
        }

        pub fn close(handle: HANDLE) {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

#[cfg(windows)]
mod create {
    use anyhow::{Context as _, Result, bail};
    use std::collections::BTreeMap;
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    pub const CREATE_SUSPENDED_FLAG: u32 = CREATE_SUSPENDED;

    /// One created child, held for its whole lifetime. Handles come from
    /// `CreateProcessW` itself, so ownership never depends on a pid.
    pub struct ChildGuard {
        pub pid: u32,
        pub process: HANDLE,
        pub thread: HANDLE,
    }

    impl ChildGuard {
        pub fn resume(&self) -> Result<()> {
            unsafe {
                if ResumeThread(self.thread) == u32::MAX {
                    return Err(std::io::Error::last_os_error())
                        .context("could not resume a suspended acceptance child");
                }
                Ok(())
            }
        }

        /// Terminate through the creation handle and wait for the exit.
        pub fn kill_and_wait(&self, timeout_ms: u32) -> Result<()> {
            unsafe {
                if TerminateProcess(self.process, 1) == 0 {
                    // Already exited is fine; anything else is an error.
                    let wait = WaitForSingleObject(self.process, 0);
                    if wait != WAIT_OBJECT_0 {
                        return Err(std::io::Error::last_os_error())
                            .context("could not terminate an acceptance child");
                    }
                    return Ok(());
                }
                let wait = WaitForSingleObject(self.process, timeout_ms);
                if wait != WAIT_OBJECT_0 {
                    bail!("an acceptance child did not exit after termination");
                }
                Ok(())
            }
        }

        pub fn exit_code(&self) -> Result<u32> {
            unsafe {
                let mut code: u32 = 0;
                if GetExitCodeProcess(self.process, &mut code) == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not read an acceptance child exit code");
                }
                Ok(code)
            }
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = TerminateProcess(self.process, 1);
                // Bounded waits and their proof belong to run_steps. Drop
                // must not add another timeout for every unresolved root.
                CloseHandle(self.thread);
                CloseHandle(self.process);
            }
        }
    }

    pub struct Pipes {
        /// The parent's ends.
        pub stdin_write: HANDLE,
        pub stdout_read: HANDLE,
        pub stderr_read: HANDLE,
        /// The child's ends, handed to CreateProcessW and closed in the
        /// parent right after creation.
        pub stdin_read: HANDLE,
        pub stdout_write: HANDLE,
        pub stderr_write: HANDLE,
    }

    impl Drop for Pipes {
        /// RAII for partial-failure paths: every end this struct still
        /// owns is closed. Ends transferred elsewhere (a drain thread, or
        /// consumed by a write) are nulled out first, so nothing is closed
        /// twice.
        fn drop(&mut self) {
            for handle in [
                self.stdin_write,
                self.stdout_read,
                self.stderr_read,
                self.stdin_read,
                self.stdout_write,
                self.stderr_write,
            ] {
                if !handle.is_null() {
                    unsafe {
                        CloseHandle(handle);
                    }
                }
            }
        }
    }

    /// Create one pipe whose ends both start non-inheritable; inheritance
    /// is granted afterwards, per end, with `SetHandleInformation` — only
    /// on exactly the three handles the child's STARTUPINFO names.
    fn create_pipe_exact() -> Result<(HANDLE, HANDLE)> {
        unsafe {
            let mut read_end: HANDLE = std::ptr::null_mut();
            let mut write_end: HANDLE = std::ptr::null_mut();
            if CreatePipe(&mut read_end, &mut write_end, std::ptr::null(), 0) == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("could not create an acceptance pipe");
            }
            Ok((read_end, write_end))
        }
    }

    fn grant_inherit(handle: HANDLE) -> Result<()> {
        unsafe {
            const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("could not mark an acceptance pipe end inheritable");
            }
            Ok(())
        }
    }

    pub fn create_pipes() -> Result<Pipes> {
        let (stdin_read, stdin_write) = create_pipe_exact()?;
        let (stdout_read, stdout_write) = match create_pipe_exact() {
            Ok(pair) => pair,
            Err(error) => {
                unsafe {
                    CloseHandle(stdin_read);
                    CloseHandle(stdin_write);
                }
                return Err(error);
            }
        };
        let (stderr_read, stderr_write) = match create_pipe_exact() {
            Ok(pair) => pair,
            Err(error) => {
                unsafe {
                    CloseHandle(stdin_read);
                    CloseHandle(stdin_write);
                    CloseHandle(stdout_read);
                    CloseHandle(stdout_write);
                }
                return Err(error);
            }
        };
        // These ends are eligible for inheritance; the creation attribute
        // list below restricts inheritance to exactly these three handles.
        if let Err(error) = grant_inherit(stdin_read)
            .and_then(|_| grant_inherit(stdout_write))
            .and_then(|_| grant_inherit(stderr_write))
        {
            unsafe {
                CloseHandle(stdin_read);
                CloseHandle(stdin_write);
                CloseHandle(stdout_read);
                CloseHandle(stdout_write);
                CloseHandle(stderr_read);
                CloseHandle(stderr_write);
            }
            return Err(error);
        }
        Ok(Pipes {
            stdin_write,
            stdout_read,
            stderr_read,
            stdin_read,
            stdout_write,
            stderr_write,
        })
    }

    /// Quote one argument by the Windows command-line rules: backslashes
    /// before a quote are doubled, quotes are escaped, and the argument is
    /// always wrapped in quotes.
    fn quote(argument: &str) -> String {
        let mut quoted = String::with_capacity(argument.len() + 3);
        quoted.push('"');
        let mut backslashes = 0_usize;
        for character in argument.chars() {
            if character == '\\' {
                backslashes += 1;
                quoted.push('\\');
            } else if character == '"' {
                for _ in 0..=backslashes {
                    quoted.push('\\');
                }
                quoted.push('"');
                backslashes = 0;
            } else {
                backslashes = 0;
                quoted.push(character);
            }
        }
        for _ in 0..backslashes {
            quoted.push('\\');
        }
        quoted.push('"');
        quoted
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// An environment block: sorted `NAME=value` pairs, double-null ended.
    fn environment_block(env: &BTreeMap<String, String>) -> Vec<u16> {
        let mut block: Vec<u16> = Vec::new();
        for (name, value) in env {
            block.extend(format!("{name}={value}").encode_utf16());
            block.push(0);
        }
        block.push(0);
        block
    }

    pub struct Created {
        pub guard: ChildGuard,
        pub pipes: Pipes,
    }

    // The opaque list needs suitably aligned storage, alive through creation.
    struct HandleAttributes {
        storage: Vec<usize>,
    }

    impl HandleAttributes {
        fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.storage.as_mut_ptr().cast()
        }

        fn new(handles: &mut [HANDLE]) -> Result<Self> {
            let mut size = 0;
            unsafe {
                InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
            }
            if size == 0 {
                bail!("could not size the acceptance handle list");
            }
            let mut list = Self {
                storage: vec![0; size.div_ceil(std::mem::size_of::<usize>())],
            };
            unsafe {
                if InitializeProcThreadAttributeList(list.pointer(), 1, 0, &mut size) == 0 {
                    // A failed initialization must not be passed to Delete.
                    list.storage.clear();
                    return Err(std::io::Error::last_os_error())
                        .context("could not initialize the acceptance handle list");
                }
                if UpdateProcThreadAttribute(
                    list.pointer(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_mut_ptr().cast(),
                    std::mem::size_of_val(handles),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) == 0
                {
                    return Err(std::io::Error::last_os_error())
                        .context("could not restrict acceptance handle inheritance");
                }
            }
            Ok(list)
        }
    }

    impl Drop for HandleAttributes {
        fn drop(&mut self) {
            if !self.storage.is_empty() {
                unsafe {
                    DeleteProcThreadAttributeList(self.pointer());
                }
            }
        }
    }

    /// Create the child suspended, with this run's pipes as its stdio.
    ///
    /// `extra_inherit` extends the explicit `HANDLE_LIST` beyond the three
    /// stdio ends. It is empty for every ordinary step, so the ordinary
    /// inheritance contract is unchanged: an unobserved step inherits
    /// exactly its stdio and nothing else. Only an observing step receives
    /// the observer handle and its two channel ends, and those values are
    /// marked inheritable by whoever created them, never opened from a pid.
    #[allow(clippy::too_many_arguments)]
    pub fn create_suspended(
        program: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&std::path::Path>,
        stdin_text: bool,
        extra_inherit: &[HANDLE],
        debug_descendants: bool,
    ) -> Result<Created> {
        let mut command_line = String::from(&quote(program));
        for argument in args {
            command_line.push(' ');
            command_line.push_str(&quote(argument));
        }
        let mut command_line_wide = wide(&command_line);
        let mut application_wide = wide(program);
        let mut cwd_wide = cwd.and_then(|path| Some(wide(&path.to_string_lossy())));
        let mut environment = environment_block(env);

        let mut pipes = create_pipes()?;
        let mut inherited = Vec::with_capacity(3 + extra_inherit.len());
        inherited.extend_from_slice(&[pipes.stdin_read, pipes.stdout_write, pipes.stderr_write]);
        inherited.extend_from_slice(extra_inherit);
        let mut attributes = HandleAttributes::new(&mut inherited)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags =
            STARTF_USESTDHANDLES | windows_sys::Win32::System::Threading::STARTF_USESHOWWINDOW;
        startup.StartupInfo.wShowWindow = 0; // SW_HIDE; preserve the inherited console.
        startup.StartupInfo.hStdInput = pipes.stdin_read;
        startup.StartupInfo.hStdOutput = pipes.stdout_write;
        startup.StartupInfo.hStdError = pipes.stderr_write;
        startup.lpAttributeList = attributes.pointer();
        let mut process_info = PROCESS_INFORMATION::default();

        unsafe {
            let created = CreateProcessW(
                application_wide.as_mut_ptr(),
                command_line_wide.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1, // inherit the child's pipe ends
                CREATE_SUSPENDED_FLAG
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT
                    | if debug_descendants {
                        windows_sys::Win32::System::Threading::DEBUG_PROCESS
                    } else {
                        0
                    },
                environment.as_mut_ptr() as *mut _,
                cwd_wide
                    .as_mut()
                    .map(|wide| wide.as_mut_ptr())
                    .unwrap_or(std::ptr::null_mut()),
                &startup.StartupInfo,
                &mut process_info,
            );
            if created == 0 {
                let error = std::io::Error::last_os_error();
                return Err(error)
                    .with_context(|| format!("could not create the suspended child {program:?}"));
            }
            // The parent never needs the child's pipe ends; holding them
            // would keep EOF from ever arriving.
            CloseHandle(pipes.stdin_read);
            CloseHandle(pipes.stdout_write);
            CloseHandle(pipes.stderr_write);
            pipes.stdin_read = std::ptr::null_mut();
            pipes.stdout_write = std::ptr::null_mut();
            pipes.stderr_write = std::ptr::null_mut();
            let _ = stdin_text;
            Ok(Created {
                guard: ChildGuard {
                    pid: process_info.dwProcessId,
                    process: process_info.hProcess,
                    thread: process_info.hThread,
                },
                pipes,
            })
        }
    }

    /// Wait a bounded time on a creation handle; `None` on timeout.
    pub fn wait_handle(process: HANDLE, timeout_ms: u32) -> Result<Option<u32>> {
        unsafe {
            let wait = WaitForSingleObject(process, timeout_ms);
            if wait == WAIT_OBJECT_0 {
                let mut code: u32 = 0;
                if GetExitCodeProcess(process, &mut code) == 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("could not read an acceptance child exit code");
                }
                Ok(Some(code))
            } else if wait == WAIT_TIMEOUT {
                Ok(None)
            } else {
                Err(std::io::Error::last_os_error())
                    .context("could not wait for an acceptance child")
            }
        }
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("the acceptance supervisor requires Windows job containment");
    std::process::exit(3);
}

#[cfg(windows)]
fn main() -> Result<()> {
    supervisor_main()
}

/// The plan schema, shared by the runner that writes it and the supervisor
/// that executes it. Kept at module level so prevalidation and execution
/// see exactly one definition.
#[cfg(windows)]
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OutputSubstitute {
    file: PathBuf,
    placeholder: String,
    field: String,
}

#[cfg(windows)]
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    tag: String,
    mode: String,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    stdin_text: Option<String>,
    #[serde(default)]
    ready_from_log: bool,
    #[serde(default)]
    output_substitute: Option<OutputSubstitute>,
    /// This step is a trusted synthetic observing fixture: the supervisor
    /// creates a dedicated two-way channel pair for it, hands the observer
    /// its ends through an explicit `FixtureChannel` hand-off, and gives the
    /// fixture — and only the fixture — the observer handle, the subject
    /// pipe ends, the round nonce and a structurally unique fixture id
    /// through this step's own environment. Steps without this flag never
    /// see any of those.
    ///
    /// Only valid on a `run` step: the fixture creates its descendant,
    /// hands the handle over, and exits — the verified exit of the step is
    /// the handshake that says the descendant hand-off is complete, so the
    /// freeze that follows never has to be guessed from a sleep.
    #[serde(default)]
    observe_descendants: bool,
    /// Kernel-provided creation handles for a non-cooperating descendant tree.
    /// Only the final run step, with an independent observer, may use this.
    #[serde(default)]
    debug_descendants: bool,
}

#[cfg(windows)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    steps: Vec<Step>,
}

/// The observation arguments the runner passes on the command line. All
/// optional: absent means "no observation was requested", and every code
/// path behaves exactly as it did before observation existed.
#[cfg(windows)]
struct ObservationArgs {
    /// The observer's process handle, inherited narrowed to exactly
    /// `PROCESS_DUP_HANDLE`. A handle value in this process's table.
    observer: usize,
    /// The round's correlation token.
    nonce: String,
    /// This process's write end for hand-offs.
    request: usize,
    /// This process's read end for ACKs.
    response: usize,
}

/// What one observing step receives, once its channel is registered.
#[cfg(windows)]
struct FixtureGrant {
    // Keep source pipe ends alive until CreateProcess has inherited them.
    _channel: observation::Channel,
    /// This fixture's structural writer id — unique, monotonic per run.
    fixture_id: u32,
    /// The fixture's write end, inheritable, for its hand-offs.
    request_write: windows_sys::Win32::Foundation::HANDLE,
    /// The fixture's read end, inheritable, for ACKs.
    response_read: windows_sys::Win32::Foundation::HANDLE,
}

/// The environment keys through which an observing step learns its channel.
/// Handle *values* travel here; the handles themselves travel only through
/// the explicit, minimal `HANDLE_LIST`. A step without the flag never sees
/// any of these keys.
#[cfg(windows)]
mod observation_env {
    pub const HANDLE: &str = "FINTWIND_OBSERVATION_HANDLE";
    pub const NONCE: &str = "FINTWIND_OBSERVATION_NONCE";
    pub const REQUEST: &str = "FINTWIND_OBSERVATION_REQUEST";
    pub const RESPONSE: &str = "FINTWIND_OBSERVATION_RESPONSE";
    pub const FIXTURE_ID: &str = "FINTWIND_OBSERVATION_FIXTURE_ID";
}

/// The subject side of the observation channel, live during a run.
#[cfg(windows)]
struct Observation {
    channel: observation::SubjectChannel,
    /// The observer's process, for `DuplicateHandle` at the single
    /// auditable hand-off site.
    observer: windows_sys::Win32::Foundation::HANDLE,
    nonce: String,
    /// Monotonic per writer — this supervisor is one writer.
    next_transfer: u64,
    /// Monotonic across observing steps: every fixture gets a structurally
    /// unique writer id, so two fixtures can never be confused.
    next_fixture_id: u32,
}

#[cfg(windows)]
impl Observation {
    fn start(args: ObservationArgs) -> Result<Self> {
        use windows_sys::Win32::Foundation::HANDLE;
        let observer = args.observer as HANDLE;
        if observer.is_null() {
            bail!("the observation arguments carried a null observer handle");
        }
        let channel = observation::SubjectChannel::from_raw(
            observation::WriterId::supervisor(),
            args.request as HANDLE,
            args.response as HANDLE,
        )?;
        Ok(Self {
            channel,
            observer,
            nonce: args.nonce,
            next_transfer: 0,
            // Fixture writer ids start at one so that zero never names one.
            next_fixture_id: 1,
        })
    }

    /// One hand-off with the next monotonic transfer id.
    fn offer(
        &mut self,
        kind: observation::Kind,
        claimed_pid: u32,
        process_handle: Option<u64>,
        job_handle: Option<u64>,
        deadline: Instant,
    ) -> Result<bool> {
        self.next_transfer += 1;
        let message = observation::Handoff {
            protocol: observation::PROTOCOL.to_owned(),
            nonce: self.nonce.clone(),
            source: observation::Source::Supervisor,
            fixture: None,
            transfer_id: self.next_transfer,
            kind,
            claimed_pid,
            process_handle,
            job_handle,
            pipe_request_read: None,
            pipe_response_write: None,
            granted_access: "PROCESS_QUERY_LIMITED_INFORMATION|SYNCHRONIZE".to_owned(),
        };
        let ack = self.channel.handoff(&message, deadline)?;
        Ok(ack.accepted)
    }

    /// Duplicate a child's real creation handle for the observer — query
    /// and wait rights, never termination — and offer it. The access mask
    /// is enforced inside `duplicate_for_observer`; the label in the
    /// message records it, it does not grant it.
    fn report_root_child(
        &mut self,
        pid: u32,
        process: windows_sys::Win32::Foundation::HANDLE,
        deadline: Instant,
    ) -> Result<bool> {
        let duplicated = observation::duplicate_for_observer(
            self.observer,
            process,
            observation::OBSERVER_PROCESS_ACCESS,
        )?;
        self.offer(
            observation::Kind::RootChild,
            pid,
            Some(duplicated as u64),
            None,
            deadline,
        )
    }

    fn report_descendant(
        &mut self,
        pid: u32,
        process: windows_sys::Win32::Foundation::HANDLE,
        deadline: Instant,
    ) -> Result<bool> {
        let duplicated = observation::duplicate_for_observer(
            self.observer,
            process,
            observation::OBSERVER_PROCESS_ACCESS,
        )?;
        self.offer(
            observation::Kind::Descendant,
            pid,
            Some(duplicated as u64),
            None,
            deadline,
        )
    }

    /// Offer the exact job query handle, before the freeze marker.
    fn report_job_query(
        &mut self,
        job: windows_sys::Win32::Foundation::HANDLE,
        deadline: Instant,
    ) -> Result<bool> {
        let duplicated = observation::duplicate_for_observer(
            self.observer,
            job,
            0x0004, // JOB_OBJECT_QUERY; never grant job termination rights.
        )?;
        self.offer(
            observation::Kind::JobQuery,
            0,
            None,
            Some(duplicated as u64),
            deadline,
        )
    }

    /// The freeze marker: no further children will be created. Sent once,
    /// after every step ran.
    fn report_topology_frozen(&mut self, deadline: Instant) -> Result<bool> {
        self.offer(observation::Kind::TopologyFrozen, 0, None, None, deadline)
    }

    /// Create one observing fixture's channel pair and register it with the
    /// observer.
    ///
    /// Two anonymous pipes — the fixture's write side and the observer's
    /// read side, plus the reverse direction for ACKs. The observer's ends
    /// travel to it through an explicit `FixtureChannel` hand-off whose pipe
    /// fields are typed as pipes, never disguised as process handles. The
    /// fixture id is this supervisor's own monotonic counter, so two
    /// fixtures never share a writer identity, and the pair is created
    /// **before** the fixture process exists — the registration ACK is what
    /// lets the supervisor go on to create, contain, hand over and resume
    /// the fixture, in that order.
    fn grant_fixture_channel(&mut self, deadline: Instant) -> Result<FixtureGrant> {
        let fixture_id = self.next_fixture_id;
        self.next_fixture_id += 1;
        let channel = observation::Channel::create()?;
        // The fixture's two ends enter the fixture's HANDLE_LIST, so they
        // must be inheritable before the process is created.
        channel.mark_subject_ends_inheritable()?;
        // The observer's ends are duplicated for it at the single auditable
        // site, labelled explicitly as pipe ends in the hand-off.
        let pipe_request_read = observation::duplicate_pipe_end_for_observer(
            self.observer,
            channel
                .request_read()
                .ok_or_else(|| anyhow::anyhow!("the fixture channel has no observer read end"))?,
        )?;
        let pipe_response_write = observation::duplicate_pipe_end_for_observer(
            self.observer,
            channel
                .response_write()
                .ok_or_else(|| anyhow::anyhow!("the fixture channel has no observer write end"))?,
        )?;
        self.next_transfer += 1;
        let message = observation::Handoff {
            protocol: observation::PROTOCOL.to_owned(),
            nonce: self.nonce.clone(),
            source: observation::Source::Supervisor,
            fixture: Some(fixture_id),
            transfer_id: self.next_transfer,
            kind: observation::Kind::FixtureChannel,
            claimed_pid: 0,
            process_handle: None,
            job_handle: None,
            pipe_request_read: Some(pipe_request_read as u64),
            pipe_response_write: Some(pipe_response_write as u64),
            granted_access: "inherited pipe ends; no process rights".to_owned(),
        };
        let ack = self.channel.handoff(&message, deadline)?;
        if !ack.accepted {
            bail!(
                "the observer refused the fixture channel registration: {}",
                ack.reason
            );
        }
        let request_write = channel
            .request_write()
            .ok_or_else(|| anyhow::anyhow!("the fixture channel has no fixture write end"))?;
        let response_read = channel
            .response_read()
            .ok_or_else(|| anyhow::anyhow!("the fixture channel has no fixture read end"))?;
        Ok(FixtureGrant {
            _channel: channel,
            fixture_id,
            request_write,
            response_read,
        })
    }

    /// Stop both channel workers and reap them within a bound.
    fn finish(self, deadline: Instant) -> Result<()> {
        self.channel.finish(deadline)
    }
}

#[cfg(windows)]
fn supervisor_main() -> Result<()> {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |name: &str| -> Option<String> {
        let position = arguments.iter().position(|argument| argument == name)?;
        let value = arguments.get(position + 1).cloned()?;
        arguments.drain(position..=position + 1);
        Some(value)
    };
    let plan_path = take("--plan").ok_or_else(|| anyhow::anyhow!("--plan is required"))?;
    let status_path = take("--status").ok_or_else(|| anyhow::anyhow!("--status is required"))?;
    let total_deadline_ms: u64 = take("--total-deadline-ms")
        .ok_or_else(|| anyhow::anyhow!("--total-deadline-ms is required"))?
        .parse()
        .context("--total-deadline-ms is not a number")?;

    // Observation is opt-in by argument. Any observation argument that is
    // present must parse, and the full set must be present together: a
    // half-specified channel is refused, never guessed into working.
    let observation_args = match (
        take("--observer-handle"),
        take("--observer-nonce"),
        take("--observation-request"),
        take("--observation-response"),
    ) {
        (None, None, None, None) => None,
        (Some(observer), Some(nonce), Some(request), Some(response)) => Some(ObservationArgs {
            observer: observer
                .parse()
                .context("--observer-handle is not a number")?,
            nonce,
            request: request
                .parse()
                .context("--observation-request is not a number")?,
            response: response
                .parse()
                .context("--observation-response is not a number")?,
        }),
        _ => bail!(
            "the observation arguments are all-or-nothing: \
             --observer-handle, --observer-nonce, --observation-request \
             and --observation-response must appear together"
        ),
    };

    let plan: Plan = serde_json::from_str(
        &std::fs::read_to_string(&plan_path)
            .with_context(|| format!("could not read {plan_path}"))?,
    )
    .context("the acceptance plan is not valid JSON")?;

    // ---- prevalidation, before anything is spawned -----------------------
    {
        if plan.steps.is_empty() {
            bail!("an acceptance plan must contain at least one step");
        }
        let mut seen = BTreeMap::new();
        for (index, step) in plan.steps.iter().enumerate() {
            if step.mode != "spawn" && step.mode != "run" {
                bail!(
                    "step {:?} has an invalid mode {:?} (spawn|run)",
                    step.tag,
                    step.mode
                );
            }
            if step.tag.trim().is_empty() {
                bail!("a plan step has an empty tag");
            }
            if seen.insert(step.tag.clone(), ()).is_some() {
                bail!("duplicate plan step tag {:?}", step.tag);
            }
            if step.mode == "run" && step.ready_from_log {
                bail!("readyFromLog is only valid on a spawn step");
            }
            if step.observe_descendants && step.mode != "run" {
                // The verified exit of a run step is what tells the
                // supervisor the descendant hand-off is complete; a spawn
                // step has no such point, and guessing one from a sleep is
                // exactly what this design refuses to do.
                bail!(
                    "step {:?} asks for descendant observation but is not a run step",
                    step.tag
                );
            }
            if step.debug_descendants
                && (step.mode != "run"
                    || index + 1 != plan.steps.len()
                    || step.observe_descendants
                    || observation_args.is_none())
            {
                bail!(
                    "debugDescendants requires an independent observer and the final run step, without observeDescendants"
                );
            }
        }
    }

    let status_file = std::fs::File::create(&status_path)
        .with_context(|| format!("could not create {status_path}"))?;

    let verdict = run_steps(&plan, status_file, total_deadline_ms, observation_args);
    let green = verdict.green && !verdict.status_failed;
    // Everything — job handle, child handles, status writer — has been
    // dropped by the time this line runs, and the final status event is
    // already on disk. Only now may the process set its exit code.
    if !green {
        std::process::exit(1);
    }
    Ok(())
}

/// What one supervised run concluded. Produced only after every owned
/// handle has been dropped or verified.
struct Verdict {
    green: bool,
    status_failed: bool,
    deadline_exceeded: bool,
    failed_step: Option<String>,
    failure_class: Option<String>,
    containment_verified: bool,
}

/// One kernel event stays paused while its real descendant handle is offered
/// to the independent observer. A refused hand-off is continued only after
/// the caller has terminated its own Job in the cleanup path.
#[cfg(windows)]
fn pump_debug_event(
    tree: &mut debug_tree::DebugTree,
    job: &job::Job,
    root_pid: u32,
    observer: &mut Option<Observation>,
    handles: &mut Vec<debug_tree::ProcessHandle>,
    deadline: Instant,
) -> Result<()> {
    if let Some(event) = tree.poll(job.handle())? {
        match event {
            debug_tree::Event::Created(process) => {
                let pid = process.pid;
                let first = handles.is_empty();
                handles.push(process.handle);
                if first && pid != root_pid {
                    bail!("debug root does not match the creation handle");
                }
                if !first {
                    let observer = observer.as_mut().context("debug tree has no observer")?;
                    if !observer.report_descendant(pid, handles.last().unwrap().raw(), deadline)? {
                        bail!("observer refused the debug descendant");
                    }
                }
            }
            debug_tree::Event::Exited { pid, code } => {
                let _ = (pid, code);
            }
            debug_tree::Event::Other => {}
        }
        tree.continue_event()?;
    }
    Ok(())
}

/// Execute the frozen plan. Returns the verdict only after the job has
/// been terminated, the job accounting reports zero active processes, and
/// every held child handle has been waited within the cleanup deadline.
#[cfg(windows)]
fn run_steps(
    plan: &Plan,
    status_file: std::fs::File,
    total_deadline_ms: u64,
    observation_args: Option<ObservationArgs>,
) -> Verdict {
    use std::io::Read as _;
    use std::os::windows::io::FromRawHandle as _;

    fn status_emit(
        status: &mut Option<std::io::BufWriter<std::fs::File>>,
        status_ok: &mut bool,
        value: &serde_json::Value,
    ) {
        if let Some(writer) = status.as_mut() {
            use std::io::Write as _;
            if writeln!(writer, "{value}")
                .and_then(|_| writer.flush())
                .is_err()
            {
                *status_ok = false;
            }
        } else {
            *status_ok = false;
        }
    }

    let mut status = Some(std::io::BufWriter::new(status_file));
    let mut status_ok = true;
    let mut verdict = Verdict {
        green: false,
        status_failed: false,
        deadline_exceeded: false,
        failed_step: None,
        failure_class: None,
        containment_verified: false,
    };

    let job = match job::Job::new() {
        Ok(job) => job,
        Err(_) => {
            verdict.failure_class = Some("job-create".to_owned());
            status_emit(
                &mut status,
                &mut status_ok,
                &serde_json::json!({
                    "event": "final", "containmentVerified": false,
                    "failureClass": "job-create",
                }),
            );
            return verdict;
        }
    };
    let total_deadline = Instant::now() + Duration::from_millis(total_deadline_ms);
    // Rebuild the subject side of the observation channel, if the runner
    // asked for one. A channel that cannot be opened is a failed run, never
    // a silent downgrade: the runner requested evidence, and proceeding
    // without the channel would look like a run that simply never reported.
    let mut observation = match observation_args {
        Some(args) => match Observation::start(args) {
            Ok(observation) => Some(observation),
            Err(_) => {
                let mut verdict = Verdict {
                    green: false,
                    status_failed: false,
                    deadline_exceeded: false,
                    failed_step: None,
                    failure_class: Some("observation".to_owned()),
                    containment_verified: false,
                };
                status_emit(
                    &mut status,
                    &mut status_ok,
                    &serde_json::json!({
                        "event": "final", "containmentVerified": false,
                        "failureClass": "observation", "observation": "failed",
                    }),
                );
                verdict.status_failed = !status_ok;
                return verdict;
            }
        },
        None => None,
    };
    let mut failed_step: Option<String> = None;
    let mut failure_class: Option<String> = None;
    // Membership must be checked before a root is resumed, so the observer
    // needs the exact job handle before the first root hand-off.
    if let Some(observer) = observation.as_mut() {
        if !matches!(
            observer.report_job_query(job.handle(), total_deadline),
            Ok(true)
        ) {
            failed_step = Some("*:job-observation".to_owned());
            failure_class = Some("observation".to_owned());
            let _ = job.terminate();
        }
    }
    let mut substitutions: BTreeMap<String, String> = BTreeMap::new();
    // Every created child is held for its whole lifetime: its handles stay
    // open until the verified teardown, never forgotten.
    let mut held: Vec<create::ChildGuard> = Vec::new();
    let mut startup_cleanup_ok = true;
    let mut io_workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut debug_state: Option<(debug_tree::DebugTree, u32)> = None;
    let mut debug_handles = Vec::new();
    let mut debug_cleanup_ok = true;

    'steps: for step in &plan.steps {
        if failed_step.is_some() {
            break;
        }
        if Instant::now() >= total_deadline {
            verdict.deadline_exceeded = true;
            failed_step.get_or_insert_with(|| step.tag.clone());
            failure_class.get_or_insert_with(|| "total-deadline".to_owned());
            break;
        }
        let mut step = step.clone();
        for argument in &mut step.args {
            for (placeholder, resolved) in &substitutions {
                let needle = format!("{{{placeholder}}}");
                if argument.contains(&needle) {
                    *argument = argument.replace(&needle, resolved);
                }
            }
        }
        for value in step.env.values_mut() {
            for (placeholder, resolved) in &substitutions {
                let needle = format!("{{{placeholder}}}");
                if value.contains(&needle) {
                    *value = value.replace(&needle, resolved);
                }
            }
        }

        // ---- create suspended, contain, then resume ---------------------
        // An observing step's channel is registered with the observer
        // **before** the fixture exists: the registration ACK is the first
        // link in the order registration → creation → containment →
        // root-child hand-off → resume. A grant that cannot complete ends
        // the run here — the job is terminated explicitly, because the
        // observer may already hold the pipe ends or job copies whose
        // presence would otherwise delay kill-on-close past this teardown.
        let mut observation_grant: Option<FixtureGrant> = None;
        if step.observe_descendants {
            let grant = match observation.as_mut() {
                Some(observation) => observation.grant_fixture_channel(total_deadline),
                // A plan that asks for observation on a run that has no
                // observation channel is a malformed run, never a silent
                // downgrade to unobserved.
                None => Err(anyhow::anyhow!(
                    "the plan asks for descendant observation but no observation channel was provided"
                )),
            };
            match grant {
                Ok(grant) => observation_grant = Some(grant),
                Err(_) => {
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| "observation".to_owned());
                    let _ = job.terminate();
                    status_emit(
                        &mut status,
                        &mut status_ok,
                        &serde_json::json!({
                            "event": "step", "tag": step.tag, "failed": true,
                            "class": "observation",
                        }),
                    );
                    if !status_ok {
                        verdict.status_failed = true;
                    }
                    break 'steps;
                }
            }
        }
        // Only an observing step sees the observer and its channel: the env
        // extension and the extra inherited handles are both empty for every
        // other step, so an unobserved child never learns this run is
        // observed at all.
        let mut step_env = step.env.clone();
        let mut extra_inherit: Vec<windows_sys::Win32::Foundation::HANDLE> = Vec::new();
        if let Some(grant) = observation_grant.as_ref() {
            let observation = observation
                .as_ref()
                .expect("a grant exists only when observation does");
            step_env.insert(
                observation_env::HANDLE.to_owned(),
                (observation.observer as usize).to_string(),
            );
            step_env.insert(observation_env::NONCE.to_owned(), observation.nonce.clone());
            step_env.insert(
                observation_env::REQUEST.to_owned(),
                (grant.request_write as usize).to_string(),
            );
            step_env.insert(
                observation_env::RESPONSE.to_owned(),
                (grant.response_read as usize).to_string(),
            );
            step_env.insert(
                observation_env::FIXTURE_ID.to_owned(),
                grant.fixture_id.to_string(),
            );
            // The observer handle is already inheritable — the runner marked
            // it before this process inherited it — and the channel ends
            // were marked by `grant_fixture_channel`.
            extra_inherit.extend_from_slice(&[
                observation.observer,
                grant.request_write,
                grant.response_read,
            ]);
        }
        // Debug events must be consumed on this same creating thread.
        let debug = step.debug_descendants.then(debug_tree::DebugTree::new);
        let mut created = match create::create_suspended(
            &step.program,
            &step.args,
            &step_env,
            step.cwd.as_deref(),
            step.stdin_text.is_some(),
            &extra_inherit,
            step.debug_descendants,
        ) {
            Ok(created) => created,
            Err(_) => {
                failed_step.get_or_insert_with(|| step.tag.clone());
                failure_class.get_or_insert_with(|| "spawn".to_owned());
                status_emit(
                    &mut status,
                    &mut status_ok,
                    &serde_json::json!({
                        "event": "step", "tag": step.tag, "failed": true,
                        "class": "spawn",
                    }),
                );
                if !status_ok {
                    verdict.status_failed = true;
                }
                break 'steps;
            }
        };
        let pid = created.guard.pid;
        if let Some(debug) = debug {
            debug_state = Some((debug, pid));
        }
        if job.assign_handle(created.guard.process).is_err() {
            // It never ran an instruction and was never in the job: end it
            // through the creation handle and wait, verified.
            let waited = created.guard.kill_and_wait(5_000).is_ok();
            startup_cleanup_ok &= waited;
            held.push(created.guard);
            failed_step.get_or_insert_with(|| step.tag.clone());
            failure_class.get_or_insert_with(|| "containment".to_owned());
            if !waited {
                failure_class.get_or_insert_with(|| "cleanup".to_owned());
            }
            status_emit(
                &mut status,
                &mut status_ok,
                &serde_json::json!({
                    "event": "step", "tag": step.tag, "failed": true,
                    "class": "containment",
                }),
            );
            break 'steps;
        }
        // Observation: the child is contained but has not run an
        // instruction. Its real creation handle — query and wait rights
        // only — travels to the observer now, and only an accepted
        // hand-off lets it run. A refusal, a broken channel, or a dead
        // observer all stop the resume: an unobserved child is not the
        // evidence this run was started to produce, and a job handle copy
        // the observer may still hold means only an explicit terminate is
        // a guaranteed ending.
        if let Some(observation) = observation.as_mut() {
            let accepted =
                observation.report_root_child(pid, created.guard.process, total_deadline);
            if !matches!(accepted, Ok(true)) {
                let waited = created.guard.kill_and_wait(5_000).is_ok();
                startup_cleanup_ok &= waited;
                held.push(created.guard);
                failed_step.get_or_insert_with(|| step.tag.clone());
                failure_class.get_or_insert_with(|| "observation".to_owned());
                if !waited {
                    failure_class.get_or_insert_with(|| "cleanup".to_owned());
                }
                let _ = job.terminate();
                status_emit(
                    &mut status,
                    &mut status_ok,
                    &serde_json::json!({
                        "event": "step", "tag": step.tag, "failed": true,
                        "class": "observation",
                    }),
                );
                if !status_ok {
                    verdict.status_failed = true;
                }
                break 'steps;
            }
        }
        if created.guard.resume().is_err() {
            let waited = created.guard.kill_and_wait(5_000).is_ok();
            startup_cleanup_ok &= waited;
            held.push(created.guard);
            failed_step.get_or_insert_with(|| step.tag.clone());
            failure_class.get_or_insert_with(|| "resume".to_owned());
            if !waited {
                failure_class.get_or_insert_with(|| "cleanup".to_owned());
            }
            status_emit(
                &mut status,
                &mut status_ok,
                &serde_json::json!({
                    "event": "step", "tag": step.tag, "failed": true,
                    "class": "resume",
                }),
            );
            break 'steps;
        }
        held.push(created.guard);

        // stderr is always drained in a detached thread: bounded count,
        // text discarded. A full stderr pipe would deadlock the child.
        let stderr_read = created.pipes.stderr_read;
        created.pipes.stderr_read = std::ptr::null_mut();
        let mut stderr_stream = unsafe { std::fs::File::from_raw_handle(stderr_read) };
        io_workers.push(std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match stderr_stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }));

        // stdin text is written from a detached thread too: a child that
        // never reads would block this loop forever on a large write.
        if let Some(text) = step.stdin_text.clone() {
            let stdin_write = created.pipes.stdin_write;
            created.pipes.stdin_write = std::ptr::null_mut();
            let mut stdin_stream = unsafe { std::fs::File::from_raw_handle(stdin_write) };
            io_workers.push(std::thread::spawn(move || {
                let _ = stdin_stream.write_all(text.as_bytes());
                let _ = stdin_stream.flush();
            }));
        }

        if step.mode == "spawn" {
            if step.ready_from_log {
                // Readiness runs in its own thread because pipe reads
                // block; the deadline is enforced here through the channel
                // timeout, and the teardown ends both the child and the
                // thread when it fires. The thread keeps draining after
                // readiness, so the child never sees a full pipe.
                let stdout_read = created.pipes.stdout_read;
                created.pipes.stdout_read = std::ptr::null_mut();
                let mut stdout_stream = unsafe { std::fs::File::from_raw_handle(stdout_read) };
                let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
                io_workers.push(std::thread::spawn(move || {
                    let mut line: Vec<u8> = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    let mut consumed = 0_usize;
                    let mut outcome_sent = false;
                    const CAP: usize = 64 * 1024;
                    loop {
                        match stdout_stream.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                // After the first outcome, only drain. Readiness
                                // limits must not close a running child's pipe.
                                if outcome_sent {
                                    continue;
                                }
                                consumed += read;
                                if consumed > CAP {
                                    let _ = ready_tx.try_send(Readiness::Cap);
                                    outcome_sent = true;
                                    line.clear();
                                    continue;
                                }
                                for &byte in &buffer[..read] {
                                    if byte == b'\n' {
                                        let text = String::from_utf8_lossy(&line).into_owned();
                                        line.clear();
                                        let Ok(value) =
                                            serde_json::from_str::<serde_json::Value>(&text)
                                        else {
                                            continue;
                                        };
                                        let address = value
                                            .get("address")
                                            .and_then(serde_json::Value::as_str);
                                        let protocol = value
                                            .get("protocolVersion")
                                            .and_then(serde_json::Value::as_u64);
                                        let reported_pid =
                                            value.get("pid").and_then(serde_json::Value::as_u64);
                                        let loopback = address.is_some_and(|address| {
                                            address.starts_with("127.0.0.1:")
                                        });
                                        let protocol_matched = protocol.is_some_and(|version| {
                                            version == fintwind_protocol::PROTOCOL_VERSION as u64
                                        });
                                        let pid_matched = reported_pid == Some(pid as u64);
                                        let valid_socket = address
                                            .and_then(|address| {
                                                address.parse::<std::net::SocketAddr>().ok()
                                            })
                                            .is_some();
                                        if loopback
                                            && protocol_matched
                                            && pid_matched
                                            && valid_socket
                                        {
                                            let _ = ready_tx.try_send(Readiness::Ready {
                                                address: address.unwrap_or_default().to_owned(),
                                            });
                                            outcome_sent = true;
                                            break;
                                        }
                                    } else {
                                        line.push(byte);
                                    }
                                }
                            }
                        }
                    }
                    if !outcome_sent {
                        let _ = ready_tx.try_send(Readiness::Eof);
                    }
                }));

                let mut ready = false;
                loop {
                    let remaining = total_deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(1));
                    if remaining.is_zero() {
                        verdict.deadline_exceeded = true;
                        failed_step.get_or_insert_with(|| format!("{}:ready", step.tag));
                        failure_class.get_or_insert_with(|| "readiness".to_owned());
                        break;
                    }
                    match ready_rx.recv_timeout(remaining) {
                        Ok(Readiness::Ready { address }) => {
                            substitutions.insert("daemonAddress".to_owned(), address);
                            ready = true;
                            status_emit(
                                &mut status,
                                &mut status_ok,
                                &serde_json::json!({
                                    "event": "readiness", "tag": step.tag, "ready": true,
                                }),
                            );
                            break;
                        }
                        Ok(Readiness::Eof) => {
                            failed_step.get_or_insert_with(|| format!("{}:ready", step.tag));
                            failure_class.get_or_insert_with(|| "readiness-eof".to_owned());
                            break;
                        }
                        Ok(Readiness::Cap) => {
                            failed_step.get_or_insert_with(|| format!("{}:ready", step.tag));
                            failure_class.get_or_insert_with(|| "readiness-cap".to_owned());
                            break;
                        }
                        Err(_) => continue,
                    }
                }
                if !status_ok {
                    verdict.status_failed = true;
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| "status-write".to_owned());
                    break 'steps;
                }
                if !ready {
                    break 'steps;
                }
            } else {
                let stdout_read = created.pipes.stdout_read;
                created.pipes.stdout_read = std::ptr::null_mut();
                let mut stream = unsafe { std::fs::File::from_raw_handle(stdout_read) };
                io_workers.push(std::thread::spawn(move || {
                    let mut buffer = [0_u8; 4096];
                    loop {
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                }));
            }
            status_emit(
                &mut status,
                &mut status_ok,
                &serde_json::json!({
                    "event": "step", "tag": step.tag, "mode": "spawn", "started": true,
                }),
            );
            if !status_ok {
                verdict.status_failed = true;
                failed_step.get_or_insert_with(|| step.tag.clone());
                failure_class.get_or_insert_with(|| "status-write".to_owned());
                break 'steps;
            }
            continue;
        }

        // ---- run mode: stdout drained, bounded wait on the handle -------
        let stdout_read = created.pipes.stdout_read;
        created.pipes.stdout_read = std::ptr::null_mut();
        let stdout_stream = unsafe { std::fs::File::from_raw_handle(stdout_read) };
        io_workers.push(std::thread::spawn(move || {
            let mut stream = stdout_stream;
            let mut buffer = [0_u8; 4096];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }));

        let step_start = Instant::now();
        let exit_code = loop {
            let remaining = total_deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u32::MAX as u128) as u32;
            if let Some((tree, root_pid)) = debug_state.as_mut() {
                if Instant::now() >= total_deadline {
                    verdict.deadline_exceeded = true;
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| "total-deadline".to_owned());
                    break None;
                }
                if pump_debug_event(
                    tree,
                    &job,
                    *root_pid,
                    &mut observation,
                    &mut debug_handles,
                    total_deadline,
                )
                .is_err()
                {
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| "debug-observation".to_owned());
                    break None;
                }
                match create::wait_handle(held.last().expect("pushed above").process, 0) {
                    Ok(Some(code)) => break Some(code),
                    Ok(None) => continue,
                    Err(_) => {
                        failed_step.get_or_insert_with(|| step.tag.clone());
                        failure_class.get_or_insert_with(|| "debug-wait".to_owned());
                        break None;
                    }
                }
            }
            match create::wait_handle(held.last().expect("pushed above").process, remaining.max(1))
            {
                Ok(Some(code)) => break Some(code),
                Ok(None) => {
                    verdict.deadline_exceeded = true;
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| "total-deadline".to_owned());
                    break None;
                }
                Err(_) => {
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| "wait".to_owned());
                    break None;
                }
            }
        };
        let duration_ms = step_start.elapsed().as_millis() as u64;
        match exit_code {
            Some(code) => {
                status_emit(
                    &mut status,
                    &mut status_ok,
                    &serde_json::json!({
                        "event": "step", "tag": step.tag, "mode": "run",
                        "exitCode": code, "durationMs": duration_ms,
                    }),
                );
                if !status_ok {
                    verdict.status_failed = true;
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| "status-write".to_owned());
                    break 'steps;
                }
                if code != 0 {
                    failed_step.get_or_insert_with(|| step.tag.clone());
                    failure_class.get_or_insert_with(|| format!("exit-{code}"));
                    break 'steps;
                }
            }
            None => break 'steps,
        }

        // A successful step can contribute a runtime value to later steps.
        if let Some(substitute) = &step.output_substitute {
            let parsed: Option<serde_json::Value> = std::fs::read_to_string(&substitute.file)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());
            match parsed
                .as_ref()
                .and_then(|value| value.get(&substitute.field))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
            {
                Some(value) => {
                    substitutions.insert(substitute.placeholder.clone(), value);
                }
                None => {
                    failed_step.get_or_insert_with(|| format!("{}:output", step.tag));
                    failure_class.get_or_insert_with(|| "output-substitute".to_owned());
                    break 'steps;
                }
            }
        }
    }

    // A non-cooperating debug tree cannot be frozen while it can still spawn.
    // This is always the final step. End only our Job, drain kernel events on
    // the creating thread and wait on every duplicate BEFORE topology freeze.
    if let Some((tree, root_pid)) = debug_state.as_mut() {
        debug_cleanup_ok &= job.terminate().is_ok();
        let debug_deadline = Instant::now() + Duration::from_secs(10);
        let _ = tree.continue_event(); // Pending event: Job termination came first.
        while Instant::now() < debug_deadline {
            if pump_debug_event(
                tree,
                &job,
                *root_pid,
                &mut observation,
                &mut debug_handles,
                debug_deadline,
            )
            .is_err()
            {
                debug_cleanup_ok = false;
                break;
            }
            if tree.active_count() == 0 && matches!(job.active_processes_is_zero(), Ok(true)) {
                break;
            }
        }
        debug_cleanup_ok &= tree.active_count() == 0 && !debug_handles.is_empty();
        for handle in &debug_handles {
            let remaining = debug_deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u32::MAX as u128) as u32;
            debug_cleanup_ok &= matches!(create::wait_handle(handle.raw(), remaining), Ok(Some(_)));
        }
        if !debug_cleanup_ok {
            failed_step.get_or_insert_with(|| "*:debug-cleanup".to_owned());
            failure_class.get_or_insert_with(|| "debug-cleanup".to_owned());
        }
    }

    // ---- freeze the observed topology, then tear down ---------------------
    // Sent exactly once, only after the step loop ended: the exact job
    // query handle first, then the freeze marker. From here on this
    // supervisor creates no further children, so the observer can reconcile
    // the job's own process-id list against the real handles it holds.
    // A freeze that cannot be delivered — a dead observer is the usual
    // form — is a failed run, and because the observer may hold job-handle
    // copies that delay kill-on-close, the job is terminated explicitly.
    if let Some(observation) = observation.as_mut() {
        // Debug cleanup can legitimately extend beyond the execution deadline.
        // The observer's separate lifetime covers this bounded cleanup phase.
        let freeze_deadline = if debug_state.is_some() {
            Instant::now() + Duration::from_secs(5)
        } else {
            total_deadline
        };
        let frozen = observation.report_topology_frozen(freeze_deadline);
        if !matches!(frozen, Ok(true)) {
            failed_step.get_or_insert_with(|| "*:freeze".to_owned());
            failure_class.get_or_insert_with(|| "observation".to_owned());
            let _ = job.terminate();
        }
    }

    // ---- teardown, verified ------------------------------------------------
    // Both a normal end and a failure end terminate the job: nothing owned
    // outlives the run. Termination is by job handle, verification by the
    // job's own accounting plus bounded waits on every held child handle.
    let teardown_started = Instant::now();
    let terminated = job.terminate().is_ok();
    let cleanup_deadline = teardown_started + Duration::from_secs(10);
    let mut all_exited = true;
    for child in &held {
        let remaining = cleanup_deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u32::MAX as u128) as u32;
        if !matches!(
            create::wait_handle(child.process, remaining.max(1)),
            Ok(Some(_))
        ) {
            all_exited = false;
            break;
        }
    }
    let mut active_zero = false;
    while Instant::now() < cleanup_deadline {
        match job.active_processes_is_zero() {
            Ok(true) => {
                active_zero = true;
                break;
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    // Pipe EOF follows job teardown, including descendants that inherited
    // stdio. Verify workers within the same cleanup deadline; never join a
    // still-blocked reader or writer without a bound.
    while !io_workers.iter().all(std::thread::JoinHandle::is_finished)
        && Instant::now() < cleanup_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut io_workers_exited = true;
    for worker in io_workers {
        if worker.is_finished() {
            io_workers_exited &= worker.join().is_ok();
        } else {
            io_workers_exited = false;
        }
    }

    // The observation channel's own workers are reaped within the same
    // cleanup deadline. A channel that cannot be closed cleanly fails the
    // run — an observation that never finished is not evidence, and this
    // run was observed for a reason. "absent" records the no-observation
    // mode, in which nothing here changed.
    let mut observation_ok = true;
    let observation_outcome = match observation.take() {
        Some(observation) => {
            if observation.finish(cleanup_deadline).is_ok() {
                "integrated"
            } else {
                observation_ok = false;
                "unreaped"
            }
        }
        None => "absent",
    };

    let containment_verified = terminated
        && startup_cleanup_ok
        && all_exited
        && active_zero
        && io_workers_exited
        && debug_cleanup_ok;

    let final_event = serde_json::json!({
        "event": "final",
        "totalDeadlineExceeded": verdict.deadline_exceeded,
        "failedStep": failed_step,
        "failureClass": failure_class,
        "statusFileOk": status_ok,
        "containmentVerified": containment_verified,
        "jobActiveProcessesZero": active_zero,
        "allChildrenExited": all_exited,
        "startupCleanupVerified": startup_cleanup_ok,
        "ioWorkersExited": io_workers_exited,
        "terminated": terminated,
        "observation": observation_outcome,
        "debugTreeRequested": debug_state.is_some(),
        "debugProcessHandles": debug_handles.len(),
        "debugCleanupVerified": debug_cleanup_ok,
    });
    status_emit(&mut status, &mut status_ok, &final_event);
    if !status_ok {
        verdict.status_failed = true;
    }
    verdict.failed_step = failed_step;
    verdict.failure_class = failure_class;
    verdict.containment_verified = containment_verified;
    verdict.green = !verdict.deadline_exceeded
        && verdict.failed_step.is_none()
        && !verdict.status_failed
        && containment_verified
        && observation_ok;
    verdict
}

/// Readiness outcomes sent from the reader thread to the supervisor loop.
#[cfg(windows)]
enum Readiness {
    Ready { address: String },
    Eof,
    Cap,
}
