//! Pure synthetic debug-event handle black box. No browser or production data.
//! Failure list comes first in docs/remote-control-browser-ownership.md.
//! Usage: acceptance_debug_tree_blackbox --fixture ABS --out ABS

#[cfg(windows)]
#[path = "support/acceptance_debug_tree.rs"]
mod debug_tree;

#[cfg(not(windows))]
fn main() { panic!("Windows handle semantics are required"); }

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{bail, Context as _};
    use std::{collections::BTreeMap, path::PathBuf, time::{Duration, Instant}};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        System::{
            JobObjects::{AssignProcessToJobObject, CreateJobObjectW,
                SetInformationJobObject, TerminateJobObject, QueryInformationJobObject, IsProcessInJob,
                JobObjectExtendedLimitInformation, JobObjectBasicAccountingInformation,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE},
            Threading::{CreateProcessW, GetCurrentProcess,
                ResumeThread, WaitForSingleObject, TerminateProcess,
                CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, DEBUG_PROCESS,
                PROCESS_INFORMATION, STARTUPINFOW},
        },
    };
    struct Owned(HANDLE);
    impl Drop for Owned { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
    struct Job(Owned);
    impl Drop for Job { fn drop(&mut self) { unsafe { TerminateJobObject(self.0.0, 1); } } }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |name: &str| -> anyhow::Result<PathBuf> {
        let index = args.iter().position(|arg| arg == name).context("missing argument")?;
        args.remove(index);
        if index >= args.len() { bail!("missing value"); }
        let path = PathBuf::from(args.remove(index));
        if !path.is_absolute() { bail!("absolute paths required"); }
        Ok(path)
    };
    let fixture = take("--fixture")?.canonicalize()?;
    let out = take("--out")?;
    if !args.is_empty() || out.exists() { bail!("unknown arguments or existing artifact"); }
    let directory = out.parent().context("output directory required")?;
    // This round claims a new directory, never reuses a previous round's files.
    std::fs::create_dir(directory)?;
    let mut outer = 0;
    if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut outer) } == 0 || outer != 0 {
        bail!("black box must not run in an outer job");
    }
    // All setup failures before resume affect only this root's creation handle.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() { return Err(std::io::Error::last_os_error().into()); }
    let job = Job(Owned(raw));
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe { SetInformationJobObject(raw, JobObjectExtendedLimitInformation,
        &limits as *const _ as _, std::mem::size_of_val(&limits) as u32) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let quote = |value: &str| -> anyhow::Result<String> {
        if value.contains('"') || value.ends_with('\\') { bail!("unsupported quoted path"); }
        Ok(format!("\"{value}\""))
    };
    let facts = directory.join("fixture-facts.json");
    let grandchild = directory.join("grandchild-facts.json");
    let program = fixture.to_str().context("UTF-8 fixture path required")?;
    let command = format!("{} --facts {} --stay-alive --grandchild --grandchild-facts {}",
        quote(program)?, quote(facts.to_str().context("UTF-8 path required")?)?,
        quote(grandchild.to_str().context("UTF-8 path required")?)?);
    let wide = |value: &str| value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let app = wide(program);
    let mut command = wide(&command);
    let cwd = wide(directory.to_str().context("UTF-8 directory required")?);
    let mut env = BTreeMap::new();
    for name in ["SystemRoot", "windir", "COMSPEC", "PATH", "PATHEXT"] {
        if let Ok(value) = std::env::var(name) { env.insert(name, value); }
    }
    // No inherited user profile/configuration or provider credentials.
    let home = directory.join("home");
    std::fs::create_dir_all(&home)?;
    for name in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP"] {
        env.insert(name, home.to_str().context("UTF-8 home required")?.to_owned());
    }
    let mut environment = Vec::new();
    for (key, value) in &env { environment.extend(wide(&format!("{key}={value}"))); }
    environment.push(0);
    let mut startup = STARTUPINFOW::default();
    startup.cb = std::mem::size_of_val(&startup) as u32;
    let mut info = PROCESS_INFORMATION::default();
    let mut events = debug_tree::DebugTree::new();
    if unsafe { CreateProcessW(app.as_ptr(), command.as_mut_ptr(), std::ptr::null(),
        std::ptr::null(), 0, CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | DEBUG_PROCESS,
        environment.as_ptr() as _, cwd.as_ptr(), &startup, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let root = Owned(info.hProcess);
    let thread = Owned(info.hThread);
    if unsafe { AssignProcessToJobObject(raw, root.0) } == 0 {
        unsafe { TerminateProcess(root.0, 1); }
        // Debug events must be pumped even on setup failure; debugger exit
        // remains fail-closed, but never becomes a verified cleanup claim.
        bail!("assignment failed; cleanup is unverified");
    }
    if unsafe { ResumeThread(thread.0) } == u32::MAX { bail!("resume failed"); }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut handles = Vec::new();
    let mut exits = 0;
    let mut failure = None;
    let result = (|| -> anyhow::Result<()> {
        while Instant::now() < deadline && handles.len() < 2 {
            if let Some(event) = events.poll(raw)? {
                match event {
                    debug_tree::Event::Created(process) => {
                        if handles.is_empty() && process.pid != info.dwProcessId { bail!("wrong debug root"); }
                        handles.push(process.handle);
                    }
                    debug_tree::Event::Exited { .. } => bail!("synthetic tree exited before two creation events"),
                    debug_tree::Event::Other => {}
                }
                events.continue_event()?;
            }
        }
        if handles.len() != 2 || events.active_count() != 2 { bail!("incomplete synthetic tree"); }
        Ok(())
    })();
    if let Err(error) = result { failure = Some(error.to_string()); }
    let terminated = unsafe { TerminateJobObject(job.0.0, 1) } != 0;
    let cleanup = Instant::now() + Duration::from_secs(10);
    // If a hand-off failed, continue its paused event only AFTER job termination.
    let _ = events.continue_event();
    while Instant::now() < cleanup && events.active_count() != 0 {
        match events.poll(raw) {
            Ok(Some(event)) => {
                match event {
                    debug_tree::Event::Created(process) => handles.push(process.handle),
                    debug_tree::Event::Exited { pid, code } => { let _ = (pid, code); exits += 1; }
                    debug_tree::Event::Other => {}
                }
                if events.continue_event().is_err() { failure.get_or_insert("cleanup continuation failed".to_owned()); break; }
            }
            Ok(None) => {}
            Err(_) => { failure.get_or_insert("cleanup event processing failed".to_owned()); break; }
        }
    }
    // EXIT_PROCESS is delivered before termination completes. Continue it,
    // then wait on our own duplicate; a zero-time poll races that completion.
    let remaining = || cleanup.saturating_duration_since(Instant::now())
        .as_millis().min(u32::MAX as u128) as u32;
    let mut waited = true;
    for handle in &handles {
        waited &= unsafe { WaitForSingleObject(handle.raw(), remaining()) } == WAIT_OBJECT_0;
    }
    waited &= unsafe { WaitForSingleObject(root.0, remaining()) } == WAIT_OBJECT_0;
    let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    let zero = unsafe { QueryInformationJobObject(raw, JobObjectBasicAccountingInformation,
        &mut accounting as *mut _ as _, std::mem::size_of_val(&accounting) as u32,
        std::ptr::null_mut()) } != 0 && accounting.ActiveProcesses == 0;
    let passed = failure.is_none() && terminated && waited && zero && handles.len() == 2 && exits == 2;
    let artifact = serde_json::json!({ "artifact": "acceptance-debug-tree-blackbox", "passed": passed,
        "fullPlanVerified": false, "browserVerified": false, "observerVerified": false,
        "createdHandles": handles.len(), "exitEvents": exits, "allHandlesExited": waited,
        "jobActiveProcessesZero": zero, "terminatedOwnedJob": terminated, "failure": failure,
        "notCovered": ["observer handoff", "Chrome", "timeout matrix", "debugger crash"] });
    use std::io::Write as _;
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&out)?;
    output.write_all(serde_json::to_string_pretty(&artifact)?.as_bytes())?;
    if !passed { bail!("debug tree black box failed; see artifact"); }
    println!("[OK] synthetic debug tree handles verified");
    Ok(())
}
