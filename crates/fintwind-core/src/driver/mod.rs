//! Local provider runtime owned by `fintwind-daemon`.

mod activity;
pub(crate) mod native;
mod opencode;
mod support;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, SendError, Sender, unbounded};

use fintwind_protocol::PromptFile;

use crate::model::{
    BackgroundWorkKey, DriverEvent, InteractionMode, ProviderResumeCursor, RuntimeMode,
    UserInputAnswer,
};

/// Provider events remain synchronous to send from reader threads, while the
/// bounded wake channel lets the UI sleep until at least one event is ready.
/// Multiple provider writes coalesce into one wake without ever blocking the
/// provider or dropping the events themselves.
#[derive(Clone)]
pub struct DriverEventSender {
    events: Sender<DriverEvent>,
    wake: smol::channel::Sender<()>,
}

impl DriverEventSender {
    pub fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>> {
        self.events.send(event)?;
        let _ = self.wake.try_send(());
        Ok(())
    }
}

pub(crate) trait DriverEventSink {
    fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>>;
}

impl DriverEventSink for DriverEventSender {
    fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>> {
        DriverEventSender::send(self, event)
    }
}

#[cfg(test)]
impl DriverEventSink for Sender<DriverEvent> {
    fn send(&self, event: DriverEvent) -> Result<(), SendError<DriverEvent>> {
        Sender::send(self, event)
    }
}

pub fn event_channel(
    wake: smol::channel::Sender<()>,
) -> (DriverEventSender, Receiver<DriverEvent>) {
    let (events, receiver) = unbounded();
    (DriverEventSender { events, wake }, receiver)
}

#[cfg(test)]
pub(crate) fn test_event_channel() -> (DriverEventSender, Receiver<DriverEvent>) {
    let (wake, _wakes) = smol::channel::bounded(1);
    event_channel(wake)
}

#[derive(Clone)]
pub struct DriverHandle {
    inner: Arc<dyn DriverControl>,
}

impl DriverHandle {
    pub fn from_control(control: Arc<dyn DriverControl>) -> Self {
        Self { inner: control }
    }

    pub fn prompt(&self, prompt: String, files: Vec<PromptFile>) {
        self.inner.prompt(prompt, files);
    }

    /// Whether this transport can inject a user message into the currently
    /// running turn (steering) instead of starting a new one.
    pub fn supports_steer(&self) -> bool {
        self.inner.supports_steer()
    }

    pub fn steer(&self, prompt: String, files: Vec<PromptFile>) {
        self.inner.steer(prompt, files);
    }

    pub fn compact(&self) {
        self.inner.compact();
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn refresh_background_work(&self) {
        self.inner.refresh_background_work();
    }

    pub fn stop_background_work(&self, key: BackgroundWorkKey, control_id: String) {
        self.inner.stop_background_work(key, control_id);
    }

    pub fn respond(&self, request_id: String, option_id: String) {
        self.inner.respond(request_id, option_id);
    }

    pub fn respond_user_input(&self, request_id: String, answers: Vec<UserInputAnswer>) {
        self.inner.respond_user_input(request_id, answers);
    }

    pub fn apply_options(&self, options: SessionOptions) -> bool {
        self.inner.apply_options(options)
    }

    pub fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        self.inner.fork(turns_to_remove)
    }
}

pub trait DriverControl: Send + Sync {
    fn prompt(&self, prompt: String, files: Vec<PromptFile>);
    fn supports_steer(&self) -> bool {
        false
    }
    /// Deliver a steering message to the running turn. Implementations report
    /// the outcome asynchronously through `DriverEvent::SteerAccepted` or
    /// `DriverEvent::SteerRejected`.
    fn steer(&self, _prompt: String, _files: Vec<PromptFile>) {}
    /// Ask the provider to compact this session's context. The provider
    /// admits the request durably — it runs at the next safe step boundary,
    /// or immediately when idle — and reports every outcome asynchronously
    /// through `DriverEvent::CompactionUpdated`.
    fn compact(&self) {}
    fn cancel(&self);
    fn refresh_background_work(&self) {}
    fn stop_background_work(&self, _key: BackgroundWorkKey, _control_id: String) {}
    fn respond(&self, request_id: String, option_id: String);
    fn respond_user_input(&self, _request_id: String, _answers: Vec<UserInputAnswer>) {}
    /// Applies changed turn options to the live session, returning whether the
    /// transport could do it without being restarted. A `false` answer is the
    /// driver asking to be torn down and recreated with the new options.
    fn apply_options(&self, _options: SessionOptions) -> bool {
        false
    }
    fn fork(&self, _turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        anyhow::bail!("conversation forking is not supported by this provider transport")
    }
}

pub struct DriverStartOptions {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub context_window: Option<String>,
    pub agent_preset: Option<String>,
    pub provider_cursor: Option<ProviderResumeCursor>,
    /// The app-side task (session) UUID this runtime serves. It rides the
    /// OpenCode session's `metadata` so a native session can be traced back
    /// to the task that created it, from this app or any other client.
    pub task_id: Option<String>,
}

/// The subset of `DriverStartOptions` a user can change without starting a new
/// task. Transports that carry these per turn can absorb a change in place;
/// the rest have to be restarted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOptions {
    pub mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub context_window: Option<String>,
}

pub(crate) fn start_local(
    options: DriverStartOptions,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    // OpenCode's own server is its real API, and it is what exposes
    // interactive permission requests.
    let inner: Arc<dyn DriverControl> = Arc::new(opencode::OpenCodeDriver::start(options, events)?);
    Ok(DriverHandle { inner })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_events_coalesce_wakes_without_dropping_payloads() {
        let (wake, wakes) = smol::channel::bounded(1);
        let (events, received) = event_channel(wake);

        events
            .send(DriverEvent::TextDelta {
                part: String::new(),
                delta: "one".into(),
            })
            .unwrap();
        events
            .send(DriverEvent::TextDelta {
                part: String::new(),
                delta: "two".into(),
            })
            .unwrap();

        assert_eq!(wakes.try_recv(), Ok(()));
        assert!(matches!(
            wakes.try_recv(),
            Err(smol::channel::TryRecvError::Empty)
        ));
        assert!(
            matches!(received.try_recv(), Ok(DriverEvent::TextDelta { delta, .. }) if delta == "one")
        );
        assert!(
            matches!(received.try_recv(), Ok(DriverEvent::TextDelta { delta, .. }) if delta == "two")
        );
    }
}
