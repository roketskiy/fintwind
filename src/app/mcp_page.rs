//! The MCP settings page: OpenCode's MCP server roster as a mail-style
//! master–detail split — the server list on the left, the selected server's
//! connection and variables on the right, or the add-server form in the
//! detail's place.
//!
//! The roster is `mcp.servers` in OpenCode's own configuration file
//! (`~/.config/opencode/opencode.json`), loaded when the page opens and
//! committed straight back on every mutation — the same single-source-of-
//! truth contract as the Providers page, so entries added with the CLI, the
//! TUI, or an editor are the same data. OpenCode watches the file and
//! hot-reloads, so a commit reaches running `opencode serve` processes
//! without a restart. Remote OAuth runs `opencode mcp auth <name>` on the
//! daemon and opens the CLI-printed authorization URL in the browser.
//!
//! Field edits debounce their commit; discrete actions (add, delete, toggle,
//! rename, re-type) commit immediately, the same one-shot-action allowance
//! the Providers page uses.

use crate::theme::ui_px;

use gpui::KeyDownEvent;

use fintwind_client::custom_providers::unique_provider_slug;
use fintwind_client::opencode_config::{McpOAuthMode, McpServer, McpServerKind};

use super::providers_page::{
    enabled_badge, form_hint, info_note, labeled_field, outline_button, provider_tile,
    section_label, small_action_button,
};

use super::*;

/// #D97757 — this page's selection and accent color. The dark appearance
/// uses it as authored; light darkens it, the way `Theme::accent` is tuned
/// per appearance, so fills, dots, and badge text keep their contrast on
/// light surfaces.
fn mcp_accent(theme: &Theme) -> Hsla {
    if theme.is_dark {
        rgb(0xD97757).into()
    } else {
        rgb(0xB25A39).into()
    }
}

/// Glyph color on an accent fill. The dark appearance's #D97757 is a
/// mid-tone, so near-black glyphs out-read white ones there; the light
/// appearance darkens the fill instead and takes white.
fn on_mcp_accent(theme: &Theme) -> Hsla {
    if theme.is_dark {
        rgb(0x201814).into()
    } else {
        rgb(0xFFFFFF).into()
    }
}

const MCP_LIST_WIDTH: f32 = 264.0;

/// Which key-value table of the selected server is on screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum McpVariableTable {
    Environment,
    Headers,
}

impl McpVariableTable {
    fn for_kind(kind: McpServerKind) -> Self {
        match kind {
            McpServerKind::Local => Self::Environment,
            McpServerKind::Remote => Self::Headers,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Environment => tr!("mcp.environment_label"),
            Self::Headers => tr!("mcp.headers_label"),
        }
    }

    fn add_label(self) -> String {
        match self {
            Self::Environment => tr!("mcp.add_variable"),
            Self::Headers => tr!("mcp.add_header"),
        }
    }
}

/// Which variable row the inline variable editor is working on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum McpVariableRow {
    Add,
    Edit(usize),
}

/// The editor target: which table, and which row of it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct McpVariableEditor {
    pub table: McpVariableTable,
    pub row: McpVariableRow,
}

/// Which detail field a keystroke edit landed in.
#[derive(Clone, Copy)]
pub(super) enum McpField {
    Command,
    Url,
    OAuthClientId,
    OAuthClientSecret,
    OAuthScope,
}

fn mcp_server_icon(kind: McpServerKind) -> &'static str {
    match kind {
        McpServerKind::Local => "icons/terminal.svg",
        McpServerKind::Remote => "icons/globe.svg",
    }
}

fn mcp_kind_label(kind: McpServerKind) -> String {
    match kind {
        McpServerKind::Local => tr!("mcp.type_local"),
        McpServerKind::Remote => tr!("mcp.type_remote"),
    }
}

fn mcp_oauth_mode_label(mode: McpOAuthMode) -> String {
    match mode {
        McpOAuthMode::Automatic => tr!("mcp.oauth_automatic"),
        McpOAuthMode::Disabled => tr!("mcp.oauth_disabled"),
        McpOAuthMode::Custom => tr!("mcp.oauth_custom"),
    }
}

fn mcp_server_variables(server: &McpServer, table: McpVariableTable) -> &Vec<(String, String)> {
    match table {
        McpVariableTable::Environment => &server.environment,
        McpVariableTable::Headers => &server.headers,
    }
}

fn mcp_server_variables_mut(
    server: &mut McpServer,
    table: McpVariableTable,
) -> &mut Vec<(String, String)> {
    match table {
        McpVariableTable::Environment => &mut server.environment,
        McpVariableTable::Headers => &mut server.headers,
    }
}

