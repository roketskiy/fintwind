use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail, ensure};
use fintwind_protocol::{WorkspaceOperation, WorkspaceResult};
use serde_json::{Value, json};

fn isolated_command(program: &Path, root: &Path, fixture: &Path, git: &Path) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("PATH", fixture)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root.join("appdata"))
        .env("LOCALAPPDATA", root.join("localappdata"))
        .env("TEMP", root)
        .env("TMP", root)
        .env("GH_CONFIG_DIR", root.join("gh-config"))
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("FINTWIND_PR_REAL_GIT", git)
        .env("FINTWIND_PR_FIXTURE_LOG", root.join("gh-requests.jsonl"));
    command
}

fn fixture_gh() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.starts_with(&["pr".into(), "list".into()]),
        "unexpected gh command"
    );
    ensure!(
        args.windows(2)
            .any(|pair| pair == ["--repo", "fixture/project"]),
        "unexpected repository arguments: {args:?}"
    );
    ensure!(
        args.windows(2)
            .any(|pair| pair == ["--head", "feature/card"])
    );
    ensure!(args.windows(2).any(|pair| pair == ["--state", "all"]));
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var_os("FINTWIND_PR_FIXTURE_LOG").context("fixture log")?)?;
    use std::io::Write;
    writeln!(log, "{}", serde_json::to_string(&args)?)?;
    let mode = std::env::var("FINTWIND_PR_FIXTURE_MODE")?;
    match mode.as_str() {
        "auth" => {
            eprintln!("authentication required; run gh auth login");
            std::process::exit(4);
        }
        "network" => bail!("fixture network unavailable"),
        "malformed" => println!("{{\"unexpected\":true}}"),
        "empty" => println!("[]"),
        mode => {
            let state = if mode == "merged" { "MERGED" } else { "OPEN" };
            let checks = match mode {
                "pending" => json!([{ "status":"IN_PROGRESS", "conclusion":null }]),
                "failed" => json!([{ "status":"COMPLETED", "conclusion":"FAILURE" }]),
                "startup-failure" => {
                    json!([{ "status":"COMPLETED", "conclusion":"STARTUP_FAILURE" }])
                }
                "no-checks" => json!([]),
                _ => {
                    json!([{ "status":"COMPLETED", "conclusion":"SUCCESS" }, { "state":"SUCCESS" }])
                }
            };
            println!(
                "{}",
                json!([{
                    "number":42, "title":"Round checkout totals", "state":state,
                    "isDraft":mode == "draft", "url": if mode == "bad-url" { "https://example.invalid/untrusted" } else { "https://github.com/fixture/project/pull/42" },
                    "headRefName":"feature/card", "baseRefName":"main", "author":{"login":"octocat"},
                    "additions":120, "deletions":9, "changedFiles":5, "comments":[{},{}],
                    "updatedAt":"2026-10-20T21:35:39Z", "statusCheckRollup":checks
                }])
            );
        }
    }
    Ok(())
}

