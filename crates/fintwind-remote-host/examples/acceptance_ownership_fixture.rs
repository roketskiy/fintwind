//! Synthetic child fixture for the synthetic Windows process-ownership black box
//! (`acceptance_ownership_blackbox`). It is the ONLY thing the black box ever
//! asks the supervisor to run, plus the control tree the black box owns itself.
//!
//! Minimal scope, on purpose:
//!
//! * one binary, no daemon, no provider, no browser, no real service, no
//!   credential, no network beyond one loopback listener on an ephemeral port
//!   (the supervisor's readiness check wants a real `127.0.0.1:PORT`);
//! * it writes only inside the run directory it is handed (`--facts`,
//!   `--grandchild-facts`, `--handle-out`, `--substitute-out`);
//! * it reports kernel facts about itself and about the grandchild it creates
//!   with its own `CreateProcessW`, so the black box has an evidence source
//!   that is independent of the supervisor's status file;
//! * in control-tree mode (`--parent-handle`) it duplicates the grandchild's
//!   creation handle into the black box runner and hands the resulting handle
//!   value back over a private file. That value is only meaningful inside the
//!   runner's own handle table; it is harness plumbing, never child output.
//!
//! Roles are composed from flags so the black box never needs a second fixture
//! binary. Every role is deterministic and bounded: nothing waits forever, and
//! `--stay-alive` is the only role that outlives its own script (it is ended by
//! the job teardown, never by name).
//!
//! Usage:
//!   acceptance_ownership_fixture --facts F [--stay-alive] [--exit-code N]
//!     [--sleep-ms N] [--ready-mode none|correct|wrong-pid|wrong-protocol|
//!      wrong-address|wrong-then-correct] [--ready-delay-ms N]
//!     [--junk-bytes N] [--grandchild] [--grandchild-facts F]
//!     [--substitute-out F --substitute-field NAME]
//!     [--parent-handle VALUE --handle-out F] [--echo-args]
//!     [--observation-request V --observation-response V --observer-handle V
//!      --observer-nonce HEX --observation-fixture-id N]

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

/// The shared observation channel. It lives under `examples/support/` so Cargo
/// never auto-discovers it as a runnable example.
#[path = "support/acceptance_observation.rs"]
mod observation;

const MARKER: &str = "fintwind-ownership-fixture-v1";

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ReadyMode {
    #[default]
    None,
    Correct,
    WrongPid,
    WrongProtocol,
    WrongAddress,
    WrongThenCorrect,
}

impl ReadyMode {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "none" => Ok(Self::None),
            "correct" => Ok(Self::Correct),
            "wrong-pid" => Ok(Self::WrongPid),
            "wrong-protocol" => Ok(Self::WrongProtocol),
            "wrong-address" => Ok(Self::WrongAddress),
            "wrong-then-correct" => Ok(Self::WrongThenCorrect),
            other => Err(format!("unknown ready mode {other:?}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Correct => "correct",
            Self::WrongPid => "wrong-pid",
            Self::WrongProtocol => "wrong-protocol",
            Self::WrongAddress => "wrong-address",
            Self::WrongThenCorrect => "wrong-then-correct",
        }
    }
}

#[derive(Serialize)]
// camelCase field names: these are the facts file's wire shape, not Rust style.
#[allow(non_snake_case)]
struct Facts {
    marker: &'static str,
    fixturePid: u32,
    inAnyJob: bool,
    grandchildPid: Option<u32>,
    grandchildInAnyJob: Option<bool>,
    substituteField: Option<String>,
    echoedArgs: Option<Vec<String>>,
    readyMode: &'static str,
}

#[derive(Default)]
struct Options {
    facts: Option<PathBuf>,
    stay_alive: bool,
    exit_code: u32,
    sleep_ms: u64,
    ready_mode: ReadyMode,
    ready_delay_ms: u64,
    junk_bytes: usize,
    grandchild: bool,
    grandchild_facts: Option<PathBuf>,
    substitute_out: Option<PathBuf>,
    substitute_field: Option<String>,
    parent_handle: Option<usize>,
    handle_out: Option<PathBuf>,
    /// The observation grant the supervisor wrote into this step's own
    /// environment, for a step whose plan carries `observeDescendants`. A
    /// step without the flag has no grant and never sees the observer.
    observation: Option<ObservationEnv>,
    echo_args: bool,
}

