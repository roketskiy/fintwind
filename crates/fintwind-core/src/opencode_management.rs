//! Explicit OpenCode operations on the daemon host, never on the UI thread.

use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const LATEST_URL: &str = "https://opencode.ai/update/api/latest/cli/npm";

pub fn latest_version_from(url: &str) -> Result<String> {
    let (status, body, _) = fintwind_protocol::http::http_json("GET", url, &[], None, 20)?;
    ensure!(
        status == 200,
        "OpenCode version service returned HTTP {status}"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(&body).context("invalid OpenCode version response")?;
    let version = metadata
        .get("version")
        .and_then(|value| value.as_str())
        .context("OpenCode version is missing")?;
    let parsed = Version::parse(version).context("invalid OpenCode version")?;
    ensure!(parsed.major >= 2, "unsupported OpenCode version");
    Ok(version.to_owned())
}

pub fn is_newer(current: &str, latest: &str) -> Result<bool> {
    Ok(Version::parse(latest)? > Version::parse(current)?)
}

#[derive(Clone, Copy)]
pub enum PackageManagerKind {
    Npm,
    Bun,
}

pub struct PackageManager {
    kind: PackageManagerKind,
    executable: PathBuf,
    bin: PathBuf,
}

impl PackageManager {
    pub fn new(kind: PackageManagerKind, executable: PathBuf, bin: PathBuf) -> Self {
        Self {
            kind,
            executable,
            bin,
        }
    }

    fn discover(kind: PackageManagerKind) -> Result<Self> {
        let name = match kind {
            PackageManagerKind::Npm => "npm",
            PackageManagerKind::Bun => "bun",
        };
        let executable = crate::command_env::find_executable(name)
            .with_context(|| format!("{name} was not found"))?;
        let mut command = package_command(&executable);
        match kind {
            PackageManagerKind::Npm => {
                command.args(["prefix", "-g"]);
            }
            PackageManagerKind::Bun => {
                command.args(["pm", "bin", "-g"]);
            }
        }
        let output = run_command(&mut command, Duration::from_secs(15))?;
        let directory = String::from_utf8_lossy(&output.stdout)
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_owned();
        let directory = PathBuf::from(directory);
        ensure!(
            directory.is_absolute(),
            "{name} did not report an absolute global directory"
        );
        let bin = match kind {
            PackageManagerKind::Npm if !cfg!(windows) => directory.join("bin"),
            _ => directory,
        };
        Ok(Self::new(kind, executable, bin))
    }

    fn candidates(&self) -> Vec<PathBuf> {
        if cfg!(windows) {
            ["opencode.exe", "opencode.cmd", "opencode.bat"]
                .into_iter()
                .map(|name| self.bin.join(name))
                .collect()
        } else {
            vec![self.bin.join("opencode")]
        }
    }

    fn installed_binary(&self) -> Option<PathBuf> {
        self.candidates().into_iter().find(|path| path.is_file())
    }

    fn owns(&self, binary: &Path) -> bool {
        let Ok(target) = binary.canonicalize() else {
            return false;
        };
        let root = match self.kind {
            PackageManagerKind::Npm if cfg!(windows) => self.bin.join("node_modules"),
            PackageManagerKind::Npm => self
                .bin
                .parent()
                .unwrap_or(&self.bin)
                .join("lib/node_modules"),
            PackageManagerKind::Bun => self
                .bin
                .parent()
                .unwrap_or(&self.bin)
                .join("install/global/node_modules"),
        };
        let package = root.join("@opencode/cli");
        if !package.join("package.json").is_file() {
            return false;
        }
        if self
            .candidates()
            .iter()
            .filter_map(|path| path.canonicalize().ok())
            .any(|path| path == target)
        {
            return true;
        }
        let name = if cfg!(windows) {
            "opencode.exe"
        } else {
            "opencode"
        };
        package.join("bin")
            .join(name)
            .canonicalize()
            .is_ok_and(|path| path == target)
    }

    fn install_version(&self, version: &str) -> Result<()> {
        let parsed = Version::parse(version).context("invalid OpenCode version")?;
        ensure!(parsed.major >= 2, "unsupported OpenCode version");
        let mut command = package_command(&self.executable);
        command.args(["install", "-g"]);
        match self.kind {
            PackageManagerKind::Bun => {
                command.arg("--trust");
            }
            PackageManagerKind::Npm => {
                // npm 12 blocks postinstall scripts unless the package is approved.
                let mut probe = package_command(&self.executable);
                probe.arg("--version");
                let output = run_command(&mut probe, Duration::from_secs(15))?;
                let version = String::from_utf8_lossy(&output.stdout);
                if version
                    .trim()
                    .split('.')
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .is_some_and(|major| major >= 12)
                {
                    command.arg("--allow-scripts=@opencode/cli");
                }
            }
        }
        command.arg(format!("@opencode/cli@{version}"));
        run_command(&mut command, Duration::from_secs(240))?;
        Ok(())
    }
}

fn package_command(executable: &Path) -> Command {
    let mut command = crate::command_env::command(executable);
    if let Some(path) = crate::command_env::executable_search_path() {
        command.env("PATH", path);
    }
    if let Some(proxy) = fintwind_protocol::http::active_proxy() {
        command.env("HTTP_PROXY", &proxy).env("HTTPS_PROXY", &proxy)
            .env("http_proxy", &proxy).env("https_proxy", &proxy);
    }
    command
}

fn run_command(command: &mut Command, timeout: Duration) -> Result<std::process::Output> {
    let output = crate::command_env::output_with_timeout(command, timeout)?
        .context("package manager timed out")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!(
            "package manager failed: {}",
            detail.chars().take(800).collect::<String>()
        );
    }
    Ok(output)
}

