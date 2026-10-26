use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PullRequestSnapshot {
    Ready {
        repository: String,
        branch: String,
        requests: Vec<PullRequest>,
    },
    Unavailable {
        reason: PullRequestUnavailable,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestUnavailable {
    NotRepository,
    DetachedHead,
    NoGithubRemote,
    CliMissing,
    AuthenticationRequired,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestChecks {
    None,
    Pending,
    Passed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    pub head_branch: String,
    pub base_branch: String,
    pub author: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub comments: usize,
    pub updated_at: String,
    pub checks: PullRequestChecks,
}
