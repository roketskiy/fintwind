use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::attachments::{AttachmentUpload, StoredAttachment};
use crate::model::{AgentSession, Project, ProviderProbe, UserInputAnswer};
use crate::persistence::{ComposerDraftChange, ComposerDrafts, SessionMessageMatch};
use crate::provider_session::{
    IntegrationSummary, McpServerStatus, NativeSessionSummary, NativeTranscript,
    ProviderSessionFork, ProviderSessionForkRequest, UsageStats,
};
use crate::settings::DaemonSettings;
use crate::skills::SkillsCatalog;
use crate::usage::PlanUsage;
use crate::workspace::{WorkspaceOperation, WorkspaceResult};

pub const PROTOCOL_VERSION: u32 = 7;

/// One attachment admitted with an OpenCode prompt. The path is on the daemon
/// host; the driver turns it into a `file:` URI and does not copy the bytes
/// onto this socket again.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptFile {
    pub path: PathBuf,
    pub name: String,
}

pub const MAX_WIRE_MESSAGE_BYTES: usize = 48 * 1024 * 1024;
pub const DAEMON_TOKEN_ENV: &str = "FINTWIND_DAEMON_TOKEN";
pub const DAEMON_ADDRESS_ENV: &str = "FINTWIND_DAEMON_ADDRESS";
pub const APP_EXECUTABLE_ENV: &str = "FINTWIND_APP_EXECUTABLE";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonReady {
    pub address: String,
    pub protocol_version: u32,
    pub pid: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
        token: String,
        client_id: Uuid,
        #[serde(default)]
        resume_from: Vec<ReplayCursor>,
    },
    Request(Request),
    Shutdown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub request_id: Uuid,
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub command: Command,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayCursor {
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    /// Identifies the daemon process that assigned `sequence`.
    pub epoch: Uuid,
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    /// Resolve the daemon-owned provider runtime for an existing task.
    ///
    /// Clients use this after reconnecting or opening the same daemon from a
    /// second app. It observes the session actor without starting, replacing,
    /// or otherwise mutating the provider process.
    AttachSession,
    Start {
        options: WireDriverStartOptions,
    },
    Prompt {
        prompt: String,
        /// Daemon-host paths the OpenCode driver sends as prompt `files`.
        /// Absent on older clients, which sent attachments as `@` text only.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<PromptFile>,
    },
    Steer {
        prompt: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<PromptFile>,
    },
    /// Ask the provider to compact this session's context. Admission is
    /// asynchronous: outcomes arrive as `DriverEvent::CompactionUpdated`,
    /// and the provider coalesces repeated requests while one is pending.
    CompactSession,
    Cancel,
    RefreshBackgroundWork,
    StopBackgroundWork {
        key: Value,
        control_id: String,
    },
    Respond {
        request_id: String,
        option_id: String,
    },
    RespondUserInput {
        request_id: String,
        answers: Vec<UserInputAnswer>,
    },
    ApplyOptions {
        options: WireSessionOptions,
    },
    Fork {
        turns_to_remove: usize,
    },
    GetSettings,
    UpdateSettings {
        settings: DaemonSettings,
    },
    ProbeProvider {
        binary_override: Option<String>,
        discover_models: bool,
        probe_version: bool,
    },
    FetchPlanUsage {
        binary_override: Option<String>,
        cli_version: Option<String>,
    },
    LoadSkills {
        projects: Vec<(String, PathBuf)>,
    },
    SetSkillsEnabled {
        dirs: Vec<PathBuf>,
        enabled: bool,
    },
    TrashSkills {
        dirs: Vec<PathBuf>,
    },
    LoadTaskState,
    SaveTaskState {
        projects: Vec<Project>,
        live_session_ids: Vec<Uuid>,
        sessions: Vec<AgentSession>,
    },
    /// Explicitly remove one daemon-owned task. Ordinary state saves are
    /// merge-only so a stale client snapshot cannot delete tasks another
    /// client just created.
    RemoveSession,
    /// Remove a project from the app catalog. Ordinary state saves are
    /// merge-only, so a stale client snapshot cannot restore a project
    /// another client just hid. This does not delete the project folder or
    /// OpenCode's own sessions.
    RemoveProject {
        project_id: Uuid,
    },
    /// List the OpenCode server's sessions for a workspace directory, so the
    /// client can reconcile its sidebar with sessions created outside the app
    /// (CLI, TUI, another client).
    ListProviderSessions {
        binary: PathBuf,
        directory: PathBuf,
    },
    /// Fetch one native session's transcript and translate it into the app's
    /// message/block model. Runs off any session runtime: the daemon reaches
    /// the workspace's OpenCode server through the pool.
    FetchNativeTranscript {
        binary: PathBuf,
        directory: PathBuf,
        session_id: String,
    },
    /// Walk the OpenCode store's whole session list in one pass and collect
    /// every top-level session's cumulative usage, for the usage statistics
    /// page. `directory` only anchors which resident server to ask — the
    /// listing itself is global, so every session in the store is covered
    /// no matter which project the app is showing. Blocking traversal on
    /// the daemon; the client aggregates the returned entries itself.
    FetchUsageStats {
        binary: PathBuf,
        directory: PathBuf,
    },
    /// Rename a native session on the OpenCode server, so the title matches
    /// what the CLI and TUI show.
    RenameProviderSession {
        binary: PathBuf,
        directory: PathBuf,
        session_id: String,
        title: String,
    },
    /// Delete a native session on the OpenCode server. Deleting a session in
    /// the app removes it from OpenCode too — the server is the single store.
    DeleteProviderSession {
        binary: PathBuf,
        directory: PathBuf,
        session_id: String,
    },
    /// List the server's provider integrations: the connectable roster with
    /// each entry's key-method support and live connections. The connection
    /// list is the source of truth for authorization; no file is.
    FetchIntegrations {
        binary: PathBuf,
        directory: PathBuf,
    },
    /// Authorize a catalog provider on the workspace's OpenCode server via
    /// its connect API. The running server picks the credential up
    /// immediately and persists it in its own store, so sessions can use it
    /// without a restart.
    AuthorizeProvider {
        binary: PathBuf,
        directory: PathBuf,
        provider_id: String,
        key: String,
    },
    /// Remove a provider's connected credential from the workspace's
    /// OpenCode server — the logout. The server owns the credential store,
    /// so removal goes through it; an already-disconnected provider
    /// acknowledges without error.
    LogoutProvider {
        binary: PathBuf,
        directory: PathBuf,
        provider_id: String,
    },
    /// Ask the workspace's OpenCode server how many models it currently
    /// exposes for `provider_id`, timed. A credential the server accepts is
    /// what makes models appear, so the count is the connectivity verdict.
    ProbeBuiltinProvider {
        binary: PathBuf,
        directory: PathBuf,
        provider_id: String,
    },
    /// Run `opencode mcp auth <name>` on the daemon, open the CLI-printed
    /// authorization URL in the browser, and wait for the flow to finish.
    /// Tokens stay in OpenCode's store, not in opencode.json.
    AuthenticateMcpServer {
        binary: PathBuf,
        directory: PathBuf,
        name: String,
    },
    /// Ask the workspace's OpenCode server for its MCP servers' live
    /// connection statuses. Works without any session: the server holds the
    /// connections itself.
    ListMcpServerStatuses {
        binary: PathBuf,
        directory: PathBuf,
    },
    /// Kill the pending `opencode mcp auth` child for `name`. Ack succeeds
    /// whether or not a flow was running, so the button is idempotent.
    CancelAuthenticateMcpServer {
        name: String,
    },
    HydrateSession {
        session_id: Uuid,
    },
    SearchSessionMessages {
        query: String,
        limit: usize,
    },
    LoadComposerDrafts,
    SaveComposerDrafts {
        drafts: ComposerDrafts,
        generation: u64,
    },
    ApplyComposerDraftChanges {
        changes: Vec<ComposerDraftChange>,
    },
    StoreBlob {
        mime_type: String,
        #[serde(with = "base64_bytes")]
        bytes: Vec<u8>,
    },
    ImportAttachment {
        name: String,
        upload: AttachmentUpload,
    },
    ImportPathAttachment {
        path: PathBuf,
    },
    ReadBlob {
        reference: String,
    },
    ReadAttachment {
        reference: String,
        path: PathBuf,
    },
    SweepBlobs,
    /// Fork a persisted task through one completed provider turn.
    ///
    /// This is intentionally a daemon-owned operation: provider-native
    /// conversation state, Git checkpoint refs, and SQLite all live on the
    /// daemon host and must move together for remote clients.
    ForkSessionFromResponse {
        turn_count: usize,
    },
    /// Restore a task and its provider conversation to immediately before a
    /// prior user message. The client can then submit the edited replacement
    /// as an ordinary new turn.
    RewindSessionToMessage {
        turn_count: usize,
    },
    ForkProviderSession {
        request: ProviderSessionForkRequest,
    },
    Workspace {
        operation: WorkspaceOperation,
    },
    OpenTerminal {
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    },
    WriteTerminal {
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
    },
    ResizeTerminal {
        cols: u16,
        rows: u16,
    },
    CloseTerminal,
    CloseSession,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireDriverStartOptions {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub mode: String,
    pub interaction_mode: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub context_window: Option<String>,
    pub agent_preset: Option<String>,
    pub provider_cursor: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSessionOptions {
    pub mode: String,
    pub interaction_mode: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub context_window: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireDriverEvent {
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

impl WireDriverEvent {
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencedEvent {
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    /// Changes whenever the daemon restarts, so a reused runtime id can begin
    /// again at sequence one without being mistaken for an old event.
    pub epoch: Uuid,
    pub sequence: u64,
    pub event: WireDriverEvent,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerMessage {
    Hello {
        protocol_version: u32,
        daemon_version: String,
    },
    Rejected {
        message: String,
    },
    Response {
        request_id: Uuid,
        outcome: ResponseOutcome,
    },
    Event(SequencedEvent),
    /// The daemon-owned project/task catalog changed through another client.
    /// Clients should invalidate their lightweight task-state snapshot; live
    /// runtime events continue through [`Self::Event`].
    TaskStateChanged {
        revision: u64,
    },
    ShuttingDown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ResponseOutcome {
    Ok { payload: ResponsePayload },
    Error { error: RpcError },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ResponsePayload {
    Ack,
    SessionRuntime {
        runtime_id: Option<Uuid>,
        supports_steer: bool,
    },
    Started {
        supports_steer: bool,
    },
    OptionsApplied {
        applied: bool,
    },
    Cursor {
        cursor: Option<Value>,
    },
    Settings {
        settings: DaemonSettings,
    },
    ProviderProbe {
        probe: ProviderProbe,
        version: Option<String>,
    },
    PlanUsage {
        usage: Option<PlanUsage>,
    },
    SkillsCatalog {
        catalog: SkillsCatalog,
    },
    TaskState {
        projects: Vec<Project>,
        sessions: Vec<AgentSession>,
        default_cwd: PathBuf,
        projectless_root: Option<PathBuf>,
    },
    TaskStateSaved {
        sessions: Vec<AgentSession>,
    },
    Session {
        session: Option<AgentSession>,
    },
    SessionMessageMatches {
        matches: Vec<SessionMessageMatch>,
    },
    ProviderSessions {
        sessions: Vec<NativeSessionSummary>,
    },
    Integrations {
        integrations: Vec<IntegrationSummary>,
    },
    BuiltinProviderProbed {
        models: usize,
        latency_ms: u64,
    },
    McpServerStatuses {
        statuses: Vec<McpServerStatus>,
    },
    NativeTranscript {
        transcript: NativeTranscript,
    },
    UsageStats {
        stats: UsageStats,
    },
    ComposerDrafts {
        drafts: ComposerDrafts,
    },
    BlobStored {
        reference: String,
        path: PathBuf,
    },
    AttachmentStored {
        attachment: StoredAttachment,
    },
    BlobData {
        #[serde(with = "base64_bytes")]
        bytes: Vec<u8>,
    },
    ProviderSessionForked {
        result: ProviderSessionFork,
    },
    SessionForked {
        session: AgentSession,
        checkpoint_warning: Option<String>,
    },
    SessionRewound {
        session: AgentSession,
        cleanup_warning: Option<String>,
    },
    Workspace {
        result: WorkspaceResult,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcError {
    pub message: String,
}

impl From<anyhow::Error> for RpcError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_payloads_use_base64_json_strings() {
        let payload = ResponsePayload::BlobData {
            bytes: vec![0, 1, 2, 255],
        };
        let json = serde_json::to_value(&payload).unwrap();

        assert_eq!(json["bytes"], "AAEC/w==");
        let ResponsePayload::BlobData { bytes } = serde_json::from_value(json).unwrap() else {
            panic!("unexpected payload variant");
        };
        assert_eq!(bytes, vec![0, 1, 2, 255]);

        let command = Command::WriteTerminal {
            data: vec![0, 1, 2, 255],
        };
        let json = serde_json::to_value(&command).unwrap();
        assert_eq!(json["type"], "writeTerminal");
        assert_eq!(json["data"], "AAEC/w==");
        let Command::WriteTerminal { data } = serde_json::from_value(json).unwrap() else {
            panic!("unexpected command variant");
        };
        assert_eq!(data, vec![0, 1, 2, 255]);
    }

    #[test]
    fn prompt_files_round_trip_and_legacy_prompts_have_none() {
        let plain = serde_json::to_value(Command::Prompt {
            prompt: "hi".into(),
            files: Vec::new(),
        })
        .unwrap();
        assert_eq!(plain["type"], "prompt");
        assert_eq!(plain["prompt"], "hi");
        assert!(plain.get("files").is_none());

        let path = PathBuf::from("E:/attachments/notes.md");
        let with_files = Command::Prompt {
            prompt: String::new(),
            files: vec![PromptFile {
                path: path.clone(),
                name: "notes.md".into(),
            }],
        };
        let json = serde_json::to_value(&with_files).unwrap();
        assert_eq!(json["files"][0]["name"], "notes.md");
        let Command::Prompt { prompt, files } = serde_json::from_value(json).unwrap() else {
            panic!("prompt command");
        };
        assert!(prompt.is_empty());
        assert_eq!(
            files,
            vec![PromptFile {
                path,
                name: "notes.md".into()
            }]
        );

        let legacy = serde_json::json!({"type": "prompt", "prompt": "hi"});
        let Command::Prompt { prompt, files } = serde_json::from_value(legacy).unwrap() else {
            panic!("legacy prompt");
        };
        assert_eq!(prompt, "hi");
        assert!(files.is_empty());
    }

    #[test]
    fn response_fork_command_uses_stable_camel_case_fields() {
        let json =
            serde_json::to_value(Command::ForkSessionFromResponse { turn_count: 7 }).unwrap();

        assert_eq!(json["type"], "forkSessionFromResponse");
        assert_eq!(json["turnCount"], 7);
        assert_eq!(PROTOCOL_VERSION, 7);
    }

    #[test]
    fn remove_project_command_uses_stable_camel_case_fields() {
        let project_id = Uuid::from_u128(9);
        let json = serde_json::to_value(Command::RemoveProject { project_id }).unwrap();

        assert_eq!(json["type"], "removeProject");
        assert_eq!(json["projectId"], project_id.to_string());
    }

    #[test]
    fn message_rewind_command_uses_stable_camel_case_fields() {
        let json = serde_json::to_value(Command::RewindSessionToMessage { turn_count: 4 }).unwrap();

        assert_eq!(json["type"], "rewindSessionToMessage");
        assert_eq!(json["turnCount"], 4);
        assert_eq!(PROTOCOL_VERSION, 7);
    }

    #[test]
    fn handshake_and_replay_field_names_are_stable() {
        let session_id = Uuid::nil();
        let runtime_id = Uuid::from_u128(1);
        let message = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: "secret".into(),
            client_id: Uuid::from_u128(2),
            resume_from: vec![ReplayCursor {
                session_id,
                runtime_id,
                epoch: Uuid::from_u128(3),
                sequence: 9,
            }],
        };
        let json = serde_json::to_value(message).unwrap();

        assert_eq!(json["type"], "hello");
        assert_eq!(json["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(json["resumeFrom"][0]["sessionId"], session_id.to_string());
        assert_eq!(json["resumeFrom"][0]["runtimeId"], runtime_id.to_string());
        assert_eq!(
            json["resumeFrom"][0]["epoch"],
            Uuid::from_u128(3).to_string()
        );
        assert!(json.get("protocol_version").is_none());
    }

    #[test]
    fn composer_draft_changes_have_stable_wire_keys() {
        let project_id = Uuid::from_u128(7);
        let command = Command::ApplyComposerDraftChanges {
            changes: vec![ComposerDraftChange {
                target: crate::persistence::ComposerDraftTarget::NewSession { project_id },
                draft: Some(crate::persistence::ComposerDraft {
                    text: "unfinished".into(),
                    attachments: Vec::new(),
                }),
            }],
        };
        let json = serde_json::to_value(command).unwrap();

        assert_eq!(json["type"], "applyComposerDraftChanges");
        assert_eq!(json["changes"][0]["target"]["type"], "newSession");
        assert_eq!(
            json["changes"][0]["target"]["projectId"],
            project_id.to_string()
        );
        assert_eq!(json["changes"][0]["draft"]["text"], "unfinished");
    }
}
