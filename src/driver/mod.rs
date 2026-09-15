//! Desktop proxy for the provider runtime owned by `fintwind-daemon`.

use std::sync::Arc;

use crate::model::{BackgroundWorkKey, DriverEvent, ProviderResumeCursor, RuntimeEventCursor};

pub use fintwind_client::driver::{
    DriverControl, DriverEventSender, DriverHandle, DriverStartOptions, SessionOptions,
    event_channel,
};

pub(crate) fn start_remote(
    client: fintwind_client::DaemonClient,
    session_id: uuid::Uuid,
    options: DriverStartOptions,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    let runtime_id = uuid::Uuid::new_v4();
    let command = fintwind_client::Command::Start {
        options: fintwind_client::WireDriverStartOptions {
            binary: options.binary,
            cwd: options.cwd,
            mode: fintwind_client::encode_enum(options.mode)?,
            interaction_mode: fintwind_client::encode_enum(options.interaction_mode)?,
            model: options.model,
            reasoning_effort: options.reasoning_effort,
            service_tier: options.service_tier,
            context_window: options.context_window,
            agent_preset: options.agent_preset,
            provider_cursor: options
                .provider_cursor
                .map(serde_json::to_value)
                .transpose()?,
        },
    };
    let supports_steer = match client.request(session_id, runtime_id, command) {
        Ok(fintwind_client::ResponsePayload::Started { supports_steer }) => supports_steer,
        Ok(_) => anyhow::bail!("fintwind daemon returned an invalid start response"),
        Err(error) => return Err(error),
    };
    connect_remote(client, session_id, runtime_id, supports_steer, None, events)
}

pub(crate) fn attach_remote(
    client: fintwind_client::DaemonClient,
    session_id: uuid::Uuid,
    runtime_id: uuid::Uuid,
    supports_steer: bool,
    replay_cursor: Option<RuntimeEventCursor>,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    connect_remote(
        client,
        session_id,
        runtime_id,
        supports_steer,
        replay_cursor,
        events,
    )
}

fn connect_remote(
    client: fintwind_client::DaemonClient,
    session_id: uuid::Uuid,
    runtime_id: uuid::Uuid,
    supports_steer: bool,
    replay_cursor: Option<RuntimeEventCursor>,
    events: DriverEventSender,
) -> anyhow::Result<DriverHandle> {
    let remote_events = client.subscribe(session_id, runtime_id);
    let forwarding_events = events.clone();
    let thread_client = client.clone();
    let spawn = std::thread::Builder::new()
        .name(format!("fintwind-daemon-session-{session_id}"))
        .spawn(move || {
            let mut saw_process_exit = false;
            while let Ok(sequenced) = remote_events.recv() {
                if replay_cursor.is_some_and(|cursor| {
                    cursor.runtime_id == sequenced.runtime_id
                        && cursor.epoch == sequenced.epoch
                        && cursor.sequence >= sequenced.sequence
                }) {
                    continue;
                }
                let cursor = RuntimeEventCursor {
                    runtime_id: sequenced.runtime_id,
                    epoch: sequenced.epoch,
                    sequence: sequenced.sequence,
                };
                let event = match fintwind_client::event_from_wire(sequenced.event) {
                    Ok(event) => event,
                    Err(error) => DriverEvent::Error(format!(
                        "fintwind daemon sent an invalid event: {error}"
                    )),
                };
                saw_process_exit |= matches!(&event, DriverEvent::ProcessExited);
                if forwarding_events.send(event).is_err()
                    || forwarding_events
                        .send(DriverEvent::RuntimeEventCursorAdvanced(cursor))
                        .is_err()
                {
                    break;
                }
            }
            thread_client.unsubscribe(session_id, runtime_id);
            // A development hot-swap (or daemon crash) closes the old event
            // channel. Surface that as an ordinary provider exit so the task
            // cannot remain stuck in Working while the supervisor connects
            // subsequent work to the replacement daemon.
            if !saw_process_exit {
                let _ = forwarding_events.send(DriverEvent::ProcessExited);
            }
        });
    if let Err(error) = spawn {
        client.unsubscribe(session_id, runtime_id);
        return Err(error.into());
    }
    Ok(DriverHandle::from_control(Arc::new(RemoteDriverControl {
        client,
        session_id,
        runtime_id,
        supports_steer,
        events,
    })))
}