pub fn install(version: &str) -> Result<PathBuf> {
    let manager = PackageManager::discover(PackageManagerKind::Npm)
        .or_else(|_| PackageManager::discover(PackageManagerKind::Bun))
        .context("install npm or Bun before installing OpenCode")?;
    install_with(&manager, version)
}

pub fn install_with(manager: &PackageManager, version: &str) -> Result<PathBuf> {
    ensure!(
        manager.installed_binary().is_none(),
        "OpenCode already exists in the package manager's global bin directory"
    );
    manager.install_version(version)?;
    let binary = manager
        .installed_binary()
        .context("package manager did not install an OpenCode executable")?;
    let installed = crate::model::probe_provider_version(&binary)
        .context("installed OpenCode could not report its version")?;
    ensure!(
        Version::parse(&installed)? == Version::parse(version)?,
        "installed OpenCode version does not match the requested version"
    );
    Ok(binary)
}

pub fn upgrade(binary: &Path, version: &str) -> Result<String> {
    let manager = [PackageManagerKind::Npm, PackageManagerKind::Bun]
        .into_iter()
        .filter_map(|kind| PackageManager::discover(kind).ok())
        .find(|manager| manager.owns(binary))
        .context(
            "the selected OpenCode is not managed by the current npm or Bun global installation",
        )?;
    upgrade_with(&manager, binary, version)
}

pub fn upgrade_with(manager: &PackageManager, binary: &Path, version: &str) -> Result<String> {
    ensure!(
        manager.owns(binary),
        "the selected OpenCode is not managed by this package manager"
    );
    let before = crate::model::probe_provider_version(binary)
        .context("could not read the current OpenCode version")?;
    ensure!(
        is_newer(&before, version)?,
        "the requested version is not newer than the installed version"
    );
    manager.install_version(version)?;
    let after = crate::model::probe_provider_version(binary)
        .context("could not verify the updated OpenCode version")?;
    ensure!(
        Version::parse(&after)? >= Version::parse(version)?,
        "the selected OpenCode executable was not updated"
    );
    Ok(after)
}
