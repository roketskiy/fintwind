//! Package-manager behavior against an isolated fake npm and loopback server.
//! The fixture never starts a daemon or invokes the user's npm or OpenCode.

use anyhow::{Context, Result, ensure};
use fintwind_core::{
    model,
    opencode_management::{self, PackageManager, PackageManagerKind},
};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let executable = std::env::current_exe()?;
    if args.get(1).is_some_and(|arg| arg == "--version") {
        println!(
            "{}",
            std::fs::read_to_string(executable.with_extension("fixture-version"))
                .unwrap_or_else(|_| "2.0.0".into())
        );
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "install") {
        ensure!(args.iter().any(|arg| arg == "-g"));
        let is_bun = executable.file_stem().is_some_and(|name| name == "bun");
        if is_bun {
            ensure!(args.iter().any(|arg| arg == "--trust"));
        } else {
            ensure!(args.iter().any(|arg| arg == "--allow-scripts=@opencode/cli"));
        }
        let bin = PathBuf::from(std::fs::read_to_string(
            executable.with_extension("fixture-bin"),
        )?);
        let version = args
            .iter()
            .find_map(|arg| arg.strip_prefix("@opencode/cli@"))
            .context("missing fixture version")?;
        let target = bin.join(if cfg!(windows) {
            "opencode.exe"
        } else {
            "opencode"
        });
        let package = if is_bun {
            bin.parent().unwrap().join("install/global/node_modules/@opencode/cli")
        } else if cfg!(windows) {
            bin.join("node_modules/@opencode/cli")
        } else {
            bin.parent().unwrap().join("lib/node_modules/@opencode/cli")
        };
        std::fs::create_dir_all(&package)?;
        std::fs::write(package.join("package.json"), b"{}")?;
        std::fs::copy(&executable, &target)?;
        std::fs::write(target.with_extension("fixture-version"), version)?;
        return Ok(());
    }
    run()
}

fn run() -> Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("pass an artifact directory")?,
    )
    .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let bin = root.join("global-bin");
    std::fs::create_dir(&bin)?;
    let npm = root.join(if cfg!(windows) { "npm.exe" } else { "npm" });
    std::fs::copy(std::env::current_exe()?, &npm)?;
    std::fs::write(
        npm.with_extension("fixture-bin"),
        bin.to_string_lossy().as_bytes(),
    )?;
    std::fs::write(npm.with_extension("fixture-version"), "12.2.0")?;
    let manager = PackageManager::new(PackageManagerKind::Npm, npm, bin.clone());
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let url = format!("http://{}/latest", listener.local_addr()?);
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(90);
        while !server_stop.load(Ordering::Acquire) && Instant::now() < deadline {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request);
            let body = br#"{"version":"2.0.0"}"#;
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(body);
        }
    });
    let result = (|| -> Result<Vec<&'static str>> {
        let mut checks = Vec::new();
        let latest = opencode_management::latest_version_from(&url)?;
        ensure!(latest == "2.0.0");
        ensure!(std::fs::read_dir(&bin)?.next().is_none());
        checks.push("checking latest metadata does not install or update anything");
        let target = opencode_management::install_with(&manager, &latest)?;
        ensure!(model::probe_provider_version(&target).as_deref() == Some("2.0.0"));
        checks.push("installation uses the chosen package manager and verifies the resulting CLI");
        ensure!(opencode_management::install_with(&manager, &latest).is_err());
        checks.push("installation refuses an existing executable");
        let other = root.join("other.exe");
        std::fs::copy(&target, &other)?;
        ensure!(opencode_management::upgrade_with(&manager, &other, "2.1.0").is_err());
        ensure!(model::probe_provider_version(&target).as_deref() == Some("2.0.0"));
        checks.push("update refuses an executable outside the selected manager's bin directory");
        ensure!(opencode_management::upgrade_with(&manager, &target, "1.0.0").is_err());
        ensure!(model::probe_provider_version(&target).as_deref() == Some("2.0.0"));
        checks.push("update refuses a downgrade");
        let version = opencode_management::upgrade_with(&manager, &target, "2.1.0")?;
        ensure!(version == "2.1.0");
        checks.push("explicit update uses the manager and rechecks the selected executable");
        let bun_root = root.join("bun-global");
        let bun_bin = bun_root.join("bin");
        std::fs::create_dir_all(&bun_bin)?;
        let bun = bun_root.join(if cfg!(windows) { "bun.exe" } else { "bun" });
        std::fs::copy(std::env::current_exe()?, &bun)?;
        std::fs::write(bun.with_extension("fixture-bin"), bun_bin.to_string_lossy().as_bytes())?;
        let bun_manager = PackageManager::new(PackageManagerKind::Bun, bun, bun_bin);
        let bun_target = opencode_management::install_with(&bun_manager, "2.0.0")?;
        ensure!(opencode_management::upgrade_with(&bun_manager, &bun_target, "2.1.0")? == "2.1.0");
        checks.push("Bun installation and update use trusted lifecycle scripts");
        Ok(checks)
    })();
    stop.store(true, Ordering::Release);
    let _ = server.join();
    let checks = result?;
    std::fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&json!({"passed": true, "checks": checks}))?,
    )?;
    println!("{} checks passed; {}", checks.len(), root.display());
    Ok(())
}
