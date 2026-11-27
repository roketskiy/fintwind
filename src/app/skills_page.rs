//! Fintwind host bridge for [`fintwind_integrations_ui::SkillsPage`].

use std::path::{Path, PathBuf};
use std::rc::Rc;

use fintwind_integrations_ui::SkillsHost;
use gpui::WeakEntity;

pub use fintwind_integrations_ui::skills::init;

use super::*;

struct FintwindSkillsHost(WeakEntity<Fintwind>);

pub(super) fn host(weak: WeakEntity<Fintwind>) -> Rc<dyn SkillsHost> {
    Rc::new(FintwindSkillsHost(weak))
}

impl SkillsHost for FintwindSkillsHost {
    fn is_active(&self, cx: &App) -> bool {
        self.0
            .read_with(cx, |this, _| {
                this.mode == WorkspaceMode::Settings && this.settings_page == SettingsPage::Skills
            })
            .unwrap_or(false)
    }

    fn ssh_host(&self, cx: &App) -> Option<Uuid> {
        self.0
            .read_with(cx, |this, _| this.settings_ssh_host())
            .ok()
            .flatten()
    }

    fn ssh_host_ready(&self, cx: &App) -> bool {
        self.0
            .read_with(cx, |this, _| this.settings_ssh_host_ready())
            .unwrap_or(true)
    }

    fn daemon_client(&self, host: Option<Uuid>, cx: &App) -> Option<fintwind_client::DaemonClient> {
        self.0
            .read_with(cx, |this, _| match host {
                Some(id) => this.ssh_client_for(id),
                None => Some(this.daemon.client()),
            })
            .ok()
            .flatten()
    }

    fn scan_projects(&self, cx: &App) -> Vec<(String, PathBuf)> {
        self.0
            .read_with(cx, |this, _| {
                this.state
                    .projects
                    .iter()
                    .filter(|project| !project.is_projectless())
                    .map(|project| (project.display_name(), project.path.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn open_host_path(&self, path: &Path, cx: &mut App) {
        let path = path.to_path_buf();
        let _ = self.0.update(cx, |this, cx| {
            this.open_host_path(&path, cx);
        });
    }

    fn reveal_host_path(&self, path: &Path, cx: &mut App) {
        let path = path.to_path_buf();
        let _ = self.0.update(cx, |this, cx| {
            this.reveal_host_path(&path, cx);
        });
    }

    fn show_toast(&self, message: String, success: bool, cx: &mut App) {
        let _ = self.0.update(cx, |this, _| {
            if success {
                this.show_success_toast(message);
            } else {
                this.show_toast(message);
            }
        });
    }

    fn update_card_visible(&self, cx: &App) -> bool {
        self.0
            .read_with(cx, |this, _| this.update_card_visible())
            .unwrap_or(false)
    }
}

impl Fintwind {
    pub(super) fn ensure_skills_catalog(&mut self, force: bool, cx: &mut Context<Self>) {
        let page = self.skills_page.clone();
        // The page consults its host; wait until the host update has finished.
        cx.defer(move |cx| page.update(cx, |page, cx| page.ensure_catalog(force, cx)));
    }

    pub(super) fn reset_skills_machine(&mut self, cx: &mut Context<Self>) {
        let page = self.skills_page.clone();
        cx.defer(move |cx| page.update(cx, |page, cx| page.reset_machine(cx)));
    }
}
