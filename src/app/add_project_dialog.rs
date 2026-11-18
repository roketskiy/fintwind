use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    theme::ControlSize,
};
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton, ParentElement,
    PathPromptOptions, SharedString, Styled, Window, div, prelude::*, px,
};

use crate::input::{ComposerEvent, ComposerInput};
use crate::model::Project;
use crate::theme::{Theme, ui_px};
use crate::ui::loading::{LoadingDelay, LoadingPhase};
use crate::ui::text_field::TextField;
use crate::ui::{ActivationExt, icon, icon_button};

use super::Fintwind;

use crate::ui::{
    MenuChip,
    menu::{MenuAlign, MenuItem, dropdown_menu},
};
use uuid::Uuid;
use fintwind_client::ssh::{GitCloneProgress, GitCloneProgressReader, GitCloneStage};
use super::ToastTone;

pub(super) const ADD_PROJECT_LAYER_PRIORITY: usize = 4;

pub(super) struct AddProjectDialogState {
    id: Uuid,
    window: gpui::AnyWindowHandle,
    server: Option<Uuid>,
    remote_path: Entity<ComposerInput>,
    method: ProjectMethod,
    repository: Entity<ComposerInput>,
    directory_name: Entity<ComposerInput>,
    pending: bool,
    created_remote_directory: Option<(RemoteProjectTarget, PathBuf)>,
    clone_toast: Option<u64>,
    loading: Option<LoadingDelay<()>>,
    error: Option<String>,
    folder: Option<PathBuf>,
    name: Entity<ComposerInput>,
    folder_focus: FocusHandle,
}

#[derive(Clone, Default)]
pub(super) struct CloneProgress(Arc<Mutex<Option<GitCloneProgress>>>);

impl CloneProgress {
    pub(super) fn publish(&self, progress: GitCloneProgress) {
        *self.0.lock().unwrap() = Some(progress);
    }
}

#[cfg(test)]
pub(super) fn ssh_dialog_inputs(app: &Fintwind) -> (Entity<ComposerInput>, Entity<ComposerInput>) {
    let dialog = app.add_project_dialog.as_ref().unwrap();
    (dialog.remote_path.clone(), dialog.directory_name.clone())
}

#[cfg(test)]
pub(super) fn project_repository_input(app: &Fintwind) -> Entity<ComposerInput> {
    app.add_project_dialog.as_ref().unwrap().repository.clone()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectMethod {
    Existing,
    Empty,
    Clone,
}

#[derive(PartialEq, Eq)]
struct RemoteProjectTarget {
    server: Option<Uuid>,
    parent: String,
    folder: String,
    repository: Option<String>,
}

fn github_repository(address: &str) -> Option<(url::Url, String)> {
    let address = url::Url::parse(address).ok()?;
    if address.scheme() != "https"
        || address.host_str() != Some("github.com")
        || !address.username().is_empty()
        || address.password().is_some()
        || address.port().is_some()
        || address.query().is_some()
        || address.fragment().is_some()
    {
        return None;
    }
    let (owner, repo) = address.path().trim_matches('/').split_once('/')?;
    let name = repo.strip_suffix(".git").unwrap_or(repo);
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    let name = (valid(owner) && valid(name)).then(|| name.to_owned())?;
    Some((address, name))
}

async fn clone_local_project(
    parent: &Path,
    folder: &str,
    repository: url::Url,
    progress: CloneProgress,
) -> anyhow::Result<PathBuf> {
    let mut components = Path::new(folder).components();
    anyhow::ensure!(
        matches!(components.next(), Some(Component::Normal(name)) if name == folder)
            && components.next().is_none()
            && !folder.contains(['/', '\\']),
        tr!("project.invalid_folder")
    );
    let target = parent.join(folder);
    std::fs::create_dir(&target).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!(tr!("project.target_exists"))
        } else {
            anyhow::Error::from(error)
        }
    })?;
    let mut command = std::process::Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let mut command = smol::process::Command::from(command);
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["clone", "--progress", "--", repository.as_str()])
        .arg(&target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let result = futures_lite::future::race(
        async {
            use futures_lite::io::AsyncReadExt as _;
            let mut child = command.spawn()?;
            let mut stderr = child.stderr.take().expect("git stderr is piped");
            let mut reader = GitCloneProgressReader::default();
            let mut tail = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let size = stderr.read(&mut chunk).await?;
                if size == 0 {
                    break;
                }
                reader.push(&chunk[..size], |update| progress.publish(update));
                tail.extend_from_slice(&chunk[..size]);
                if tail.len() > 8192 {
                    tail.drain(..tail.len() - 8192);
                }
            }
            let status = child.status().await?;
            anyhow::ensure!(
                status.success(),
                "{}",
                String::from_utf8_lossy(&tail).lines().rev().find(|line| !line.trim().is_empty())
                    .unwrap_or("Git clone failed")
            );
            Ok(())
        },
        async {
            smol::Timer::after(Duration::from_secs(300)).await;
            Err(anyhow::anyhow!(tr!("project.clone_timeout")))
        },
    )
    .await;
    if let Err(error) = result {
        let _ = std::fs::remove_dir(&target);
        return Err(error);
    }
    Ok(target)
}