/// The observation grant as it arrived through this step's own environment.
/// Handle **values** travel here; the handles themselves arrive only through
/// the explicit minimal `HANDLE_LIST`, written inheritable by their creator.
/// The nonce is a correlation token, never a handle, and never a member of
/// any `HANDLE_LIST`.
#[cfg(windows)]
#[derive(Clone, Debug)]
struct ObservationEnv {
    request_write: usize,
    response_read: usize,
    observer_handle: usize,
    nonce: String,
    fixture_id: u32,
}

/// The environment keys. Must match the supervisor's `observation_env`
/// module character for character — there is no shared module by design,
/// because the fixture must stay buildable without the supervisor.
#[cfg(windows)]
mod observation_env_keys {
    pub const HANDLE: &str = "FINTWIND_OBSERVATION_HANDLE";
    pub const NONCE: &str = "FINTWIND_OBSERVATION_NONCE";
    pub const REQUEST: &str = "FINTWIND_OBSERVATION_REQUEST";
    pub const RESPONSE: &str = "FINTWIND_OBSERVATION_RESPONSE";
    pub const FIXTURE_ID: &str = "FINTWIND_OBSERVATION_FIXTURE_ID";
}

/// Read the observation grant from this process's environment.
///
/// All five keys together mean "this step is observed"; none of them means
/// "it is not". **Some** of them is a broken grant — the supervisor that
/// wrote half a channel wrote nothing usable — and the fixture refuses to
/// run rather than guessing which half was intended.
#[cfg(windows)]
fn observation_from_env() -> Result<Option<ObservationEnv>, String> {
    use observation_env_keys as keys;
    let value = |name: &str| std::env::var(name).ok();
    let request = value(keys::REQUEST);
    let response = value(keys::RESPONSE);
    let observer = value(keys::HANDLE);
    let nonce = value(keys::NONCE);
    let fixture_id = value(keys::FIXTURE_ID);
    match (request, response, observer, nonce, fixture_id) {
        (None, None, None, None, None) => Ok(None),
        (
            Some(request),
            Some(response),
            Some(observer),
            Some(nonce),
            Some(fixture_id),
        ) => {
            let parse = |text: &str, what: &str| {
                text.parse::<usize>()
                    .map_err(|_| format!("{what} is not a number"))
            };
            Ok(Some(ObservationEnv {
                request_write: parse(&request, keys::REQUEST)?,
                response_read: parse(&response, keys::RESPONSE)?,
                observer_handle: parse(&observer, keys::HANDLE)?,
                nonce,
                fixture_id: fixture_id
                    .parse::<u32>()
                    .map_err(|_| format!("{} is not a number", keys::FIXTURE_ID))?,
            }))
        }
        _ => Err(
            "the observation environment is partially present; a granted \
             channel always names all five values, so this grant is broken"
                .to_owned(),
        ),
    }
}

fn take_value(arguments: &mut Vec<String>, name: &str) -> Result<String, String> {
    // parse_options already consumed the option name.
    if arguments.is_empty() || arguments[0].starts_with("--") {
        return Err(format!("{name} requires a value"));
    }
    Ok(arguments.remove(0))
}

fn parse_options(arguments: &[String]) -> Result<Options, String> {
    let mut arguments = arguments.to_vec();
    let mut options = Options::default();
    while !arguments.is_empty() {
        let flag = arguments.remove(0);
        match flag.as_str() {
            "--facts" => options.facts = Some(PathBuf::from(take_value(&mut arguments, &flag)?)),
            "--stay-alive" => options.stay_alive = true,
            "--exit-code" => {
                options.exit_code = take_value(&mut arguments, &flag)?
                    .parse()
                    .map_err(|_| "--exit-code is not a number".to_owned())?
            }
            "--sleep-ms" => {
                options.sleep_ms = take_value(&mut arguments, &flag)?
                    .parse()
                    .map_err(|_| "--sleep-ms is not a number".to_owned())?
            }
            "--ready-mode" => {
                options.ready_mode = ReadyMode::parse(&take_value(&mut arguments, &flag)?)?
            }
            "--ready-delay-ms" => {
                options.ready_delay_ms = take_value(&mut arguments, &flag)?
                    .parse()
                    .map_err(|_| "--ready-delay-ms is not a number".to_owned())?
            }
            "--junk-bytes" => {
                options.junk_bytes = take_value(&mut arguments, &flag)?
                    .parse()
                    .map_err(|_| "--junk-bytes is not a number".to_owned())?
            }
            "--grandchild" => options.grandchild = true,
            "--grandchild-facts" => {
                options.grandchild_facts = Some(PathBuf::from(take_value(&mut arguments, &flag)?))
            }
            "--substitute-out" => {
                options.substitute_out = Some(PathBuf::from(take_value(&mut arguments, &flag)?))
            }
            "--substitute-field" => {
                options.substitute_field = Some(take_value(&mut arguments, &flag)?)
            }
            "--parent-handle" => {
                options.parent_handle = Some(
                    take_value(&mut arguments, &flag)?
                        .parse()
                        .map_err(|_| "--parent-handle is not a number".to_owned())?,
                )
            }
            "--handle-out" => {
                options.handle_out = Some(PathBuf::from(take_value(&mut arguments, &flag)?))
            }
            "--echo-args" => options.echo_args = true,
            // A positional argument carries no flag: it is whatever a later
            // plan step substituted into this role's argv, and it is reported
            // through --echo-args rather than through any output stream.
            other if !other.starts_with("--") => {}
            other => return Err(format!("unknown flag {other:?}")),
        }
    }
    if options.facts.is_none() {
        return Err("--facts is required".to_owned());
    }
    if options.parent_handle.is_some() != options.handle_out.is_some() {
        return Err("--parent-handle and --handle-out belong together".to_owned());
    }
    Ok(options)
}

