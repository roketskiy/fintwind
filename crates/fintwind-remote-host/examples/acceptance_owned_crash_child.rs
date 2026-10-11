//! Isolated acceptance only. Termination uses Child's creation handle, never PID lookup.
use std::{io::{BufRead, Read, Write}, path::Path, process::{Command, Stdio}, sync::mpsc,
    time::{Duration, Instant}};
use anyhow::{bail, Context as _, Result};

fn main() -> Result<()> {
    #[cfg(not(windows))]
    bail!("Windows-only acceptance helper");
    #[cfg(windows)]
    run()
}

#[cfg(windows)]
fn run() -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::{JobObjects::IsProcessInJob, Threading::GetCurrentProcess};
    let mut member = 0;
    if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut member) } == 0 || member == 0 {
        bail!("acceptance wrapper must inherit its supervisor job");
    }
    let mut args = std::env::args().skip(1);
    let nonce = args.next().context("missing control nonce")?;
    if nonce.len() != 64 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid control nonce");
    }
    let program = args.next().context("missing frozen executable")?;
    let name = Path::new(&program).file_name().and_then(|name| name.to_str()).unwrap_or("");
    if !matches!(name, "fintwind-remote-host.exe" | "fintwind-daemon.exe") {
        bail!("unsupported acceptance target");
    }
    let args: Vec<String> = args.collect();
    let data = args.windows(2).find(|pair| pair[0] == "--data-dir")
        .context("isolated data directory is required")?;
    if std::fs::read(Path::new(&data[1]).join(".fintwind-owned-crash-fixture"))? != b"synthetic\n" {
        bail!("missing isolated fixture witness");
    }
    let mut child = Command::new(program).args(args).stdin(Stdio::null())
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn()?;
    println!("{}", serde_json::json!({"type":"ownedCrashReady", "childPid":child.id()}));
    std::io::stdout().flush()?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut input = String::new();
        let result = std::io::stdin().lock().take(4097).read_line(&mut input)
            .map(|size| (size, input));
        let _ = tx.send(result);
    });
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("acceptance target exited before injection: {status}");
        }
        match rx.try_recv() {
            Ok(result) => {
                let (size, input) = result?;
                let message: serde_json::Value = serde_json::from_str(&input)?;
                if size > 4096 || message.as_object().map(|value| value.len()) != Some(2)
                    || message["type"] != "crash" || message["nonce"] != nonce {
                    bail!("refused crash command");
                }
                // Rust's Windows Child::kill calls TerminateProcess on the handle
                // retained by spawn. Do not replace this with PID-based tooling.
                child.kill()?;
                let status = child.wait()?;
                let code = status.code().context("missing Windows exit code")?;
                if code == 0 { bail!("injection did not produce a crash exit"); }
                println!("{}", serde_json::json!({"type":"ownedCrashConfirmed", "exitCode":code}));
                std::io::stdout().flush()?;
                return Ok(());
            }
            Err(mpsc::TryRecvError::Disconnected) => bail!("crash control pipe closed"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if started.elapsed() > Duration::from_secs(110) { bail!("injection deadline exceeded"); }
        std::thread::sleep(Duration::from_millis(20));
    }
}