fn create_empty_local_project(
    client: fintwind_client::WorkspaceClient,
    parent_dir: PathBuf,
    project_name: String,
) -> anyhow::Result<PathBuf> {
    use fintwind_protocol::workspace::{
        ProjectOperationState, WorkspaceOperation, WorkspaceResult, project_operation,
    };

    let result = client
        .request(WorkspaceOperation::CreateEmptyProject {
            operation_id: Uuid::new_v4(),
            parent_dir,
            project_name,
        })
        .map_err(
            |error| match project_operation::kind_of(&error.to_string()) {
                Some(project_operation::INVALID_NAME) => {
                    anyhow::anyhow!(tr!("project.invalid_folder"))
                }
                _ => error,
            },
        )?;
    match result {
        WorkspaceResult::ProjectOperation {
            state: ProjectOperationState::Succeeded,
            path,
            ..
        } => Ok(path),
        WorkspaceResult::ProjectOperation {
            state: ProjectOperationState::Failed,
            failure_kind,
            ..
        } if failure_kind.as_deref() == Some(project_operation::TARGET_EXISTS) => {
            Err(anyhow::anyhow!(tr!("project.target_exists")))
        }
        WorkspaceResult::ProjectOperation {
            state: ProjectOperationState::Unknown | ProjectOperationState::NeedsReconciliation,
            ..
        } => Err(anyhow::anyhow!(tr!("project.creation_unknown"))),
        _ => Err(anyhow::anyhow!(tr!("project.creation_failed"))),
    }
}

