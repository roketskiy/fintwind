//! Fintwind host bridge for [intwind_scheduled_ui::ScheduledTasksView].

pub use fintwind_scheduled_ui::{ScheduledSessionChoice, ScheduledTasksEvent, ScheduledTasksView};

use super::*;

impl Fintwind {
    pub(super) fn open_scheduled_tasks(&mut self, cx: &mut Context<Self>) {
        self.mode = WorkspaceMode::ScheduledTasks;
        self.refresh_scheduled_task_catalog(cx);
        cx.notify();
    }

    pub(super) fn refresh_scheduled_task_catalog(&self, cx: &mut Context<Self>) {
        use fintwind_protocol::scheduled_task::SessionLaunch;
        let projects = self.state.projects.clone();
        let sessions = self
            .state
            .sessions
            .iter()
            .map(|session| ScheduledSessionChoice {
                id: session.id,
                title: session.display_title().to_owned(),
                project_id: session.project_id,
            })
            .collect();
        let launch = self.selected_session().and_then(|session| {
            session
                .model
                .clone()
                .or_else(|| self.state.last_model.clone())
                .map(|model| SessionLaunch {
                    model,
                    runtime_mode: session.runtime_mode,
                    interaction_mode: session.interaction_mode,
                    reasoning_effort: session.reasoning_effort.clone(),
                    service_tier: session.service_tier.clone(),
                    context_window: session.context_window.clone(),
                    agent_preset: session.agent_preset.clone(),
                })
        });
        self.scheduled_tasks_view.update(cx, |view, cx| {
            let models = self
                .probes
                .iter()
                .filter(|probe| probe.installed)
                .flat_map(|probe| probe.models.iter().cloned())
                .collect();
            view.set_catalog(projects, sessions, models, launch, cx);
        });
        cx.notify();
    }
}
