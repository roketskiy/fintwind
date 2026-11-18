use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_millis(300);
const MINIMUM_VISIBLE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum LoadingPhase {
    Done,
    Hidden,
    Visible,
}

pub(crate) struct LoadingDelay<K> {
    key: K,
    started: Instant,
    shown: Option<Instant>,
    wake_pending: Rc<Cell<bool>>,
}

pub(crate) struct LoadingWake {
    delay: Duration,
    pending: Rc<Cell<bool>>,
}

impl LoadingWake {
    pub(crate) fn delay(&self) -> Duration {
        self.delay
    }

    pub(crate) fn fired(self) {
        self.pending.set(false);
    }
}

impl<K: PartialEq> LoadingDelay<K> {
    pub(crate) fn is_visible(&self) -> bool {
        self.shown.is_some()
    }

    pub(crate) fn key(&self) -> &K {
        &self.key
    }

    pub(crate) fn observe(
        state: &mut Option<Self>,
        key: K,
        active: bool,
    ) -> (LoadingPhase, Option<LoadingWake>) {
        if state.as_ref().is_some_and(|wait| wait.key != key) {
            *state = None;
        }
        if state.is_none() && active {
            *state = Some(Self {
                key,
                started: Instant::now(),
                shown: None,
                wake_pending: Rc::new(Cell::new(true)),
            });
            let pending = state.as_ref().unwrap().wake_pending.clone();
            return (
                LoadingPhase::Hidden,
                Some(LoadingWake {
                    delay: DELAY,
                    pending,
                }),
            );
        }
        let Some(wait) = state.as_mut() else {
            return (LoadingPhase::Done, None);
        };
        if let Some(shown) = wait.shown {
            if active {
                return (LoadingPhase::Visible, None);
            }
            if let Some(remaining) = MINIMUM_VISIBLE.checked_sub(shown.elapsed()) {
                return (LoadingPhase::Visible, wait.schedule(remaining));
            }
        } else if active {
            if let Some(remaining) = DELAY.checked_sub(wait.started.elapsed()) {
                return (LoadingPhase::Hidden, wait.schedule(remaining));
            }
            wait.shown = Some(Instant::now());
            return (LoadingPhase::Visible, wait.schedule(MINIMUM_VISIBLE));
        }
        *state = None;
        (LoadingPhase::Done, None)
    }

    fn schedule(&self, delay: Duration) -> Option<LoadingWake> {
        if self.wake_pending.replace(true) {
            None
        } else {
            Some(LoadingWake {
                delay,
                pending: self.wake_pending.clone(),
            })
        }
    }
}