struct RemoteDriverControl {
    client: fintwind_client::DaemonClient,
    session_id: uuid::Uuid,
    runtime_id: uuid::Uuid,
    supports_steer: bool,
    events: DriverEventSender,
}

impl RemoteDriverControl {
    fn notify(&self, command: fintwind_client::Command) {
        if let Err(error) = self
            .client
            .notify(self.session_id, self.runtime_id, command)
        {
            let _ = self.events.send(DriverEvent::Error(format!(
                "fintwind daemon command failed: {error}"
            )));
        }
    }
}

impl DriverControl for RemoteDriverControl {
    fn prompt(&self, prompt: String) {
        self.notify(fintwind_client::Command::Prompt { prompt });
    }

    fn supports_steer(&self) -> bool {
        self.supports_steer
    }

    fn steer(&self, prompt: String) {
        self.notify(fintwind_client::Command::Steer { prompt });
    }

    fn compact(&self) {
        self.notify(fintwind_client::Command::CompactSession);
    }

    fn cancel(&self) {
        self.notify(fintwind_client::Command::Cancel);
    }

    fn refresh_background_work(&self) {
        self.notify(fintwind_client::Command::RefreshBackgroundWork);
    }

    fn stop_background_work(&self, key: BackgroundWorkKey, control_id: String) {
        match serde_json::to_value(key) {
            Ok(key) => {
                self.notify(fintwind_client::Command::StopBackgroundWork { key, control_id })
            }
            Err(error) => {
                let _ = self.events.send(DriverEvent::Error(format!(
                    "could not encode background-work command: {error}"
                )));
            }
        }
    }

    fn respond(&self, request_id: String, option_id: String) {
        self.notify(fintwind_client::Command::Respond {
            request_id,
            option_id,
        });
    }

    fn respond_user_input(
        &self,
        request_id: String,
        answers: Vec<fintwind_protocol::model::UserInputAnswer>,
    ) {
        self.notify(fintwind_client::Command::RespondUserInput {
            request_id,
            answers,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        let options = (|| {
            Ok::<_, anyhow::Error>(fintwind_client::WireSessionOptions {
                mode: fintwind_client::encode_enum(options.mode)?,
                interaction_mode: fintwind_client::encode_enum(options.interaction_mode)?,
                model: options.model,
                reasoning_effort: options.reasoning_effort,
                service_tier: options.service_tier,
                context_window: options.context_window,
            })
        })();
        let Ok(options) = options else {
            return false;
        };
        matches!(
            self.client.request(
                self.session_id,
                self.runtime_id,
                fintwind_client::Command::ApplyOptions { options }
            ),
            Ok(fintwind_client::ResponsePayload::OptionsApplied { applied: true })
        )
    }

    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        match self.client.request(
            self.session_id,
            self.runtime_id,
            fintwind_client::Command::Rollback { turns },
        )? {
            fintwind_client::ResponsePayload::Cursor { cursor } => cursor
                .map(serde_json::from_value)
                .transpose()
                .map_err(Into::into),
            _ => anyhow::bail!("fintwind daemon returned an invalid rollback response"),
        }
    }

    fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        match self.client.request(
            self.session_id,
            self.runtime_id,
            fintwind_client::Command::Fork { turns_to_remove },
        )? {
            fintwind_client::ResponsePayload::Cursor {
                cursor: Some(cursor),
            } => serde_json::from_value(cursor).map_err(Into::into),
            _ => anyhow::bail!("fintwind daemon returned an invalid fork response"),
        }
    }

    fn close(&self) {
        self.notify(fintwind_client::Command::CloseSession);
    }
}

impl Drop for RemoteDriverControl {
    fn drop(&mut self) {
        self.client.unsubscribe(self.session_id, self.runtime_id);
    }
}