/// The command line as one editor string; the argv vector is the storage
/// format, whitespace splits it back apart.
fn mcp_parse_command(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn mcp_url_valid(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// The one-line summary under a server's name: the command it launches or
/// the endpoint it serves, exactly what identifies it in the config file.
fn mcp_row_caption(server: &McpServer) -> String {
    match server.kind {
        McpServerKind::Local if !server.command.is_empty() => server.command.join(" "),
        McpServerKind::Remote if !server.url.is_empty() => server.url.clone(),
        _ => tr!("mcp.no_connection"),
    }
}

/// One MCP server's live connection state as the workspace's OpenCode server
/// reports it.
pub(super) type McpStatusEntry = fintwind_client::provider_session::McpServerStatus;

/// Each state pairs a distinct glyph with its color, so the status reads
/// without relying on hue alone — failed and needs_auth differ in shape,
/// pending spins, disabled is hollow.
fn mcp_status_glyph(state: fintwind_client::provider_session::McpConnectionState) -> &'static str {
    use fintwind_client::provider_session::McpConnectionState as State;
    match state {
        State::Connected => "icons/check.svg",
        State::Pending => "icons/loader-circle.svg",
        State::Disabled => "icons/block.svg",
        State::Failed => "icons/alert.svg",
        State::NeedsAuth => "icons/lock.svg",
    }
}

fn mcp_status_color(
    state: fintwind_client::provider_session::McpConnectionState,
    theme: &Theme,
) -> Hsla {
    use fintwind_client::provider_session::McpConnectionState as State;
    match state {
        State::Connected => theme.success,
        State::Pending => mcp_accent(theme),
        State::Disabled => theme.text_tertiary,
        State::Failed | State::NeedsAuth => theme.danger,
    }
}

fn mcp_status_label(entry: &McpStatusEntry) -> String {
    use fintwind_client::provider_session::McpConnectionState as State;
    match entry.status {
        State::Connected => tr!("mcp.status_connected"),
        State::Pending => tr!("mcp.status_pending"),
        State::Disabled => tr!("mcp.status_disabled"),
        State::Failed => tr!("mcp.status_failed"),
        State::NeedsAuth => tr!("mcp.status_needs_auth"),
    }
}

impl Fintwind {
    // ── Selection & state ──────────────────────────────────────────────────

    /// The server the detail pane shows: the stored selection while it still
    /// resolves, the first server on the roster otherwise, so the pane never
    /// opens empty while any server exists.
    fn effective_mcp_server(&self) -> Option<McpServer> {
        let selected = self.mcp_selected.clone();
        self.mcp_servers
            .iter()
            .find(|server| Some(&server.name) == selected.as_ref())
            .or_else(|| self.mcp_servers.first())
            .cloned()
    }

    fn selected_mcp_server_mut(&mut self) -> Option<&mut McpServer> {
        let selected = self.mcp_selected.clone();
        self.mcp_servers
            .iter_mut()
            .find(|server| Some(&server.name) == selected.as_ref())
    }

    fn select_mcp_server(&mut self, name: String, cx: &mut Context<Self>) {
        self.mcp_selected = Some(name);
        self.mcp_adding = false;
        self.mcp_renaming = false;
        self.mcp_delete_arming = None;
        self.mcp_variable_editor = None;
        if let Some(selected) = self.mcp_selected.clone() {
            self.load_mcp_fields(&selected, cx);
        }
        self.mcp_detail_scroll.set_offset(gpui::Point::default());
        cx.notify();
    }

    /// Load the detail's connection fields from the server now under
    /// selection. The fields are the only editors of these values, so this
    /// runs on selection changes; the `Edited` handler's echo guard skips
    /// the reflection.
    fn load_mcp_fields(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(server) = self
            .mcp_servers
            .iter()
            .find(|server| server.name == name)
            .cloned()
        else {
            return;
        };
        let command_text = server.command.join(" ");
        let url = server.url;
        let client_id = server.oauth.client_id;
        let client_secret = server.oauth.client_secret;
        let scope = server.oauth.scope;
        self.mcp_command_input.update(cx, |input, cx| {
            if input.content() != command_text {
                input.set_content(command_text, cx);
            }
        });
        self.mcp_url_input.update(cx, |input, cx| {
            if input.content() != url {
                input.set_content(url, cx);
            }
        });
        self.mcp_oauth_client_id.update(cx, |input, cx| {
            if input.content() != client_id {
                input.set_content(client_id, cx);
            }
        });
        self.mcp_oauth_client_secret.update(cx, |input, cx| {
            if input.content() != client_secret {
                input.set_content(client_secret, cx);
            }
        });
        self.mcp_oauth_scope.update(cx, |input, cx| {
            if input.content() != scope {
                input.set_content(scope, cx);
            }
        });
    }

    /// Clear the page's transient editor state and re-read OpenCode's
    /// configuration, so a server added with the CLI since the last visit is
    /// already on the roster. Called when the page opens; a half-finished
    /// rename or variable edit never survives the visit.
    pub(super) fn reset_mcp_page(&mut self, cx: &mut Context<Self>) {
        self.mcp_adding = false;
        self.mcp_renaming = false;
        self.mcp_delete_arming = None;
        self.mcp_variable_editor = None;
        if let Some(name) = self.mcp_selected.clone() {
            self.load_mcp_fields(&name, cx);
        }
        self.load_mcp_servers_from_config(cx);
        self.refresh_mcp_statuses(cx);
    }

    // ── Persistence ────────────────────────────────────────────────────────

    /// Load the roster from `mcp.servers` in OpenCode's own configuration
    /// off-thread. The result replaces the working store unless a newer load
    /// started or a debounced field edit is pending.
    pub(super) fn load_mcp_servers_from_config(&mut self, cx: &mut Context<Self>) {
        self.mcp_load_generation += 1;
        let generation = self.mcp_load_generation;
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { fintwind_client::opencode_config::load_mcp_servers() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.mcp_load_generation != generation || this.mcp_commit_generation != 0 {
                    return;
                }
                match loaded {
                    Ok(servers) => {
                        let selected_missing = this
                            .mcp_selected
                            .as_deref()
                            .is_some_and(|name| !servers.iter().any(|server| server.name == name));
                        this.mcp_servers = servers;
                        if selected_missing {
                            this.mcp_selected = None;
                            this.mcp_adding = false;
                            this.mcp_renaming = false;
                            this.mcp_variable_editor = None;
                            this.mcp_delete_arming = None;
                        }
                        // The editors target the stored selection, so an
                        // unselected-but-populated roster pins the first
                        // server before the fields load onto it.
                        if this.mcp_selected.is_none()
                            && let Some(first) =
                                this.mcp_servers.first().map(|server| server.name.clone())
                        {
                            this.mcp_selected = Some(first);
                        }
                        if let Some(name) = this.mcp_selected.clone() {
                            this.load_mcp_fields(&name, cx);
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        this.show_toast(tr!("mcp.sync_failed", error = error.to_string()));
                    }
                }
            });
        })
        .detach();
    }

    /// Commit the working roster straight into OpenCode's configuration.
    /// OpenCode watches the file and hot-reloads, so running serves pick the
    /// change up without a restart.
    pub(super) fn commit_mcp_servers(&mut self, cx: &mut Context<Self>) {
        self.mcp_load_generation += 1;
        if let Err(error) = fintwind_client::opencode_config::save_mcp_servers(&self.mcp_servers) {
            self.show_toast(tr!("mcp.sync_failed", error = error.to_string()));
        }
        cx.notify();
    }

    /// Debounced commit for keystroke-level edits (command, URL), so a burst
    /// of typing costs one save instead of one per key.
    fn schedule_mcp_commit(&mut self, cx: &mut Context<Self>) {
        self.mcp_commit_generation += 1;
        let generation = self.mcp_commit_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(700))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.mcp_commit_generation != generation {
                    return;
                }
                this.mcp_commit_generation = 0;
                this.commit_mcp_servers(cx);
            });
        })
        .detach();
    }

    /// Refresh the roster's live connection statuses from the workspace's
    /// OpenCode server. Best-effort: a server that cannot be reached leaves
    /// the previous snapshot in place; the list simply renders without dots
    /// until one succeeds. The listing RPC is blocking, so it runs on the
    /// background executor — never reachable from `render`.
    pub(super) fn refresh_mcp_statuses(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = self.native_binary_path() else {
            return;
        };
        let directory = self.mcp_auth_directory();
        self.mcp_status_generation += 1;
        let generation = self.mcp_status_generation;
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    fintwind_client::persistence::StateStore::remote(daemon)
                        .list_mcp_server_statuses(binary, directory)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.mcp_status_generation != generation {
                    return;
                }
                match fetched {
                    Ok(statuses) => {
                        this.mcp_statuses = Some(
                            statuses
                                .into_iter()
                                .map(|entry| (entry.name.clone(), entry))
                                .collect(),
                        );
                        cx.notify();
                    }
                    Err(error) => {
                        eprintln!("could not refresh MCP statuses: {error}");
                    }
                }
            });
        })
        .detach();
    }

    /// The status snapshot for `name`, when one has been fetched.
    fn mcp_status(&self, name: &str) -> Option<&McpStatusEntry> {
        self.mcp_statuses
            .as_ref()
            .and_then(|statuses| statuses.get(name))
    }

    /// A detail field was edited. Applies the new value to the selected
    /// server; the `Edited` echo from loading fields is filtered here.
    pub(super) fn mcp_field_edited(
        &mut self,
        field: McpField,
        value: String,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.selected_mcp_server_mut() else {
            return;
        };
        let persist = match field {
            McpField::Command => {
                let command = mcp_parse_command(&value);
                if server.command == command {
                    return;
                }
                server.command = command;
                true
            }
            McpField::Url => {
                if server.url == value {
                    return;
                }
                server.url = value;
                true
            }
            McpField::OAuthClientId => {
                if server.oauth.client_id == value {
                    return;
                }
                server.oauth.client_id = value;
                server.oauth.mode == McpOAuthMode::Custom
            }
            McpField::OAuthClientSecret => {
                if server.oauth.client_secret == value {
                    return;
                }
                server.oauth.client_secret = value;
                server.oauth.mode == McpOAuthMode::Custom
            }
            McpField::OAuthScope => {
                if server.oauth.scope == value {
                    return;
                }
                server.oauth.scope = value;
                server.oauth.mode == McpOAuthMode::Custom
            }
        };
        if persist {
            self.schedule_mcp_commit(cx);
        }
        cx.notify();
    }

    // ── Mutations ──────────────────────────────────────────────────────────

    fn begin_add_mcp_server(&mut self, cx: &mut Context<Self>) {
        self.mcp_adding = true;
        self.mcp_renaming = false;
        self.mcp_delete_arming = None;
        self.mcp_variable_editor = None;
        self.mcp_form_kind = McpServerKind::Local;
        for input in [
            &self.mcp_form_name,
            &self.mcp_form_command,
            &self.mcp_form_url,
        ] {
            input.update(cx, |input, cx| input.set_content(String::new(), cx));
        }
        cx.notify();
    }

    pub(super) fn submit_mcp_form(&mut self, cx: &mut Context<Self>) {
        if !self.mcp_adding {
            return;
        }
        let name = self.mcp_form_name.read(cx).content().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let kind = self.mcp_form_kind;
        let command = mcp_parse_command(self.mcp_form_command.read(cx).content());
        let url = self.mcp_form_url.read(cx).content().trim().to_owned();
        match kind {
            McpServerKind::Local if command.is_empty() => {
                self.show_toast(tr!("mcp.hint_need_command"));
                return;
            }
            McpServerKind::Remote if !mcp_url_valid(&url) => {
                self.show_toast(tr!("mcp.hint_invalid_url"));
                return;
            }
            _ => {}
        }

        let taken: Vec<String> = self
            .mcp_servers
            .iter()
            .map(|server| server.name.clone())
            .collect();
        let key = unique_provider_slug(&name, &taken);
        let server = McpServer {
            name: key.clone(),
            kind,
            command,
            url,
            environment: Vec::new(),
            headers: Vec::new(),
            oauth: Default::default(),
            enabled: true,
            raw: serde_json::Value::Null,
        };
        self.mcp_servers.push(server);
        self.mcp_adding = false;
        self.commit_mcp_servers(cx);
        self.select_mcp_server(key, cx);
        // The new server joins the workspace's roster on the next refresh;
        // its connection state arrives with it.
        self.refresh_mcp_statuses(cx);
        self.show_success_toast(tr!("mcp.added_toast", name = name));
    }

    fn delete_mcp_server(&mut self, name: String, cx: &mut Context<Self>) {
        self.mcp_servers.retain(|server| server.name != name);
        self.mcp_delete_arming = None;
        if let Some(statuses) = self.mcp_statuses.as_mut() {
            statuses.remove(&name);
        }
        self.commit_mcp_servers(cx);
        if self.mcp_selected.as_deref() == Some(name.as_str()) {
            // Land the detail on whichever server the fallback picks next.
            self.mcp_selected = None;
            self.mcp_variable_editor = None;
            if let Some(next) = self.effective_mcp_server() {
                self.mcp_selected = Some(next.name.clone());
                self.load_mcp_fields(&next.name, cx);
            }
        }
        self.show_success_toast(tr!("mcp.deleted_toast", name = name));
    }

    fn toggle_mcp_server_enabled(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some(server) = self
            .mcp_servers
            .iter_mut()
            .find(|server| server.name == name)
        {
            server.enabled = !server.enabled;
        }
        self.commit_mcp_servers(cx);
        // OpenCode hot-reloads the file and reconnects the server; its new
        // connection state lands with the next refresh.
        self.refresh_mcp_statuses(cx);
    }

    fn set_mcp_server_kind(&mut self, name: String, kind: McpServerKind, cx: &mut Context<Self>) {
        if let Some(server) = self
            .mcp_servers
            .iter_mut()
            .find(|server| server.name == name)
        {
            if server.kind == kind {
                cx.notify();
                return;
            }
            server.kind = kind;
            self.commit_mcp_servers(cx);
            // A kind switch changes what the server connects to, so the
            // old status no longer describes it.
            if let Some(statuses) = self.mcp_statuses.as_mut() {
                statuses.remove(&name);
            }
            self.refresh_mcp_statuses(cx);
        }
    }

    fn set_mcp_oauth_mode(&mut self, name: String, mode: McpOAuthMode, cx: &mut Context<Self>) {
        if let Some(server) = self
            .mcp_servers
            .iter_mut()
            .find(|server| server.name == name)
        {
            if server.oauth.mode == mode {
                cx.notify();
                return;
            }
            server.oauth.mode = mode;
            self.commit_mcp_servers(cx);
        }
    }

    fn mcp_auth_directory(&self) -> PathBuf {
        self.state
            .selected_project
            .and_then(|project_id| {
                self.state
                    .projects
                    .iter()
                    .find(|project| project.id == project_id)
                    .map(|project| project.path.clone())
            })
            .or_else(|| {
                self.state
                    .projects
                    .first()
                    .map(|project| project.path.clone())
            })
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(std::env::temp_dir))
    }

    fn start_mcp_oauth_login(&mut self, name: String, cx: &mut Context<Self>) {
        if self.mcp_oauth_auth_name.is_some() {
            return;
        }
        let Some(binary) = self.native_binary_path() else {
            self.show_toast(tr!("mcp.oauth_missing_opencode"));
            return;
        };
        self.commit_mcp_servers(cx);
        self.mcp_oauth_auth_generation += 1;
        let generation = self.mcp_oauth_auth_generation;
        self.mcp_oauth_auth_name = Some(name.clone());
        let directory = self.mcp_auth_directory();
        let daemon = self.daemon.clone();
        self.mcp_oauth_cancel_requested = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    fintwind_client::persistence::StateStore::remote(daemon)
                        .authenticate_mcp_server(binary, directory, name)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.mcp_oauth_auth_generation != generation {
                    return;
                }
                let finished = this.mcp_oauth_auth_name.take();
                match result {
                    Ok(()) => {
                        let name = finished.unwrap_or_default();
                        this.show_success_toast(tr!("mcp.oauth_signed_in", name = name));
                        this.refresh_mcp_statuses(cx);
                    }
                    Err(error) => {
                        if this.mcp_oauth_cancel_requested {
                            this.show_toast(tr!("mcp.oauth_cancelled"));
                        } else {
                            this.show_toast(tr!(
                                "mcp.oauth_sign_in_failed_detail",
                                error = error.to_string()
                            ));
                        }
                    }
                }
                this.mcp_oauth_cancel_requested = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Fire the daemon cancel for the running browser sign-in. The pending
    /// login RPC fails on its own request thread, so this only needs the ack;
    /// `mcp_oauth_auth_name` clears when that failure lands.
    fn cancel_mcp_oauth_login(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.mcp_oauth_auth_name.clone() else {
            return;
        };
        self.mcp_oauth_cancel_requested = true;
        let daemon = self.daemon.clone();
        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move {
                    let _ = fintwind_client::persistence::StateStore::remote(daemon)
                        .cancel_mcp_server(name);
                })
                .await;
        })
        .detach();
        cx.notify();
    }

    fn begin_mcp_rename(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.mcp_selected.clone() else {
            return;
        };
        self.mcp_renaming = true;
        self.mcp_rename_input
            .update(cx, |input, cx| input.set_content(name, cx));
        cx.notify();
    }

    /// Renaming an MCP server rewrites its key in `mcp.servers`, so a name
    /// must stay unique on the roster.
    pub(super) fn confirm_mcp_rename(&mut self, cx: &mut Context<Self>) {
        if !self.mcp_renaming {
            return;
        }
        let name = self.mcp_rename_input.read(cx).content().trim().to_owned();
        self.mcp_renaming = false;
        let Some(selected) = self.mcp_selected.clone() else {
            cx.notify();
            return;
        };
        if name.is_empty() || name == selected {
            cx.notify();
            return;
        }
        if self.mcp_servers.iter().any(|server| server.name == name) {
            self.show_toast(tr!("mcp.duplicate_name"));
            return;
        }
        if let Some(server) = self
            .mcp_servers
            .iter_mut()
            .find(|server| server.name == selected)
        {
            server.name = name.clone();
        }
        if let Some(statuses) = self.mcp_statuses.as_mut()
            && let Some(entry) = statuses.remove(&selected)
        {
            statuses.insert(name.clone(), entry);
        }
        self.mcp_selected = Some(name);
        self.commit_mcp_servers(cx);
    }

    fn cancel_mcp_rename(&mut self, cx: &mut Context<Self>) {
        self.mcp_renaming = false;
        cx.notify();
    }

    // ── Variable editor (environment / headers) ────────────────────────────

    fn begin_mcp_variable_editor(&mut self, editor: McpVariableEditor, cx: &mut Context<Self>) {
        let existing = match (editor.row, self.effective_mcp_server()) {
            (McpVariableRow::Edit(index), Some(server)) => {
                mcp_server_variables(&server, editor.table)
                    .get(index)
                    .cloned()
            }
            _ => None,
        };
        let (key, value) = existing.unwrap_or_default();
        self.mcp_variable_editor = Some(editor);
        self.mcp_variable_key_input
            .update(cx, |input, cx| input.set_content(key, cx));
        self.mcp_variable_value_input
            .update(cx, |input, cx| input.set_content(value, cx));
        cx.notify();
    }

    fn cancel_mcp_variable_editor(&mut self, cx: &mut Context<Self>) {
        self.mcp_variable_editor = None;
        cx.notify();
    }

    pub(super) fn confirm_mcp_variable_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.mcp_variable_editor else {
            return;
        };
        let key = self
            .mcp_variable_key_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        if key.is_empty() {
            return;
        }
        let value = self.mcp_variable_value_input.read(cx).content().to_owned();
        enum Outcome {
            Done,
            Duplicate,
        }
        let outcome = match self.selected_mcp_server_mut() {
            Some(server) => {
                let table = mcp_server_variables_mut(server, editor.table);
                let duplicate = table.iter().enumerate().any(|(index, (existing, _))| {
                    existing == &key
                        && match editor.row {
                            McpVariableRow::Add => true,
                            McpVariableRow::Edit(edit_index) => index != edit_index,
                        }
                });
                if duplicate {
                    Outcome::Duplicate
                } else {
                    match editor.row {
                        McpVariableRow::Add => table.push((key, value)),
                        McpVariableRow::Edit(index) => {
                            if let Some(slot) = table.get_mut(index) {
                                *slot = (key, value);
                            }
                        }
                    }
                    Outcome::Done
                }
            }
            None => return,
        };
        match outcome {
            Outcome::Done => {
                self.mcp_variable_editor = None;
                self.commit_mcp_servers(cx);
            }
            Outcome::Duplicate => {
                self.show_toast(tr!("mcp.duplicate_variable"));
            }
        }
    }

    fn delete_mcp_variable(
        &mut self,
        table: McpVariableTable,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.selected_mcp_server_mut() else {
            return;
        };
        let rows = mcp_server_variables_mut(server, table);
        if index < rows.len() {
            rows.remove(index);
        }
        if self.mcp_variable_editor
            == Some(McpVariableEditor {
                table,
                row: McpVariableRow::Edit(index),
            })
        {
            self.mcp_variable_editor = None;
        }
        self.commit_mcp_servers(cx);
    }

    // ── Page ───────────────────────────────────────────────────────────────

    pub(super) fn render_mcp_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .size_full()
            .min_h_0()
            .flex()
            .child(self.render_mcp_list_pane(&theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_mcp_detail(&theme, cx)),
            )
            .into_any_element()
    }

    fn render_mcp_list_pane(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let accent = mcp_accent(theme);
        let query = self.mcp_search.read(cx).content().trim().to_lowercase();

        // Two fixed sections, local then remote, each hiding while the
        // search or the roster leaves it empty — the grouping mirrors how
        // the servers differ at runtime (spawned process vs HTTP endpoint).
        let mut rows = div().px(px(8.0)).flex().flex_col();
        let mut any_row = false;
        let selected_name = self.mcp_selected.clone();
        for (label_key, kind) in [
            ("mcp.section_local", McpServerKind::Local),
            ("mcp.section_remote", McpServerKind::Remote),
        ] {
            let matches: Vec<&McpServer> = self
                .mcp_servers
                .iter()
                .filter(|server| server.kind == kind && Self::mcp_search_matches(server, &query))
                .collect();
            if matches.is_empty() {
                continue;
            }
            rows = rows.child(section_label(
                theme,
                format!("{} {}", crate::i18n::translate(label_key), matches.len()),
                !any_row,
            ));
            for server in matches {
                any_row = true;
                let selected = selected_name.as_deref() == Some(server.name.as_str());
                rows = rows.child(self.render_mcp_list_row(server, selected, theme, accent, cx));
            }
        }
        rows = rows.child(self.render_add_mcp_row(theme, cx));

        div()
            .w(px(MCP_LIST_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .pt(px(14.0))
                    .px(px(16.0))
                    .flex_none()
                    .text_size(ui_px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("settings.mcp_servers")),
            )
            .child(
                div().px(px(12.0)).pt(px(10.0)).flex_none().child(
                    TextField::new("mcp-search-field", self.mcp_search.clone())
                        .icon("icons/search.svg", 13.0),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mt(px(8.0))
                    .relative()
                    .child(
                        div()
                            .id("mcp-list-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.mcp_list_scroll)
                            .pb(px(8.0))
                            .child(rows),
                    )
                    .child(scrollbar::vertical(
                        &self.mcp_list_scroll,
                        &self.mcp_list_scrollbar,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(26.0))
                    .px(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .text_size(ui_px(9.5))
                    .text_color(theme.text_ghost)
                    .child(self.mcp_footer_caption()),
            )
    }

    fn mcp_footer_caption(&self) -> SharedString {
        let count = self.mcp_servers.len();
        SharedString::from(match count {
            0 => tr!("mcp.count_zero"),
            1 => tr!("mcp.count_one", count = count),
            _ => tr!("mcp.count_many", count = count),
        })
    }

    /// The query a roster entry must match to stay visible: its name, its
    /// command, or its URL. Purely in-memory, so the filter runs per frame.
    fn mcp_search_matches(server: &McpServer, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        format!(
            "{} {} {}",
            server.name,
            server.command.join(" "),
            server.url
        )
        .to_lowercase()
        .contains(query)
    }

    fn render_mcp_list_row(
        &self,
        server: &McpServer,
        selected: bool,
        theme: &Theme,
        accent: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = server.name.clone();
        let toggle_name = name.clone();
        let caption = mcp_row_caption(server);
        let enabled = server.enabled;
        let status = self.mcp_status(&server.name);
        div()
            .child(
                div()
                    .id(SharedString::from(format!("mcp-row-{}", server.name)))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(accent))
                    .w_full()
                    .px(px(9.0))
                    .py(px(7.0))
                    .mb(px(1.0))
                    .rounded(px(8.0))
                    .cursor_default()
                    .when(selected, |element| element.bg(accent.opacity(0.13)))
                    .when(!selected, |element| {
                        element
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .flex_none()
                            // Concentric with the row: 8px row radius minus
                            // the 7-9px row padding leaves almost no arc.
                            .rounded(px(2.0))
                            .bg(theme.overlay)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                mcp_server_icon(server.kind),
                                14.0,
                                if selected {
                                    accent
                                } else {
                                    theme.text_secondary
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if selected || enabled {
                                        theme.text
                                    } else {
                                        theme.text_secondary
                                    })
                                    .child(SharedString::from(server.name.clone())),
                            )
                            .child(
                                div()
                                    .mt(px(1.0))
                                    .min_w_0()
                                    .truncate()
                                    .font_family(crate::md::render::MONO_FAMILY)
                                    .text_size(ui_px(10.5))
                                    .text_color(theme.text_tertiary)
                                    .child(SharedString::from(caption)),
                            ),
                    )
                    .children(status.map(|entry| {
                        icon(
                            mcp_status_glyph(entry.status),
                            12.0,
                            mcp_status_color(entry.status, theme),
                        )
                    }))
                    .child(toggle_switch(
                        SharedString::from(format!("mcp-toggle-{}", server.name)),
                        enabled,
                        false,
                        *theme,
                        cx,
                        move |this, _, cx| {
                            this.toggle_mcp_server_enabled(toggle_name.clone(), cx);
                        },
                    ))
                    .on_click({
                        let name = name.clone();
                        cx.listener(move |this, _, _, cx| {
                            this.select_mcp_server(name.clone(), cx);
                        })
                    })
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.select_mcp_server(name.clone(), cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_add_mcp_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let accent = mcp_accent(theme);
        let adding = self.mcp_adding;
        div()
            .child(
                div()
                    .id("add-mcp-server")
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(accent))
                    .w_full()
                    .px(px(9.0))
                    .py(px(7.0))
                    .mt(px(10.0))
                    .rounded(px(8.0))
                    .cursor_default()
                    .border_1()
                    .border_color(if adding { accent } else { theme.border_strong })
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .text_color(if adding {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(!adding, |element| {
                        element
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .child(icon(
                        "icons/plus.svg",
                        13.0,
                        if adding { accent } else { theme.text_tertiary },
                    ))
                    .child(div().text_size(ui_px(12.5)).child(tr!("mcp.add_server")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.begin_add_mcp_server(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.begin_add_mcp_server(cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    // ── Detail pane ────────────────────────────────────────────────────────

    fn render_mcp_detail(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.mcp_adding {
            return self.render_mcp_form(theme, cx);
        }
        if let Some(server) = self.effective_mcp_server() {
            return self.render_mcp_server_detail(&server, theme, cx);
        }
        self.render_mcp_empty_detail(theme, cx)
    }

    pub(super) fn mcp_scrollable_detail(&self, content: Div) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                div()
                    .id("mcp-detail-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.mcp_detail_scroll)
                    .px(px(24.0))
                    .pt(px(18.0))
                    .pb(px(24.0))
                    .child(content),
            )
            .child(scrollbar::vertical(
                &self.mcp_detail_scroll,
                &self.mcp_detail_scrollbar,
            ))
            .into_any_element()
    }

    fn render_mcp_server_detail(
        &self,
        server: &McpServer,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = mcp_accent(theme);
        let enabled = server.enabled;
        let name = server.name.clone();

        // Header: tile, name (or rename editor), enable badge, and actions.
        let name_area: AnyElement = if self.mcp_renaming {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    TextField::new("mcp-rename-field", self.mcp_rename_input.clone()).w(px(220.0)),
                )
                .child(
                    small_action_button(
                        "confirm-mcp-rename",
                        "icons/check.svg",
                        tr!("mcp.save"),
                        accent,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_mcp_rename(cx);
                    }))
                    .on_key_down(cx.listener(
                        |this, event: &KeyDownEvent, _, cx| {
                            if !event.keystroke.modifiers.modified()
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                this.confirm_mcp_rename(cx);
                                cx.stop_propagation();
                            }
                        },
                    )),
                )
                .child(
                    small_action_button(
                        "cancel-mcp-rename",
                        "icons/x.svg",
                        tr!("common.cancel"),
                        theme.text_secondary,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_mcp_rename(cx);
                    })),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_px(15.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(server.name.clone())),
                )
                .child(
                    icon_button("rename-mcp-server", "icons/pencil.svg", *theme)
                        .tooltip(Tooltip::text(tr!("mcp.rename")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.begin_mcp_rename(cx);
                        })),
                )
                .child(enabled_badge(theme, accent, enabled))
                .into_any_element()
        };

        let toggle_button = outline_button(
            "toggle-mcp-enabled",
            if enabled {
                tr!("mcp.action_disable")
            } else {
                tr!("mcp.action_enable")
            },
            None,
            theme,
        )
        .on_click({
            let name = name.clone();
            cx.listener(move |this, _, _, cx| {
                this.toggle_mcp_server_enabled(name.clone(), cx);
            })
        })
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
            if !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                this.toggle_mcp_server_enabled(name.clone(), cx);
                cx.stop_propagation();
            }
        }));

        let armed = self.mcp_delete_arming.as_deref() == Some(server.name.as_str());
        let delete_button = div()
            .id("delete-mcp-server")
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(30.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(if armed {
                theme.danger
            } else {
                theme.border_strong
            })
            .when(armed, |element| element.bg(theme.danger.opacity(0.12)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(ui_px(12.0))
            .text_color(if armed {
                theme.danger
            } else {
                theme.text_secondary
            })
            .hover(|element| element.bg(theme.overlay).text_color(theme.danger))
            .active(|element| {
                element
                    .bg(theme.danger.opacity(0.18))
                    .text_color(theme.danger)
            })
            .child(icon(
                "icons/trash.svg",
                12.5,
                if armed {
                    theme.danger
                } else {
                    theme.text_tertiary
                },
            ))
            .child(if armed {
                tr!("mcp.confirm_delete_server")
            } else {
                tr!("mcp.delete_server")
            })
            .on_click(cx.listener({
                let name = server.name.clone();
                move |this, _, _, cx| {
                    if this.mcp_delete_arming.as_deref() == Some(name.as_str()) {
                        this.delete_mcp_server(name.clone(), cx);
                    } else {
                        this.mcp_delete_arming = Some(name.clone());
                        cx.notify();
                    }
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.mcp_delete_arming.take().is_some() {
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener({
                let name = server.name.clone();
                move |this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        if this.mcp_delete_arming.as_deref() == Some(name.as_str()) {
                            this.delete_mcp_server(name.clone(), cx);
                        } else {
                            this.mcp_delete_arming = Some(name.clone());
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                }
            }));

        // Connection: the kind dropdown, then the one field the kind speaks.
        let current_kind = server.kind;
        let weak = cx.entity().downgrade();
        let kind_handle = self.menu_handle("mcp-kind-selector", cx);
        let kind_selector = dropdown_menu(
            MenuChip::new("mcp-kind-selector")
                .label(mcp_kind_label(current_kind))
                .outlined()
                .selected(kind_handle.is_open())
                .w(px(280.0))
                .justify_between(),
            "mcp-kind-selector-menu",
            &kind_handle,
            MenuAlign::BelowLeft,
            move |_| {
                [McpServerKind::Local, McpServerKind::Remote]
                    .into_iter()
                    .map(|kind| {
                        let weak = weak.clone();
                        MenuItem::new(mcp_kind_label(kind), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                if let Some(name) = this.mcp_selected.clone() {
                                    this.set_mcp_server_kind(name, kind, cx);
                                }
                            });
                        })
                        .selected(kind == current_kind)
                    })
                    .collect()
            },
        );

        let connection_field = match current_kind {
            McpServerKind::Local => labeled_field(
                theme,
                tr!("mcp.command_label"),
                TextField::new("mcp-command-field", self.mcp_command_input.clone()).w_full(),
            ),
            McpServerKind::Remote => labeled_field(
                theme,
                tr!("mcp.url_label"),
                TextField::new("mcp-url-field", self.mcp_url_input.clone()).w_full(),
            ),
        };

        let variables_section =
            self.render_mcp_variables(server, McpVariableTable::for_kind(current_kind), theme, cx);

        let status_section = self.mcp_status(&server.name).map(|entry| {
            let color = mcp_status_color(entry.status, theme);
            div()
                .mt(px(16.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(icon(mcp_status_glyph(entry.status), 13.0, color))
                        .child(
                            div()
                                .text_size(ui_px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(color)
                                .child(mcp_status_label(entry)),
                        ),
                )
                .children(entry.error.clone().map(|error| {
                    div()
                        .text_size(ui_px(10.5))
                        .line_height(ui_px(15.0))
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(error))
                }))
                .child(
                    div()
                        .text_size(ui_px(10.5))
                        .line_height(ui_px(15.0))
                        .text_color(theme.text_tertiary)
                        .child(tr!("mcp.status_refresh_hint", name = server.name.clone())),
                )
        });

        self.mcp_scrollable_detail(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(provider_tile(theme, mcp_server_icon(current_kind), enabled))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .child(name_area),
                        )
                        .child(toggle_button)
                        .child(delete_button),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(labeled_field(theme, tr!("mcp.type_label"), kind_selector))
                        .child(connection_field),
                )
                .children(status_section)
                .children(self.render_mcp_oauth(server, theme, cx))
                .child(variables_section)
                .child(info_note(theme, "icons/info.svg", tr!("mcp.managed_note"))),
        )
    }

    fn render_mcp_oauth(
        &self,
        server: &McpServer,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if server.kind != McpServerKind::Remote {
            return None;
        }
        let current = server.oauth.mode;
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("mcp-oauth-selector", cx);
        let selector = dropdown_menu(
            MenuChip::new("mcp-oauth-selector")
                .label(mcp_oauth_mode_label(current))
                .outlined()
                .selected(handle.is_open())
                .w(px(280.0))
                .justify_between(),
            "mcp-oauth-selector-menu",
            &handle,
            MenuAlign::BelowLeft,
            move |_| {
                [
                    McpOAuthMode::Automatic,
                    McpOAuthMode::Disabled,
                    McpOAuthMode::Custom,
                ]
                .into_iter()
                .map(|mode| {
                    let weak = weak.clone();
                    MenuItem::new(mcp_oauth_mode_label(mode), move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            if let Some(name) = this.mcp_selected.clone() {
                                this.set_mcp_oauth_mode(name, mode, cx);
                            }
                        });
                    })
                    .selected(mode == current)
                })
                .collect()
            },
        );

        let mut section = div()
            .mt(px(16.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(labeled_field(theme, tr!("mcp.oauth_label"), selector))
            .child(
                div()
                    .px(px(1.0))
                    .text_size(ui_px(10.5))
                    .line_height(ui_px(15.0))
                    .text_color(theme.text_tertiary)
                    .child(tr!("mcp.oauth_note")),
            );
        if current == McpOAuthMode::Custom {
            section = section
                .child(labeled_field(
                    theme,
                    tr!("mcp.oauth_client_id"),
                    TextField::new(
                        "mcp-oauth-client-id-field",
                        self.mcp_oauth_client_id.clone(),
                    )
                    .w_full(),
                ))
                .child(labeled_field(
                    theme,
                    tr!("mcp.oauth_client_secret"),
                    TextField::new(
                        "mcp-oauth-client-secret-field",
                        self.mcp_oauth_client_secret.clone(),
                    )
                    .w_full(),
                ))
                .child(labeled_field(
                    theme,
                    tr!("mcp.oauth_scope"),
                    TextField::new("mcp-oauth-scope-field", self.mcp_oauth_scope.clone()).w_full(),
                ));
        }
        if current != McpOAuthMode::Disabled {
            let authenticating = self.mcp_oauth_auth_name.is_some();
            let waiting = self.mcp_oauth_auth_name.as_deref() == Some(server.name.as_str());
            let name = server.name.clone();
            let accent = mcp_accent(theme);
            if waiting {
                section = section.child(
                    small_action_button(
                        "mcp-oauth-cancel",
                        "icons/stop.svg",
                        tr!("mcp.oauth_cancel"),
                        theme.text_secondary,
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel_mcp_oauth_login(cx);
                    }))
                    .on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, _, cx| {
                            if !event.keystroke.modifiers.modified()
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                this.cancel_mcp_oauth_login(cx);
                                cx.stop_propagation();
                            }
                        },
                    )),
                );
            } else {
                section = section.child(
                    small_action_button(
                        "mcp-oauth-sign-in",
                        "icons/external-link.svg",
                        tr!("mcp.oauth_sign_in"),
                        if authenticating {
                            theme.text_tertiary
                        } else {
                            accent
                        },
                        theme,
                    )
                    .when(!authenticating, |element| {
                        element
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_mcp_oauth_login(name.clone(), cx);
                            }))
                            .on_key_down({
                                let name = server.name.clone();
                                cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                                    if !event.keystroke.modifiers.modified()
                                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                                    {
                                        this.start_mcp_oauth_login(name.clone(), cx);
                                        cx.stop_propagation();
                                    }
                                })
                            })
                    }),
                );
            }
        }
        Some(section)
    }

    /// The selected server's key-value table (environment for local servers,
    /// headers for remote ones) as a bordered card, with the inline editor
    /// row when open and the add row at the bottom.
    fn render_mcp_variables(
        &self,
        server: &McpServer,
        table: McpVariableTable,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = mcp_accent(theme);
        let rows_pairs = mcp_server_variables(server, table);
        let mut rows = div().flex().flex_col();
        if rows_pairs.is_empty()
            && self.mcp_variable_editor
                != Some(McpVariableEditor {
                    table,
                    row: McpVariableRow::Add,
                })
        {
            rows = rows.child(
                div()
                    .px(px(10.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon("icons/info.svg", 12.0, theme.text_tertiary))
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .line_height(ui_px(15.0))
                            .text_color(theme.text_tertiary)
                            .child(tr!("mcp.variables_empty")),
                    ),
            );
        }
        // Each visible element after the first carries a top hairline, so an
        // editor row sliding in between entries never doubles a border.
        let mut separator = !rows_pairs.is_empty();
        for (index, (key, value)) in rows_pairs.iter().enumerate() {
            if self.mcp_variable_editor
                == Some(McpVariableEditor {
                    table,
                    row: McpVariableRow::Edit(index),
                })
            {
                rows = rows.child(self.render_mcp_variable_editor_row(theme, cx, separator));
                separator = true;
                continue;
            }
            rows = rows.child(
                div()
                    .px(px(10.0))
                    .h(px(38.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .when(separator, |element| {
                        element.border_t_1().border_color(theme.border)
                    })
                    .child(
                        div()
                            .max_w(px(180.0))
                            .flex_none()
                            .truncate()
                            .font_family(crate::md::render::MONO_FAMILY)
                            .text_size(ui_px(11.0))
                            .text_color(theme.text)
                            .child(SharedString::from(key.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::md::render::MONO_FAMILY)
                            .text_size(ui_px(9.5))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(value.clone())),
                    )
                    .child(
                        icon_button(
                            SharedString::from(format!("edit-mcp-variable-{index}")),
                            "icons/pencil.svg",
                            *theme,
                        )
                        .tooltip(Tooltip::text(tr!("mcp.edit_variable")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.begin_mcp_variable_editor(
                                McpVariableEditor {
                                    table,
                                    row: McpVariableRow::Edit(index),
                                },
                                cx,
                            );
                        })),
                    )
                    .child(
                        icon_button(
                            SharedString::from(format!("delete-mcp-variable-{index}")),
                            "icons/trash.svg",
                            *theme,
                        )
                        .tooltip(Tooltip::text(tr!("mcp.delete_variable")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.delete_mcp_variable(table, index, cx);
                        })),
                    ),
            );
            separator = true;
        }

        if self.mcp_variable_editor
            == Some(McpVariableEditor {
                table,
                row: McpVariableRow::Add,
            })
        {
            rows = rows.child(self.render_mcp_variable_editor_row(theme, cx, separator));
        } else {
            rows = rows.child(
                div()
                    .id(SharedString::from(format!(
                        "add-mcp-variable-{}",
                        match table {
                            McpVariableTable::Environment => "env",
                            McpVariableTable::Headers => "headers",
                        }
                    )))
                    .tab_index(0)
                    .focus_visible(|style| style.border_color(accent))
                    .h(px(36.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .px(px(10.0))
                    .when(separator, |element| {
                        element.border_t_1().border_color(theme.border)
                    })
                    .cursor_default()
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_secondary)
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .child(icon("icons/plus.svg", 12.5, theme.text_tertiary))
                    .child(table.add_label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.begin_mcp_variable_editor(
                            McpVariableEditor {
                                table,
                                row: McpVariableRow::Add,
                            },
                            cx,
                        );
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.begin_mcp_variable_editor(
                                McpVariableEditor {
                                    table,
                                    row: McpVariableRow::Add,
                                },
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    })),
            );
        }

        div()
            .mt(px(18.0))
            .child(section_label(theme, table.label(), true))
            .child(
                div()
                    .mt(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(9.0))
                    .overflow_hidden()
                    .child(rows),
            )
            .into_any_element()
    }

    /// The inline variable editor: key + value, confirmed with the check
    /// button or Enter in either field.
    fn render_mcp_variable_editor_row(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
        separator: bool,
    ) -> AnyElement {
        let accent = mcp_accent(theme);
        let key = self
            .mcp_variable_key_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let valid = !key.is_empty();

        div()
            .px(px(10.0))
            .py(px(8.0))
            .bg(theme.inset)
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(separator, |element| {
                element.border_t_1().border_color(theme.border)
            })
            .child(
                TextField::new(
                    "mcp-variable-key-field",
                    self.mcp_variable_key_input.clone(),
                )
                .flex_1()
                .min_w_0(),
            )
            .child(
                TextField::new(
                    "mcp-variable-value-field",
                    self.mcp_variable_value_input.clone(),
                )
                .flex_1()
                .min_w_0(),
            )
            .child(
                div()
                    .id("confirm-mcp-variable")
                    .tab_index(0)
                    .focus_visible(|style| style.border_color(accent))
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_default()
                    .text_size(ui_px(12.0))
                    .when(valid, |element| {
                        element
                            .bg(accent)
                            .text_color(on_mcp_accent(theme))
                            .hover(|element| element.bg(accent.opacity(0.85)))
                            .active(|element| element.bg(accent.opacity(0.72)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_mcp_variable_editor(cx);
                            }))
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if !event.keystroke.modifiers.modified()
                                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                                {
                                    this.confirm_mcp_variable_editor(cx);
                                    cx.stop_propagation();
                                }
                            }))
                    })
                    .when(!valid, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_ghost)
                    })
                    .child(icon(
                        "icons/check.svg",
                        12.5,
                        if valid {
                            on_mcp_accent(theme)
                        } else {
                            theme.text_ghost
                        },
                    ))
                    .child(tr!("mcp.save")),
            )
            .child(
                small_action_button(
                    "cancel-mcp-variable",
                    "icons/x.svg",
                    tr!("common.cancel"),
                    theme.text_secondary,
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_mcp_variable_editor(cx);
                })),
            )
            .into_any_element()
    }

    /// No servers on the roster: a quiet empty state with the one action
    /// that leaves it.
    fn render_mcp_empty_detail(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let accent = mcp_accent(theme);
        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .child(
                div()
                    .w(px(44.0))
                    .h(px(44.0))
                    .rounded(px(11.0))
                    .bg(theme.overlay)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("icons/wrench.svg", 20.0, theme.text_tertiary)),
            )
            .child(
                div()
                    .text_size(ui_px(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("mcp.count_zero")),
            )
            .child(
                div()
                    .max_w(px(340.0))
                    .text_center()
                    .text_size(ui_px(11.5))
                    .line_height(ui_px(17.0))
                    .text_color(theme.text_secondary)
                    .child(tr!("mcp.empty_description")),
            )
            .child(
                div()
                    .id("add-mcp-server-empty")
                    .tab_index(0)
                    .focus_visible(|style| style.border_color(accent))
                    .mt(px(6.0))
                    .h(px(32.0))
                    .px(px(16.0))
                    .rounded(px(8.0))
                    .bg(accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .text_size(ui_px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(on_mcp_accent(theme))
                    .hover(|element| element.bg(accent.opacity(0.85)))
                    .child(tr!("mcp.add_server"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.begin_add_mcp_server(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.begin_add_mcp_server(cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    // ── Add-server form ────────────────────────────────────────────────────

    fn render_mcp_form(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let accent = mcp_accent(theme);
        let name = self.mcp_form_name.read(cx).content().trim().to_owned();
        let kind = self.mcp_form_kind;
        let command = mcp_parse_command(self.mcp_form_command.read(cx).content());
        let url = self.mcp_form_url.read(cx).content().trim().to_owned();
        let valid = !name.is_empty()
            && match kind {
                McpServerKind::Local => !command.is_empty(),
                McpServerKind::Remote => mcp_url_valid(&url),
            };

        let weak = cx.entity().downgrade();
        let kind_handle = self.menu_handle("mcp-form-kind-selector", cx);
        let kind_selector = dropdown_menu(
            MenuChip::new("mcp-form-kind-selector")
                .label(mcp_kind_label(kind))
                .outlined()
                .selected(kind_handle.is_open())
                .w(px(280.0))
                .justify_between(),
            "mcp-form-kind-selector-menu",
            &kind_handle,
            MenuAlign::BelowLeft,
            move |_| {
                [McpServerKind::Local, McpServerKind::Remote]
                    .into_iter()
                    .map(|candidate| {
                        let weak = weak.clone();
                        MenuItem::new(mcp_kind_label(candidate), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.mcp_form_kind = candidate;
                                cx.notify();
                            });
                        })
                        .selected(candidate == kind)
                    })
                    .collect()
            },
        );

        let submit_button = div()
            .id("submit-mcp-form")
            .tab_index(0)
            .focus_visible(|style| style.border_color(accent))
            .h(px(32.0))
            .px(px(16.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_size(ui_px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .when(valid, |element| {
                element
                    .bg(accent)
                    .text_color(on_mcp_accent(theme))
                    .hover(|element| element.bg(accent.opacity(0.85)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.submit_mcp_form(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.submit_mcp_form(cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .when(!valid, |element| {
                element
                    .border_1()
                    .border_color(theme.border_strong)
                    .text_color(theme.text_ghost)
            })
            .child(tr!("mcp.add_server"));

        let hint: Option<AnyElement> = match kind {
            McpServerKind::Remote if !url.is_empty() && !mcp_url_valid(&url) => {
                Some(form_hint(theme, accent, tr!("mcp.hint_invalid_url")))
            }
            _ => None,
        };

        self.mcp_scrollable_detail(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(ui_px(15.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(tr!("mcp.form_title")),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(ui_px(11.5))
                        .line_height(ui_px(17.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("mcp.form_description")),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(labeled_field(
                            theme,
                            tr!("mcp.name_label"),
                            TextField::new("mcp-form-name-field", self.mcp_form_name.clone())
                                .w_full(),
                        ))
                        .child(labeled_field(theme, tr!("mcp.type_label"), kind_selector))
                        .child(match kind {
                            McpServerKind::Local => labeled_field(
                                theme,
                                tr!("mcp.command_label"),
                                TextField::new(
                                    "mcp-form-command-field",
                                    self.mcp_form_command.clone(),
                                )
                                .w_full(),
                            ),
                            McpServerKind::Remote => labeled_field(
                                theme,
                                tr!("mcp.url_label"),
                                TextField::new("mcp-form-url-field", self.mcp_form_url.clone())
                                    .w_full(),
                            ),
                        }),
                )
                .child(
                    div()
                        .mt(px(18.0))
                        .pt(px(14.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .children(hint)
                        .child(div().flex_1())
                        .child(
                            outline_button("cancel-mcp-form", tr!("common.cancel"), None, theme)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.mcp_adding = false;
                                    cx.notify();
                                })),
                        )
                        .child(submit_button),
                ),
        )
    }
}