impl Fintwind {
    fn start_clone_toast(&mut self, cx: &mut Context<Self>) -> CloneProgress {
        self.show_toast_with_tone(tr!("project.cloning"), ToastTone::Progress(None));
        let toast_id = self.toast_generation;
        self.add_project_dialog.as_mut().unwrap().clone_toast = Some(toast_id);
        let progress = CloneProgress::default();
        let reader = progress.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(125)).await;
                let update = reader.0.lock().unwrap().take();
                let active = this.update(cx, |app, cx| {
                    let Some(toast) = app.toast.as_mut().filter(|toast| {
                        toast.id == toast_id && matches!(toast.tone, ToastTone::Progress(_))
                    }) else { return false; };
                    if let Some(GitCloneProgress { stage, percent }) = update {
                        let tone = ToastTone::Progress(Some(percent));
                        let message = match stage {
                            GitCloneStage::Counting => tr!("project.clone_counting", percent = percent),
                            GitCloneStage::Compressing => tr!("project.clone_compressing", percent = percent),
                            GitCloneStage::Receiving => tr!("project.clone_receiving", percent = percent),
                            GitCloneStage::Resolving => tr!("project.clone_resolving", percent = percent),
                            GitCloneStage::Updating => tr!("project.clone_updating", percent = percent),
                        };
                        if toast.tone != tone || toast.message != message {
                            toast.tone = tone;
                            toast.message = message;
                            cx.notify();
                        }
                    }
                    true
                });
                if !matches!(active, Ok(true)) {
                    break;
                }
            }
        }).detach();
        progress
    }

    fn finish_add_project_toast(&mut self, message: String, tone: ToastTone, always_show: bool) {
        let clone_toast = self.add_project_dialog.as_mut().and_then(|dialog| dialog.clone_toast.take());
        if let Some(toast) = self.toast.as_mut().filter(|toast| Some(toast.id) == clone_toast) {
            toast.tone = tone;
            toast.message = message;
        } else if always_show || clone_toast.is_some() {
            self.show_toast_with_tone(message, tone);
        }
    }

    pub(super) fn add_project_targets_host(&self, host: Uuid) -> bool {
        self.add_project_dialog.as_ref().is_some_and(|dialog| dialog.server == Some(host))
    }

    pub(super) fn report_add_project_error(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.add_project_dialog {
            dialog.pending = false;
            dialog.error = Some(error.clone());
        }
        self.finish_add_project_toast(tr!("project.add_failed", error = error), ToastTone::Alert, true);
        cx.notify();
    }

    pub(super) fn open_add_project_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.daemon.is_remote() {
            self.load_ssh_profiles(cx);
        }
        let name = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("project.name_placeholder"))
        });
        cx.subscribe(&name, |this: &mut Self, _, event: &ComposerEvent, cx| {
            if matches!(event, ComposerEvent::Submit(_)) {
                this.confirm_add_project(cx);
            }
        })
        .detach();
        let folder_focus = cx.focus_handle();
        let remote_path = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder("/home/user/project")
        });
        let repository = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder("https://github.com/owner/repo")
        });
        let directory_name = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("project.target_folder"))
        });
        for input in [&repository, &directory_name] {
            cx.subscribe(input, |this: &mut Self, _, event: &ComposerEvent, cx| {
                if matches!(event, ComposerEvent::Submit(_)) {
                    this.confirm_add_project(cx);
                }
                cx.notify();
            })
            .detach();
        }
        cx.subscribe(
            &repository,
            |this: &mut Self, input, event: &ComposerEvent, cx| {
                if let ComposerEvent::Edited = event
                    && let Some(dialog) = &this.add_project_dialog
                    && dialog.repository == input
                {
                    let placeholder = github_repository(input.read(cx).content().trim())
                        .map(|(_, name)| name)
                        .unwrap_or_else(|| tr!("project.target_folder"));
                    dialog
                        .directory_name
                        .update(cx, |input, cx| input.set_placeholder(placeholder, cx));
                }
            },
        )
        .detach();
        cx.subscribe(
            &remote_path,
            |this: &mut Self, _, event: &ComposerEvent, cx| {
                if matches!(event, ComposerEvent::Submit(_)) {
                    this.confirm_add_project(cx);
                }
                cx.notify();
            },
        )
        .detach();
        self.add_project_dialog = Some(AddProjectDialogState {
            id: Uuid::new_v4(),
            window: window.window_handle(),
            server: None,
            remote_path,
            method: ProjectMethod::Existing,
            repository,
            directory_name,
            pending: false,
            created_remote_directory: None,
            clone_toast: None,
            loading: None,
            error: None,
            folder: None,
            name,
            folder_focus: folder_focus.clone(),
        });
        // The deferred layer joins the dispatch tree only after it draws;
        // focus the folder area two frames later so keys cannot fall through.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&folder_focus, cx));
        });
        cx.notify();
    }

    pub(super) fn close_add_project_dialog(&mut self, cx: &mut Context<Self>) {
        if self
            .add_project_dialog
            .as_ref()
            .is_some_and(|d| d.pending && d.method != ProjectMethod::Existing)
        {
            return;
        }
        self.add_project_dialog = None;
        cx.notify();
    }

    fn pick_add_project_folder(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .add_project_dialog
            .as_ref()
            .filter(|dialog| !dialog.pending)
            .map(|dialog| dialog.id)
        else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr!("project.add_project").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    let Some(dialog) = this
                        .add_project_dialog
                        .as_mut()
                        .filter(|d| d.id == id && d.server.is_none() && !d.pending)
                    else {
                        return;
                    };
                    let previous_default = dialog
                        .folder
                        .as_ref()
                        .and_then(|folder| folder.file_name())
                        .and_then(|name| name.to_str())
                        .map(str::to_owned);
                    let current = dialog.name.read(cx).content().trim().to_owned();
                    let next_default = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Project")
                        .to_owned();
                    // Prefill the folder name, but never overwrite a name the
                    // user typed themselves.
                    if current.is_empty() || Some(current) == previous_default {
                        dialog
                            .name
                            .update(cx, |input, cx| input.set_content(next_default, cx));
                    }
                    dialog.folder = Some(path);
                    dialog.error = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn confirm_add_project(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.add_project_dialog.as_ref() else {
            return;
        };
        if dialog.pending || self.project_host_connection(dialog.server).0 {
            return;
        }
        if let Some(server) = dialog.server
            && self.ssh_client_for(server).is_none()
        {
            self.connect_project_host(server, cx);
            return;
        }
        if dialog.server.is_some() || self.daemon.is_remote() {
            self.confirm_ssh_project(cx);
            return;
        }
        let Some(path) = dialog.folder.clone() else {
            return;
        };
        if dialog.method != ProjectMethod::Existing {
            let repository = if dialog.method == ProjectMethod::Clone {
                let Some(repository) =
                    github_repository(dialog.repository.read(cx).content().trim())
                else {
                    self.report_add_project_error(tr!("project.invalid_github_url"), cx);
                    return;
                };
                Some(repository)
            } else {
                None
            };
            let folder = dialog.directory_name.read(cx).content().trim().to_owned();
            let folder = if folder.is_empty() {
                let Some((_, name)) = repository.as_ref() else {
                    return;
                };
                name.clone()
            } else {
                folder
            };
            let id = dialog.id;
            let client = fintwind_client::WorkspaceClient::new(self.daemon.client());
            let dialog = self.add_project_dialog.as_mut().unwrap();
            dialog.pending = true;
            dialog.error = None;
            let progress = if repository.is_some() { self.start_clone_toast(cx) } else { CloneProgress::default() };
            cx.notify();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        match repository {
                            Some((address, _)) => {
                                clone_local_project(&path, &folder, address, progress).await
                            }
                            None => create_empty_local_project(client, path, folder),
                        }
                    })
                    .await;
                let _ = this.update(cx, |app, cx| {
                    if app.add_project_dialog.as_ref().is_none_or(|dialog| dialog.id != id) {
                        return;
                    }
                    match result {
                        Ok(path) => {
                            app.finish_add_project_toast(tr!("project.added"), ToastTone::Success, false);
                            app.add_project_dialog = None;
                            app.add_project_directory(path, String::new(), cx);
                        }
                        Err(error) => app.report_add_project_error(error.to_string(), cx),
                    }
                });
            })
            .detach();
            return;
        }
        let name = dialog.name.read(cx).content().trim().to_owned();
        self.add_project_dialog = None;
        self.add_project_directory(path, name, cx);
    }

    pub(super) fn add_project_directory(
        &mut self,
        path: PathBuf,
        name: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(existing) = self
            .state
            .projects
            .iter()
            .find(|project| project.path == path)
        {
            self.select_project(existing.id, cx);
        } else {
            let mut project = Project::from_path(path);
            if !name.is_empty() {
                project.name = name;
            }
            let project_id = project.id;
            self.state.projects.push(project);
            self.create_session_for(project_id, cx);
        }
        cx.notify();
    }

    fn confirm_ssh_project(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.add_project_dialog.as_mut() else {
            return;
        };
        let path = dialog.remote_path.read(cx).content().trim().to_owned();
        let id = dialog.id;
        let server = dialog.server;
        let window_handle = dialog.window;
        let method = dialog.method;
        let name = if method == ProjectMethod::Existing {
            dialog.name.read(cx).content().trim().to_owned()
        } else {
            String::new()
        };
        let mut folder = dialog.directory_name.read(cx).content().trim().to_owned();
        let repository = if method == ProjectMethod::Clone {
            let Some((address, suggested)) =
                github_repository(dialog.repository.read(cx).content().trim())
            else {
                self.report_add_project_error(tr!("project.invalid_github_url"), cx);
                return;
            };
            if folder.is_empty() {
                folder = suggested;
            }
            Some(address.as_str().trim_end_matches('/').to_owned())
        } else {
            None
        };
        let creator = self.ssh_project_creator(server);
        let client = match server {
            Some(id) => self.ssh_client_for(id),
            None => Some(self.daemon.client()),
        };
        let dialog = self.add_project_dialog.as_mut().expect("dialog still open");
        let Some(client) = client else {
            self.report_add_project_error(tr!("ssh.offline"), cx);
            return;
        };
        let target = RemoteProjectTarget {
            server,
            parent: path,
            folder,
            repository,
        };
        let previous_directory = dialog
            .created_remote_directory
            .as_ref()
            .filter(|(previous, _)| method != ProjectMethod::Existing && previous == &target)
            .map(|(_, directory)| directory.clone());
        dialog.pending = true;
        dialog.error = None;
        let progress = if method == ProjectMethod::Clone && previous_directory.is_none() {
            self.start_clone_toast(cx)
        } else {
            CloneProgress::default()
        };
        #[cfg(test)]
        let fail_first_registration = method == ProjectMethod::Clone
            && previous_directory.is_none()
            && std::env::var_os("FINTWIND_SSH_E2E_REGISTRATION_RETRY").is_some();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let operation_progress = progress.clone();
            let path = target.parent.clone();
            let folder = target.folder.clone();
            let repository = target.repository.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    if let Some(directory) = previous_directory {
                        return Ok(directory);
                    }
                    if method == ProjectMethod::Existing {
                        return super::ssh_page::remote_project_directory(client, path);
                    }
                    let created = creator.ok_or_else(|| anyhow::anyhow!(tr!("ssh.offline")))?(
                        path, folder, repository, operation_progress,
                    )?;
                    Ok(PathBuf::from(created))
                })
                .await;
            progress.0.lock().unwrap().take();
            if let Some(host) = server && let Ok(directory) = &result {
                let _ = this.update(cx, |app, cx| {
                    let toast_id = app.add_project_dialog.as_ref()
                        .filter(|dialog| dialog.id == id)
                        .and_then(|dialog| dialog.clone_toast);
                    if let Some(toast) = app.toast.as_mut().filter(|toast| Some(toast.id) == toast_id) {
                        toast.tone = ToastTone::Progress(None);
                        toast.message = tr!("project.opening_remote");
                        cx.notify();
                    }
                });
                let directory = directory.clone();
                let destination = directory.clone();
                let name = name.clone();
                let client = this.update(cx, |app, _| app.ssh_client_for(host)).ok().flatten();
                #[cfg(test)]
                let client = if fail_first_registration { None } else { client };
                let registered = cx.background_executor().spawn(async move {
                    let client = client.ok_or_else(|| anyhow::anyhow!(tr!("ssh.offline")))?;
                    super::ssh_page::register_remote_project(&client, destination, name)
                }).await;
                let _ = window_handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |app, cx| {
                        let Some(dialog) = app.add_project_dialog.as_mut().filter(|dialog| dialog.id == id) else { return; };
                        if method != ProjectMethod::Existing {
                            dialog.created_remote_directory = Some((target, directory));
                        }
                        match registered {
                            Ok(project) => {
                                if app.open_registered_ssh_project(host, project, window, cx) {
                                    if let Some(workspace) = app.active_ssh_workspace() {
                                        workspace.update(cx, |workspace, cx| {
                                            workspace.show_toast_with_tone(tr!("project.added"), ToastTone::Success);
                                            cx.notify();
                                        });
                                    }
                                    let toast_id = app.add_project_dialog.as_ref().and_then(|dialog| dialog.clone_toast);
                                    if app.toast.as_ref().is_some_and(|toast| Some(toast.id) == toast_id) {
                                        app.toast = None;
                                    }
                                    app.add_project_dialog = None;
                                } else {
                                    app.report_add_project_error(tr!("ssh.offline"), cx);
                                }
                            }
                            Err(error) => app.report_add_project_error(error.to_string(), cx),
                        }
                    });
                });
                return;
            }
            let _ = this.update(cx, |app, cx| {
                if app.add_project_dialog.as_ref().is_none_or(|dialog| dialog.id != id) {
                    return;
                }
                if method != ProjectMethod::Existing
                    && let Ok(directory) = &result
                    && let Some(dialog) = app.add_project_dialog.as_mut()
                {
                    dialog.created_remote_directory = Some((target, directory.clone()));
                }
                match result {
                    Ok(path) => {
                        app.finish_add_project_toast(tr!("project.added"), ToastTone::Success, false);
                        app.add_project_dialog = None;
                        app.add_project_directory(path, name, cx);
                    }
                    Err(error) => app.report_add_project_error(error.to_string(), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_add_project_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let server = self.add_project_dialog.as_ref()?.server;
        let (connecting, can_connect, connection_error) = self.project_host_connection(server);
        let needs_connection = server.is_some_and(|host| self.ssh_client_for(host).is_none());
        let (loading_phase, wake) = {
            let dialog = self.add_project_dialog.as_mut()?;
            LoadingDelay::observe(&mut dialog.loading, (), dialog.pending || connecting)
        };
        if let Some(wake) = wake {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wake.delay()).await;
                wake.fired();
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let dialog = self.add_project_dialog.as_ref()?;
        let theme = Theme::current(cx);
        let folder = dialog.folder.clone();
        let name = dialog.name.clone();
        let folder_focus = dialog.folder_focus.clone();
        let weak = cx.entity().downgrade();
        let remote = server.is_some() || self.daemon.is_remote();
        let pending = dialog.pending || connecting;
        let error = dialog.error.clone().or(connection_error);
        let remote_path = dialog.remote_path.clone();
        let method = dialog.method;
        let repository = dialog.repository.clone();
        let directory_name = dialog.directory_name.clone();
        let can_create = !remote || self.ssh_project_creator(server).is_some();
        let repo_address = repository.read(cx).content().trim().to_owned();
        let suggested_folder = github_repository(&repo_address).map(|(_, name)| name);
        let target_folder = directory_name.read(cx).content().trim().to_owned();
        let effective_folder = if method == ProjectMethod::Clone && target_folder.is_empty() {
            suggested_folder.as_deref().unwrap_or_default()
        } else {
            &target_folder
        };
        let remote_directory = remote_path.read(cx).content().trim().to_owned();
        let showing = pending && loading_phase == LoadingPhase::Visible;
        let has_directory = if remote {
            remote_directory.starts_with('/')
        } else {
            folder.is_some()
        };
        let can_add = has_directory
            && (method == ProjectMethod::Existing || (can_create && !effective_folder.is_empty()))
            && (method != ProjectMethod::Clone || !repo_address.is_empty());
        let can_confirm = !pending
            && !showing
            && if needs_connection {
                can_connect
            } else {
                can_add
            };
        let confirm_label = if needs_connection {
            tr!("ssh.connect")
        } else {
            match method {
                ProjectMethod::Existing => tr!("project.add_confirm"),
                ProjectMethod::Empty => tr!("project.create_and_add"),
                ProjectMethod::Clone => tr!("project.clone_and_add"),
            }
        };
        let cloning = dialog.pending && method == ProjectMethod::Clone;
        let target_preview = (method != ProjectMethod::Existing && !effective_folder.is_empty())
            .then(|| {
                if remote && remote_directory.starts_with('/') {
                    Some(format!(
                        "{}/{effective_folder}",
                        remote_directory.trim_end_matches('/')
                    ))
                } else if !remote {
                    folder
                        .as_ref()
                        .map(|parent| parent.join(effective_folder).display().to_string())
                } else {
                    None
                }
            })
            .flatten();
        let hosts = self.saved_ssh_hosts();
        let location_label = self
            .ssh_project_machine()
            .map(str::to_owned)
            .or_else(|| {
                hosts
                    .iter()
                    .find(|(id, _)| Some(*id) == server)
                    .map(|(_, name)| name.clone())
            })
            .unwrap_or_else(|| {
                if remote {
                    tr!("ssh.offline")
                } else {
                    tr!("project.location_local")
                }
            });
        let fixed_location = self.daemon.is_remote();
        #[cfg(test)]
        let fixed_location = fixed_location && !self.ssh_test_location_picker();
        let location = if fixed_location {
            div().child(location_label).into_any_element()
        } else {
            let handle = self
                .menu_handle("add-project-location", cx)
                .with_priority(ADD_PROJECT_LAYER_PRIORITY + 1);
            let app = weak.clone();
            let choices = std::iter::once((None, tr!("project.location_local")))
                .chain(hosts.into_iter().map(|(id, name)| (Some(id), name)))
                .collect::<Vec<_>>();
            dropdown_menu(
                MenuChip::new("add-project-location")
                    .label(location_label)
                    .outlined()
                    .debug_selector(|| "add-project-location".into()),
                "add-project-location-menu",
                &handle,
                MenuAlign::BelowLeft,
                move |_| {
                    choices
                        .iter()
                        .map(|(id, label)| {
                            let id = *id;
                            let app = app.clone();
                            MenuItem::new(label.clone(), move |_, cx| {
                                let _ = app.update(cx, |app, cx| {
                                    if let Some(dialog) = app
                                        .add_project_dialog
                                        .as_mut()
                                        .filter(|dialog| !pending && !dialog.pending)
                                    {
                                        dialog.server = id;
                                        dialog.error = None;
                                        cx.notify();
                                    }
                                });
                            })
                            .selected(id == server)
                            .disabled(pending)
                        })
                        .collect()
                },
            )
            .into_any_element()
        };

        let label = |text: String| {
            div()
                .text_size(ui_px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_secondary)
                .child(text)
        };
        let methods = SegmentedControl::new(
            "add-project-method",
            match method {
                ProjectMethod::Existing => "existing",
                ProjectMethod::Clone => "clone",
                ProjectMethod::Empty => "empty",
            },
        )
        .segment("existing", tr!("project.method_existing"), None)
        .segment("clone", tr!("project.method_clone"), None)
        .segment("empty", tr!("project.method_empty"), None)
        .size(ControlSize::Sm)
        .on_change(cx.listener(move |app, value: &SharedString, _, cx| {
            if let Some(dialog) = app
                .add_project_dialog
                .as_mut()
                .filter(|dialog| !pending && !dialog.pending)
            {
                dialog.method = match value.as_ref() {
                    "clone" => ProjectMethod::Clone,
                    "empty" => ProjectMethod::Empty,
                    _ => ProjectMethod::Existing,
                };
                dialog.error = None;
                cx.notify();
            }
        }));

        let pick_weak = weak.clone();
        let key_weak = weak.clone();
        let folder_label = folder
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr!("project.folder_pick"));
        let folder_area = div()
            .id("add-project-folder")
            .track_focus(&folder_focus)
            .tab_index(0)
            .h(px(32.0))
            .w_full()
            .px(px(9.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.composer)
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .focus_visible(|style| style.border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay))
            .on_click(move |_, _, cx| {
                let _ = pick_weak.update(cx, |this, cx| this.pick_add_project_folder(cx));
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    let _ = key_weak.update(cx, |this, cx| this.pick_add_project_folder(cx));
                    cx.stop_propagation();
                }
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(ui_px(12.5))
                    .text_color(if folder.is_some() {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(folder_label),
            )
            .child(icon("icons/folder.svg", 15.0, theme.text_tertiary));

        let close_weak = weak.clone();
        let cancel_weak = weak.clone();
        let confirm_focus = self.transcript_control_focus("add-project-confirm", cx);
        let card = div()
            .id("add-project-dialog-card")
            .w_full()
            .max_w(px(560.0))
            .h(px(460.0))
            .max_h_full()
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .shadow_lg()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    this.close_add_project_dialog(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .h(px(48.0))
                    .px(px(16.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(ui_px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(tr!("project.add_project")),
                    )
                    .child(
                        icon_button("add-project-close", "icons/x.svg", theme).on_click(
                            move |_, _, cx| {
                                let _ = close_weak
                                    .update(cx, |this, cx| this.close_add_project_dialog(cx));
                            },
                        ),
                    ),
            )
            .child(
                div()
                    .id("add-project-fields")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(20.0))
                    .pb(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(methods)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(label(tr!("project.location_label")))
                            .child(location),
                    )
                    .when(method == ProjectMethod::Clone, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(label(tr!("project.repository")))
                                .child(
                                    TextField::new("add-project-repository", repository).w_full(),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label(if method == ProjectMethod::Existing {
                                tr!("project.folder_label")
                            } else if method == ProjectMethod::Clone {
                                tr!("project.clone_parent_directory")
                            } else {
                                tr!("project.parent_directory")
                            }))
                            .child(if remote {
                                TextField::new("add-project-remote-path", remote_path)
                                    .w_full()
                                    .into_any_element()
                            } else {
                                folder_area.into_any_element()
                            }),
                    )
                    .when(method != ProjectMethod::Existing, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(label(if method == ProjectMethod::Empty {
                                    tr!("project.new_folder_name")
                                } else {
                                    tr!("project.target_folder")
                                }))
                                .child(
                                    TextField::new("add-project-directory-name", directory_name)
                                        .w_full(),
                                ),
                        )
                    })
                    .when(method == ProjectMethod::Existing, |element| {
                        element.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(label(tr!("project.name_label")))
                                .child(TextField::new("add-project-name", name).w_full()),
                        )
                    })
                    .when_some(target_preview, |element, path| {
                        element.child(
                            div()
                                .text_size(ui_px(12.0))
                                .text_color(theme.text_secondary)
                                .child(if method == ProjectMethod::Clone {
                                    tr!("project.clone_destination", path = path)
                                } else {
                                    tr!("project.create_destination", path = path)
                                }),
                        )
                    })
                    .children(error.map(|error| {
                        div()
                            .text_size(ui_px(12.0))
                            .text_color(theme.warning_text)
                            .child(error)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .mx(px(20.0))
                    .mt(px(8.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        Button::new("add-project-cancel", tr!("common.cancel"))
                            .disabled(pending && method != ProjectMethod::Existing)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let _ = cancel_weak
                                    .update(cx, |this, cx| this.close_add_project_dialog(cx));
                            }),
                    )
                    .child(
                        div()
                            .id("add-project-confirm")
                            .debug_selector(|| "add-project-submit".into())
                            .role(gpui::Role::Button)
                            .aria_label(confirm_label.clone())
                            .track_focus(&confirm_focus)
                            .when(can_confirm || pending || showing, |button| button.tab_index(0))
                            .h(px(24.0))
                            .px(px(8.0))
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme.accent_fill)
                            .text_color(theme.on_accent)
                            .text_size(ui_px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .when(!can_confirm && !pending && !showing, |button| button.opacity(0.45))
                            .when(can_confirm, |button| {
                                button.hover(|style| style.opacity(0.9))
                            })
                            .focus_visible(|style| {
                                style.border_1().border_color(theme.accent_focus)
                            })
                            .child(crate::ui::loading_label(confirm_label, cloning || showing, theme.on_accent))
                            .when(can_confirm, |button| {
                                button.on_activation(cx, |this, _, cx| {
                                    this.confirm_add_project(cx);
                                })
                            }),
                    ),
            );

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("add-project-dialog-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_add_project_dialog(cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(ADD_PROJECT_LAYER_PRIORITY).into_any_element())
    }
}
