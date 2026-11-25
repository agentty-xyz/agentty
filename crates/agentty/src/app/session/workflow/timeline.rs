//! One running turn's live timeline, reduced by the turn producer and
//! grouped by the foreground at most once per frame.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_contracts::{ActivityEvent, OutputEvent};
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::domain::session::SessionId;
use crate::domain::turn_timeline::{TimelineSnapshot, TurnTimeline};

/// A replaceable live-timeline mailbox shared by a turn producer and the
/// foreground. Equality identifies the mailbox, independent of its changing
/// contents.
///
/// The producer queues one [`AppEvent::SessionTurnReady`] notification while
/// none is armed. The foreground keeps the notification armed after a
/// [`Self::snapshot`] and applies later changes through [`Self::refresh`]
/// once per frame, so a fast stream redraws at most once per frame instead
/// of waking the event loop for every fragment.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveTurn {
    state: Arc<Mutex<LiveTurnState>>,
}

impl LiveTurn {
    /// Returns the grouped timeline, which is empty before the turn shows
    /// anything, and keeps the notification armed until a
    /// [`Self::refresh`] finds nothing new.
    pub(crate) fn snapshot(&self) -> Arc<TimelineSnapshot> {
        let mut state = self.lock();
        state.changed = false;

        Arc::new(state.timeline.snapshot())
    }

    /// Returns the grouped timeline when it changed since the last
    /// snapshot. Otherwise releases the notification, so the next change
    /// queues a new one, and returns `None`.
    pub(crate) fn refresh(&self) -> Option<Arc<TimelineSnapshot>> {
        let mut state = self.lock();
        if !state.changed {
            state.pending = false;

            return None;
        }
        state.changed = false;

        Some(Arc::new(state.timeline.snapshot()))
    }

    /// Applies one streamed output change observed at `now` Unix
    /// milliseconds.
    pub(super) fn observe_output(
        &self,
        event: OutputEvent,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        let mut state = self.lock();
        if state.timeline.observe_output(event, now) {
            self.mark_changed(&mut state, sender, session_id);
        }
    }

    /// Applies one activity snapshot observed at `now` Unix milliseconds.
    pub(super) fn observe_activity(
        &self,
        event: ActivityEvent,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        let mut state = self.lock();
        if state.timeline.observe_activity(event, now) {
            self.mark_changed(&mut state, sender, session_id);
        }
    }

    /// Records progress the turn reported outside its output, which names
    /// the current action when nothing more specific runs.
    pub(super) fn observe_progress(
        &self,
        text: String,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        let mut state = self.lock();
        if state.timeline.observe_progress(text, now) && !state.timeline.is_empty() {
            self.mark_changed(&mut state, sender, session_id);
        }
    }

    /// Settles calls that never reported a result once the turn's events
    /// end.
    pub(super) fn interrupt_running(
        &self,
        now: u64,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        let mut state = self.lock();
        if state.timeline.interrupt_running(now) {
            self.mark_changed(&mut state, sender, session_id);
        }
    }

    /// Returns the current action as one line, such as `Running cargo
    /// test`, or `None` before the turn reports one.
    pub(super) fn current_action_text(&self) -> Option<String> {
        let state = self.lock();
        if state.timeline.is_empty() {
            return None;
        }

        state.timeline.current_action().map(|current| current.text)
    }

    /// Returns the steps the turn narrated or reasoned in, or `None` when it
    /// did neither.
    pub(super) fn narrated_steps(&self) -> Option<usize> {
        self.lock().timeline.narrated_steps()
    }

    /// Removes the timeline from the session view once the turn's saved
    /// result replaces it; later calls, or a turn that showed nothing, send
    /// nothing.
    pub(super) fn clear(&self, sender: &mpsc::UnboundedSender<AppEvent>, session_id: &SessionId) {
        if std::mem::take(&mut self.lock().shown) {
            let _ = sender.send(AppEvent::SessionTurnCleared {
                session_id: session_id.clone(),
            });
        }
    }

    /// Records a change and queues a notification when none is armed.
    fn mark_changed(
        &self,
        state: &mut LiveTurnState,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        state.changed = true;
        state.shown = true;
        if !state.pending {
            state.pending = sender
                .send(AppEvent::SessionTurnReady {
                    session_id: session_id.clone(),
                    turn: self.clone(),
                })
                .is_ok();
        }
    }

    /// Locks the shared state; it stays valid after a panic.
    fn lock(&self) -> MutexGuard<'_, LiveTurnState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PartialEq for LiveTurn {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for LiveTurn {}

/// The reduced timeline with its notification state.
#[derive(Debug, Default)]
struct LiveTurnState {
    /// Whether the timeline changed since the foreground last grouped it.
    changed: bool,
    /// Whether a notification is queued or its handle is still armed.
    pending: bool,
    /// Whether the session view may show the timeline.
    shown: bool,
    timeline: TurnTimeline,
}

#[cfg(test)]
#[path = "timeline_test.rs"]
mod tests;