fn verify_child(cwd: PathBuf) -> Result<()> {
    let operation = WorkspaceOperation::InspectPullRequests { cwd };
    let operation: WorkspaceOperation = serde_json::from_value(serde_json::to_value(operation)?)?;
    let result = fintwind_core::workspace::execute(operation)?;
    ensure!(matches!(result, WorkspaceResult::PullRequests { .. }));
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn checked(output: Output) -> Result<Vec<u8>> {
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn main() -> Result<()> {
    let executable = std::env::current_exe()?;
    let stem = executable
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if stem == "gh" {
        return fixture_gh();
    }
    if stem == "git" {
        let git = PathBuf::from(std::env::var_os("FINTWIND_PR_REAL_GIT").context("real git")?);
        let output = Command::new(&git)
            .args(std::env::args_os().skip(1))
            .env("PATH", git.parent().context("git directory")?)
            .output()?;
        use std::io::Write;
        std::io::stdout().write_all(&output.stdout)?;
        std::io::stderr().write_all(&output.stderr)?;
        std::process::exit(output.status.code().unwrap_or(1));
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--inspect") {
        return verify_child(PathBuf::from(args.get(1).context("workspace path")?));
    }
    let git = PathBuf::from(
        args.first()
            .context("pass the absolute path to git as the first argument")?,
    );
    ensure!(
        git.is_absolute() && git.is_file(),
        "git must be an existing absolute path"
    );
    let root = std::env::current_dir()?
        .join("target/pull-requests-blackbox")
        .join(uuid::Uuid::new_v4().to_string());
    let fixture = root.join("bin");
    let repository = root.join("workspace");
    let outside = root.join("outside");
    for path in [&fixture, &repository, &outside, &root.join("gh-config")] {
        fs::create_dir_all(path)?;
    }
    fs::write(root.join("gitconfig"), "")?;
    for name in ["git", "gh"] {
        fs::copy(
            &executable,
            fixture.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
        )?;
    }
    let git_run = |args: &[&str]| -> Result<Vec<u8>> {
        checked(
            isolated_command(&git, &root, &fixture, &git)
                .current_dir(&repository)
                .args(args)
                .output()?,
        )
    };
    git_run(&["init", "-b", "feature/card"])?;
    git_run(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "core.hooksPath=",
        "commit",
        "--allow-empty",
        "-m",
        "fixture",
    ])?;
    git_run(&[
        "remote",
        "add",
        "origin",
        "git@github.com:fixture/project.git",
    ])?;
    let before_head = git_run(&["rev-parse", "HEAD"])?;
    let before_config = fs::read(repository.join(".git/config"))?;
    let invoke = |mode: &str, cwd: &Path| -> Result<Output> {
        Ok(isolated_command(&executable, &root, &fixture, &git)
            .args([OsString::from("--inspect"), cwd.as_os_str().to_owned()])
            .env("FINTWIND_PR_FIXTURE_MODE", mode)
            .output()?)
    };
    let inspect = |mode: &str, cwd: &Path| -> Result<Value> {
        let output = checked(invoke(mode, cwd)?)?;
        Ok(serde_json::from_slice::<Value>(&output)?["snapshot"].clone())
    };
    let expected_request = json!({
        "number":42, "title":"Round checkout totals", "url":"https://github.com/fixture/project/pull/42", "state":"merged",
        "headBranch":"feature/card", "baseBranch":"main", "author":"octocat", "additions":120, "deletions":9,
        "changedFiles":5, "comments":2, "updatedAt":"2026-10-20T21:35:39Z", "checks":"passed"
    });
    ensure!(
        inspect("merged", &repository)?
            == json!({"type":"ready", "repository":"fixture/project", "branch":"feature/card", "requests":[expected_request]}),
        "merged PR card lost information"
    );
    for (mode, state, checks) in [
        ("draft", "draft", "passed"),
        ("pending", "open", "pending"),
        ("failed", "open", "failed"),
        ("startup-failure", "open", "failed"),
        ("no-checks", "open", "none"),
    ] {
        let snapshot = inspect(mode, &repository)?;
        ensure!(
            snapshot["requests"][0]["state"] == state
                && snapshot["requests"][0]["checks"] == checks,
            "{mode}: {snapshot}"
        );
    }
    ensure!(
        inspect("empty", &repository)?
            == json!({"type":"ready", "repository":"fixture/project", "branch":"feature/card", "requests":[]}),
        "empty branch PRs"
    );
    ensure!(
        inspect("auth", &repository)?
            == json!({"type":"unavailable", "reason":"authenticationRequired"}),
        "authentication state"
    );
    for mode in ["network", "malformed", "bad-url"] {
        ensure!(
            !invoke(mode, &repository)?.status.success(),
            "{mode} was displayed as valid PR data"
        );
    }
    ensure!(
        inspect("merged", &outside)? == json!({"type":"unavailable", "reason":"notRepository"}),
        "non-repository state"
    );
    git_run(&[
        "remote",
        "set-url",
        "origin",
        "https://example.invalid/fixture/project.git",
    ])?;
    ensure!(
        inspect("merged", &repository)? == json!({"type":"unavailable", "reason":"noGithubRemote"}),
        "non-GitHub state"
    );
    git_run(&[
        "remote",
        "set-url",
        "origin",
        "git@github.com:fixture/project.git",
    ])?;
    git_run(&["checkout", "--detach"])?;
    ensure!(
        inspect("merged", &repository)? == json!({"type":"unavailable", "reason":"detachedHead"}),
        "detached HEAD state"
    );
    git_run(&["checkout", "feature/card"])?;
    ensure!(
        git_run(&["rev-parse", "HEAD"])? == before_head,
        "query changed HEAD"
    );
    ensure!(
        fs::read(repository.join(".git/config"))? == before_config,
        "query changed Git configuration"
    );
    ensure!(
        git_run(&["status", "--porcelain"])?.is_empty(),
        "query changed workspace files"
    );
    println!(
        "PASS PR cards, checks, empty/auth/error states, non-GitHub, detached HEAD, RPC serialization, read-only workspace"
    );
    println!("Evidence {}", root.display());
    Ok(())
}
