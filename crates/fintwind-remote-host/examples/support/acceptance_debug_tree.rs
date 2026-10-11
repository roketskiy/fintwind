//! Handle-only debug-event plumbing. Not a launcher and not a PID adopter.
//! Call on the thread that created the root with DEBUG_PROCESS. The caller
//! owns the exact job and must stop that job on every error or refused ACK.
//! Failure contract: docs/remote-control-browser-ownership.md.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::rc::Rc;

use anyhow::{bail, Context as _, Result};
use windows_sys::Win32::{
    Foundation::{CloseHandle, DuplicateHandle, GetLastError, HANDLE, DBG_CONTINUE,
        DBG_EXCEPTION_NOT_HANDLED, ERROR_SEM_TIMEOUT, EXCEPTION_BREAKPOINT},
    System::{
        Diagnostics::Debug::{ContinueDebugEvent, WaitForDebugEvent, DEBUG_EVENT,
            CREATE_PROCESS_DEBUG_EVENT, EXIT_PROCESS_DEBUG_EVENT, EXCEPTION_DEBUG_EVENT,
            LOAD_DLL_DEBUG_EVENT},
        JobObjects::IsProcessInJob,
        Threading::{GetCurrentProcess, GetCurrentThreadId, GetProcessId,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE},
    },
};

/// A private duplicate; the OS owns the original event process/thread handles.
pub struct ProcessHandle(HANDLE);

impl ProcessHandle {
    pub fn raw(&self) -> HANDLE { self.0 }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) { unsafe { CloseHandle(self.0); } }
}

pub struct CreatedProcess {
    pub pid: u32,
    pub handle: ProcessHandle,
}

pub enum Event {
    Created(CreatedProcess),
    Exited { pid: u32, code: u32 },
    Other,
}

/// !Send/!Sync: debug events must stay on their creating thread.
pub struct DebugTree {
    thread: u32,
    active: BTreeMap<u32, bool>, // Whether its first system breakpoint was seen.
    pending: Option<(u32, u32, i32)>,
    events: u64,
    _thread_bound: PhantomData<Rc<()>>,
}

impl DebugTree {
    /// Construct on the thread immediately before it creates the debug root.
    pub fn new() -> Self {
        Self { thread: unsafe { GetCurrentThreadId() }, active: BTreeMap::new(),
            pending: None, events: 0, _thread_bound: PhantomData }
    }

    pub fn active_count(&self) -> usize { self.active.len() }

    fn check_thread(&self) -> Result<()> {
        if unsafe { GetCurrentThreadId() } != self.thread {
            bail!("debug events must run on the creating thread");
        }
        Ok(())
    }

    /// Poll at most 20 ms. A returned event remains paused until continue_event;
    /// callers must finish observer hand-off first. Errors leave a pending event
    /// paused so that the caller can fail closed through its owned Job handle.
    pub fn poll(&mut self, exact_job: HANDLE) -> Result<Option<Event>> {
        self.check_thread()?;
        if exact_job.is_null() { bail!("an exact job handle is required"); }
        if self.pending.is_some() { bail!("a debug event still awaits continuation"); }
        let mut event = DEBUG_EVENT::default();
        if unsafe { WaitForDebugEvent(&mut event, 20) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_SEM_TIMEOUT { return Ok(None); }
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .context("debug event wait failed");
        }
        self.pending = Some((event.dwProcessId, event.dwThreadId, DBG_CONTINUE));
        self.events += 1;
        if self.events > 1_000_000 { bail!("debug event budget exceeded"); }
        match event.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                let info = unsafe { event.u.CreateProcessInfo };
                if !info.hFile.is_null() { unsafe { CloseHandle(info.hFile); } }
                if info.hProcess.is_null() ||
                    unsafe { GetProcessId(info.hProcess) } != event.dwProcessId ||
                    self.active.contains_key(&event.dwProcessId) {
                    bail!("invalid debug process creation event");
                }
                let mut duplicate = std::ptr::null_mut();
                let current = unsafe { GetCurrentProcess() };
                if unsafe { DuplicateHandle(current, info.hProcess, current, &mut duplicate,
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, 0) } == 0 {
                    return Err(std::io::Error::last_os_error()).context("debug handle duplication failed");
                }
                let handle = ProcessHandle(duplicate);
                let mut member = 0;
                if unsafe { IsProcessInJob(handle.raw(), exact_job, &mut member) } == 0 {
                    return Err(std::io::Error::last_os_error()).context("exact-job membership query failed");
                }
                if member == 0 { bail!("debug process is outside the owned job"); }
                self.active.insert(event.dwProcessId, false);
                Ok(Some(Event::Created(CreatedProcess { pid: event.dwProcessId, handle })))
            }
            EXIT_PROCESS_DEBUG_EVENT => {
                if self.active.remove(&event.dwProcessId).is_none() {
                    bail!("debug process exit has no creation event");
                }
                Ok(Some(Event::Exited { pid: event.dwProcessId,
                    code: unsafe { event.u.ExitProcess.dwExitCode } }))
            }
            LOAD_DLL_DEBUG_EVENT => {
                let file = unsafe { event.u.LoadDll.hFile };
                if !file.is_null() { unsafe { CloseHandle(file); } }
                Ok(Some(Event::Other))
            }
            EXCEPTION_DEBUG_EVENT => {
                let info = unsafe { event.u.Exception };
                let seen = self.active.get_mut(&event.dwProcessId)
                    .context("debug exception has no creation event")?;
                let startup = !*seen && info.dwFirstChance != 0 &&
                    info.ExceptionRecord.ExceptionCode == EXCEPTION_BREAKPOINT;
                if startup { *seen = true; }
                if !startup {
                    self.pending = Some((event.dwProcessId, event.dwThreadId, DBG_EXCEPTION_NOT_HANDLED));
                }
                Ok(Some(Event::Other))
            }
            _ => Ok(Some(Event::Other)),
        }
    }

    /// No automatic continuation in Drop: an unacknowledged process must not run.
    pub fn continue_event(&mut self) -> Result<()> {
        self.check_thread()?;
        let (pid, thread, status) = self.pending.context("no pending debug event")?;
        if unsafe { ContinueDebugEvent(pid, thread, status) } == 0 {
            return Err(std::io::Error::last_os_error()).context("debug continuation failed");
        }
        self.pending = None;
        Ok(())
    }
}
