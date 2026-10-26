use std::io;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use fintwind_protocol::pull_request::{
    PullRequest, PullRequestChecks, PullRequestSnapshot, PullRequestState, PullRequestUnavailable,
};
use serde::Deserialize;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub fn inspect(cwd: &Path) -> anyhow::Result<PullRequestSnapshot> {
    let unavailable = |reason| Ok(PullRequestSnapshot::Unavailable { reason });
    let root = git(cwd, &["rev-parse", "--show-toplevel"])?;
    if !root.status.success() {
        return unavailable(PullRequestUnavailable::NotRepository);
    }
    let branch = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if branch.status.code() == Some(1) {
        return unavailable(PullRequestUnavailable::DetachedHead);
    }
    ensure_success(&branch)?;
    let branch = String::from_utf8(branch.stdout)?.trim().to_owned();
    let remotes = git(cwd, &["remote", "-v"])?;
    ensure_success(&remotes)?;
    let remote_text = String::from_utf8(remotes.stdout)?;
    let mut remotes = remote_text
        .lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let name = columns.next()?;
            let url = columns.next()?;
            (columns.next()? == "(fetch)").then_some((name, url))
        })
        .collect::<Vec<_>>();
    remotes.sort_by_key(|(name, _)| *name != "origin");
    let Some(repository) = remotes
        .into_iter()
        .find_map(|(_, remote)| github_repository(remote))
    else {
        return unavailable(PullRequestUnavailable::NoGithubRemote);
    };
    let mut command = crate::command_env::command("gh");
    command
        .current_dir(cwd)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "pr",
            "list",
            "--repo",
            &repository,
            "--head",
            &branch,
            "--state",
            "all",
            "--limit",
            "8",
            "--json",
            "number,title,state,isDraft,url,headRefName,baseRefName,author,additions,deletions,changedFiles,updatedAt,comments,statusCheckRollup",
        ]);
    let output = match crate::command_env::output_with_timeout(&mut command, REQUEST_TIMEOUT) {
        Ok(Some(output)) => output,
        Ok(None) => bail!("GitHub request timed out"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return unavailable(PullRequestUnavailable::CliMissing);
        }
        Err(error) => return Err(error).context("failed to run GitHub CLI"),
    };
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        if output.status.code() == Some(4)
            || error.contains("gh auth login")
            || error.contains("authentication required")
            || error.contains("not logged into")
        {
            return unavailable(PullRequestUnavailable::AuthenticationRequired);
        }
        bail!("{}", error.trim());
    }
    let requests: Vec<GithubPullRequest> =
        serde_json::from_slice(&output.stdout).context("invalid GitHub pull request response")?;
    let requests = requests
        .into_iter()
        .map(|request| request.into_card(&repository))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(PullRequestSnapshot::Ready {
        repository,
        branch,
        requests,
    })
}

fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<Output> {
    let mut command = crate::command_env::plain_command("git");
    command.current_dir(cwd).args(args);
    crate::command_env::output_with_timeout(&mut command, Duration::from_secs(5))?
        .ok_or_else(|| anyhow!("Git query timed out"))
}

fn ensure_success(output: &Output) -> anyhow::Result<()> {
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn github_repository(remote: &str) -> Option<String> {
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("http://github.com/"))?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let (owner, repository) = path.split_once('/')?;
    if owner.is_empty()
        || repository.is_empty()
        || repository.contains('/')
        || !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
    {
        return None;
    }
    Some(path.to_owned())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubPullRequest {
    number: u64,
    title: String,
    url: String,
    state: GithubState,
    is_draft: bool,
    head_ref_name: String,
    base_ref_name: String,
    author: Option<GithubAuthor>,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    comments: Vec<GithubComment>,
    updated_at: String,
    status_check_rollup: Option<Vec<GithubCheck>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum GithubState {
    Open,
    Merged,
    Closed,
}

#[derive(Deserialize)]
struct GithubAuthor {
    login: String,
}

#[derive(Deserialize)]
struct GithubComment {}

#[derive(Deserialize)]
struct GithubCheck {
    state: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
}

impl GithubPullRequest {
    fn into_card(self, repository: &str) -> anyhow::Result<PullRequest> {
        let expected = format!("https://github.com/{repository}/pull/{}", self.number);
        if self.url != expected {
            bail!("GitHub returned an unexpected pull request URL");
        }
        let checks = self.status_check_rollup.unwrap_or_default();
        let checks = if checks.is_empty() {
            PullRequestChecks::None
        } else if checks.iter().any(|check| {
            matches!(
                check.conclusion.as_deref().or(check.state.as_deref()),
                Some(
                    "FAILURE"
                        | "ERROR"
                        | "TIMED_OUT"
                        | "CANCELLED"
                        | "ACTION_REQUIRED"
                        | "STARTUP_FAILURE"
                )
            )
        }) {
            PullRequestChecks::Failed
        } else if !checks.iter().all(|check| {
            matches!(check.state.as_deref(), Some("SUCCESS"))
                || (check.status.as_deref() == Some("COMPLETED")
                    && matches!(
                        check.conclusion.as_deref(),
                        Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
                    ))
        }) {
            PullRequestChecks::Pending
        } else {
            PullRequestChecks::Passed
        };
        Ok(PullRequest {
            number: self.number,
            title: self.title,
            url: self.url,
            state: match self.state {
                GithubState::Open if self.is_draft => PullRequestState::Draft,
                GithubState::Open => PullRequestState::Open,
                GithubState::Merged => PullRequestState::Merged,
                GithubState::Closed => PullRequestState::Closed,
            },
            head_branch: self.head_ref_name,
            base_branch: self.base_ref_name,
            author: self.author.map(|author| author.login).unwrap_or_default(),
            additions: self.additions,
            deletions: self.deletions,
            changed_files: self.changed_files,
            comments: self.comments.len(),
            updated_at: self.updated_at,
            checks,
        })
    }
}