/// Write the facts atomically: a reader must never observe a half-written file.
fn write_facts(path: &PathBuf, facts: &Facts) {
    let Ok(text) = serde_json::to_string_pretty(facts) else {
        return;
    };
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, text).is_err() {
        return;
    }
    let _ = std::fs::rename(&temporary, path);
}

fn main() {
    #[cfg(not(windows))]
    {
        eprintln!("the ownership fixture requires Windows job containment");
        std::process::exit(3);
    }
    #[cfg(windows)]
    fixture_main();
}

#[cfg(windows)]
fn fixture_main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let echoed = raw.clone();
    let options = match parse_options(&raw) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("fixture: {error}");
            std::process::exit(2);
        }
    };

    let pid = win::current_process_id();
    // The observation grant comes from the environment, never from argv: a
    // plan is frozen and hashed, so the supervisor injects the grant per
    // step, and only for a step whose plan asked for it.
    let options = match observation_from_env() {
        Ok(observation) => Options { observation, ..options },
        Err(error) => {
            eprintln!("fixture: {error}");
            std::process::exit(2);
        }
    };

    // The supervisor's readiness contract wants a real loopback socket, so bind
    // one on an ephemeral port and keep it open for the whole role.
    let listener = if options.ready_mode != ReadyMode::None {
        std::net::TcpListener::bind("127.0.0.1:0").ok()
    } else {
        None
    };
    let port = listener
        .as_ref()
        .and_then(|listener| listener.local_addr().ok())
        .map(|address| address.port())
        .unwrap_or(0);

    let facts_path = options.facts.clone().expect("checked above");
    let emit = |grandchild_pid: Option<u32>, grandchild_in_job: Option<bool>| {
        write_facts(
            &facts_path,
            &Facts {
                marker: MARKER,
                fixturePid: pid,
                inAnyJob: win::in_any_job(win::current_process()),
                grandchildPid: grandchild_pid,
                grandchildInAnyJob: grandchild_in_job,
                substituteField: options.substitute_field.clone(),
                echoedArgs: if options.echo_args {
                    Some(echoed.clone())
                } else {
                    None
                },
                readyMode: options.ready_mode.name(),
            },
        );
    };
    emit(None, None);

    // A large, newline-free burst is what trips the supervisor's 64 KiB
    // readiness cap; every byte before the first outcome is counted.
    if options.junk_bytes > 0 {
        let chunk = vec![b'x'; 4096];
        let mut written = 0_usize;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        while written < options.junk_bytes {
            let take = (options.junk_bytes - written).min(chunk.len());
            if lock.write_all(&chunk[..take]).is_err() {
                break;
            }
            written += take;
        }
        let _ = lock.flush();
    }

    if options.ready_delay_ms > 0 && options.ready_mode != ReadyMode::WrongThenCorrect {
        std::thread::sleep(Duration::from_millis(options.ready_delay_ms));
    }

    let ready_line = |pid_override: Option<u32>, protocol_override: Option<u32>, host: &str| {
        format!(
            "{{\"address\":\"{host}:{port}\",\"protocolVersion\":{},\"pid\":{}}}\n",
            protocol_override.unwrap_or(fintwind_protocol::PROTOCOL_VERSION),
            pid_override.unwrap_or(pid)
        )
    };
    match options.ready_mode {
        ReadyMode::None => {}
        ReadyMode::Correct => print!("{}", ready_line(None, None, "127.0.0.1")),
        ReadyMode::WrongPid => print!("{}", ready_line(Some(pid + 1), None, "127.0.0.1")),
        ReadyMode::WrongProtocol => print!(
            "{}",
            ready_line(
                None,
                Some(fintwind_protocol::PROTOCOL_VERSION + 1),
                "127.0.0.1"
            )
        ),
        ReadyMode::WrongAddress => print!("{}", ready_line(None, None, "0.0.0.0")),
        ReadyMode::WrongThenCorrect => {
            print!("{}", ready_line(Some(pid + 1), None, "127.0.0.1"));
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_millis(options.ready_delay_ms.max(250)));
            print!("{}", ready_line(None, None, "127.0.0.1"));
        }
    }
    let _ = std::io::stdout().flush();

    if options.grandchild {
        match win::spawn_grandchild(options.grandchild_facts.as_deref()) {
            Ok(grandchild) => {
                let grandchild_pid = Some(grandchild.pid);
                let grandchild_in_job = Some(win::in_any_job(grandchild.process));
                if let (Some(parent), Some(handle_out)) =
                    (options.parent_handle, options.handle_out.as_ref())
                {
                    if let Some(duplicated) = win::duplicate_into(parent, grandchild.process) {
                        let _ = std::fs::write(
                            handle_out,
                            format!("{} {}\n", duplicated as usize, grandchild.pid),
                        );
                    }
                }
                emit(grandchild_pid, grandchild_in_job);
                if !handoff_descendant(&options, &grandchild) {
                    // A granted channel that never delivered the
                    // descendant's handle is a failed observation, not a
                    // green step: the supervisor's verified exit would
                    // otherwise stand in for a hand-off that never
                    // happened.
                    std::process::exit(5);
                }
                // The handle must stay open for the whole role: the black box
                // verifies containment from outside, and a closed handle here
                // would only prove something about this fixture.
                std::mem::forget(grandchild);
            }
            Err(error) => {
                eprintln!("fixture: could not create the grandchild: {error}");
                std::process::exit(4);
            }
        }
    }

    if let (Some(path), Some(field)) = (options.substitute_out, options.substitute_field) {
        // Synthetic value only: the black box proves the substitution happened
        // by echoing argv, never by trusting the value itself.
        let mut map = serde_json::Map::new();
        map.insert(
            field,
            serde_json::Value::String("synthetic-seed-project".to_owned()),
        );
        if let Ok(text) = serde_json::to_string(&serde_json::Value::Object(map)) {
            let _ = std::fs::write(path, text);
        }
    }

    if options.sleep_ms > 0 {
        std::thread::sleep(Duration::from_millis(options.sleep_ms));
    }

    if options.stay_alive {
        loop {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    std::process::exit(options.exit_code as i32);
}

/// Hand the grandchild's creation handle to the observer, over the observation
/// channel the supervisor granted this step.
///
/// Returns `true` when there was no channel (this step is not observed, and
/// the role proceeds normally) or when the handle was handed over and
/// accepted. A **granted** channel that could not deliver the handle returns
/// `false`: the caller fails the step rather than exiting green, because an
/// observation the observer never received is not a completed observation.
///
/// The fixture is the grandchild's creator, so it is the only party that can
/// perform this duplication: nobody opens a pid to obtain the handle. The
/// observer's handle arrives by inheritance — the same provenance the
/// supervisor itself uses — and its *value* arrives through this step's own
/// environment, written by the supervisor and only for a step whose plan
/// carries `observeDescendants`.
#[cfg(windows)]
fn handoff_descendant(options: &Options, grandchild: &win::Grandchild) -> bool {
    use windows_sys::Win32::Foundation::HANDLE;
    let Some(grant) = options.observation.as_ref() else {
        eprintln!(
            "fixture: no observation channel was granted for this step, so the grandchild's handle is not handed over"
        );
        return true;
    };
    let channel = match observation::SubjectChannel::from_raw(
        observation::WriterId::fixture(grant.fixture_id),
        grant.request_write as HANDLE,
        grant.response_read as HANDLE,
    ) {
        Ok(channel) => channel,
        Err(error) => {
            eprintln!("fixture: could not open the observation channel: {error}");
            return false;
        }
    };
    let duplicated = match observation::duplicate_for_observer(
        grant.observer_handle as HANDLE,
        grandchild.process,
        observation::OBSERVER_PROCESS_ACCESS,
    ) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("fixture: could not hand the grandchild handle over: {error}");
            return false;
        }
    };
    let message = observation::Handoff {
        protocol: observation::PROTOCOL.to_owned(),
        nonce: grant.nonce.clone(),
        source: observation::Source::Fixture,
        fixture: Some(grant.fixture_id),
        transfer_id: grandchild.pid as u64,
        kind: observation::Kind::Descendant,
        claimed_pid: grandchild.pid,
        process_handle: Some(duplicated as u64),
        job_handle: None,
        pipe_request_read: None,
        pipe_response_write: None,
        granted_access: "PROCESS_QUERY_LIMITED_INFORMATION|SYNCHRONIZE".to_owned(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let delivered = match channel.handoff(&message, deadline) {
        Ok(ack) if ack.accepted => {
            eprintln!("fixture: the grandchild handle was accepted by the observer");
            true
        }
        Ok(ack) => {
            eprintln!("fixture: the observer refused the hand-off: {}", ack.reason);
            false
        }
        Err(error) => {
            eprintln!("fixture: the hand-off did not complete: {error}");
            false
        }
    };
    // After this point the role creates no further process: the topology is
    // frozen from the fixture's side.
    delivered
}

/// The thinnest possible Win32 layer: exactly what this fixture needs, and
/// nothing that is not already enabled in this crate's `windows-sys` features.
#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetCurrentProcess, GetCurrentProcessId, PROCESS_INFORMATION, STARTUPINFOW,
    };

    /// One child this fixture created itself. The handle is a creation handle,
    /// so nothing downstream ever has to open a pid.
    pub struct Grandchild {
        pub pid: u32,
        pub process: HANDLE,
    }

    impl Drop for Grandchild {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.process);
            }
        }
    }

    pub fn current_process() -> HANDLE {
        unsafe { GetCurrentProcess() }
    }

    pub fn current_process_id() -> u32 {
        unsafe { GetCurrentProcessId() }
    }

    /// True when the process sits in some job. A null job handle asks the
    /// kernel about *any* job, which is why the black box always pairs this
    /// with a job-handle comparison somewhere else.
    pub fn in_any_job(process: HANDLE) -> bool {
        let mut result: i32 = 0;
        let ok = unsafe { IsProcessInJob(process, std::ptr::null_mut(), &mut result) };
        ok != 0 && result != 0
    }

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

    /// Create the grandchild. No handle inheritance and no stdio redirection:
    /// the grandchild's only output channel is its own facts file, so an
    /// invalid std handle is never touched.
    pub fn spawn_grandchild(
        grandchild_facts: Option<&std::path::Path>,
    ) -> Result<Grandchild, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("could not resolve the fixture executable: {error}"))?;
        let grandchild_facts = grandchild_facts
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let arguments = [
            "--facts".to_owned(),
            grandchild_facts,
            "--stay-alive".to_owned(),
            "--exit-code".to_owned(),
            "0".to_owned(),
        ];
        let mut command = quote(&executable.to_string_lossy());
        for argument in &arguments {
            command.push(' ');
            command.push_str(&quote(argument));
        }

        let mut command_wide = wide(&command);
        let mut application_wide = wide(&executable.to_string_lossy());
        let mut startup = STARTUPINFOW::default();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup.dwFlags = windows_sys::Win32::System::Threading::STARTF_USESHOWWINDOW;
        startup.wShowWindow = 0;
        let mut information = PROCESS_INFORMATION::default();
        let created = unsafe {
            CreateProcessW(
                application_wide.as_mut_ptr(),
                command_wide.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut information,
            )
        };
        if created == 0 {
            return Err(format!(
                "CreateProcessW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        unsafe {
            let _ = CloseHandle(information.hThread);
        }
        Ok(Grandchild {
            pid: information.dwProcessId,
            process: information.hProcess,
        })
    }

    /// Copy a handle this fixture owns into another process it was handed a
    /// real handle to. The returned value is a handle **in the target
    /// process's** table; only that process may use it.
    pub fn duplicate_into(target_process: usize, source: HANDLE) -> Option<HANDLE> {
        let mut duplicated: HANDLE = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                current_process(),
                source,
                target_process as HANDLE,
                &mut duplicated,
                0,
                0,
                2, // DUPLICATE_SAME_ACCESS
            )
        };
        if ok == 0 {
            return None;
        }
        Some(duplicated)
    }
}
