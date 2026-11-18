//! Mid-turn steering inboxes shared between running turns and their callers.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use ag_protocol::TurnPrompt;
use tokio::sync::{Notify, oneshot};

use crate::app_server::AppServerError;

/// Steering inboxes of in-flight interactive turns, keyed by session id.
///
/// Transports register one inbox per running session turn; callers route
/// prompts to it by session id.
#[derive(Clone, Default)]
pub(crate) struct TurnSteeringRegistry {
    turns: Arc<Mutex<HashMap<String, Arc<TurnSteeringInbox>>>>,
}

impl TurnSteeringRegistry {
    /// Opens a steering inbox for one in-flight interactive turn.
    ///
    /// The returned guard closes the inbox and unregisters it on drop, so
    /// prompts submitted after the turn ends are rejected rather than lost.
    pub(crate) fn register(&self, session_id: &str) -> TurnSteeringRegistration {
        let inbox = Arc::new(TurnSteeringInbox::new());
        self.turns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session_id.to_string(), Arc::clone(&inbox));

        TurnSteeringRegistration {
            inbox,
            session_id: session_id.to_string(),
            turns: Arc::clone(&self.turns),
        }
    }

    /// Submits one prompt to the steering inbox of `session_id`'s running
    /// turn and returns the provider acknowledgement.
    ///
    /// # Errors
    /// Returns an error when no steerable turn is running.
    pub(crate) fn submit(
        &self,
        session_id: &str,
        prompt: TurnPrompt,
    ) -> Result<oneshot::Receiver<Result<(), AppServerError>>, AppServerError> {
        let inbox = self
            .turns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or_else(turn_not_steerable_error)?;

        inbox.submit(prompt)
    }
}

/// RAII guard for one turn's steering inbox registration.
pub(crate) struct TurnSteeringRegistration {
    inbox: Arc<TurnSteeringInbox>,
    session_id: String,
    turns: Arc<Mutex<HashMap<String, Arc<TurnSteeringInbox>>>>,
}

impl TurnSteeringRegistration {
    /// Returns the inbox drained by the running turn.
    pub(crate) fn inbox(&self) -> Arc<TurnSteeringInbox> {
        Arc::clone(&self.inbox)
    }
}

impl Drop for TurnSteeringRegistration {
    fn drop(&mut self) {
        let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        if turns
            .get(&self.session_id)
            .is_some_and(|inbox| Arc::ptr_eq(inbox, &self.inbox))
        {
            turns.remove(&self.session_id);
        }
        drop(turns);
        self.inbox.close();
    }
}

/// Awaits one steering acknowledgement; a turn that ends before answering
/// counts as a rejection.
pub(crate) async fn await_acknowledgement(
    acknowledgement: Result<oneshot::Receiver<Result<(), AppServerError>>, AppServerError>,
) -> Result<(), AppServerError> {
    acknowledgement?
        .await
        .unwrap_or_else(|_| Err(turn_not_steerable_error()))
}

/// Prompts one turn accepted, in delivery order.
type AcceptedSteers = Arc<Mutex<Vec<TurnPrompt>>>;

/// One prompt waiting for delivery into the running provider turn.
pub(crate) struct TurnSteer {
    /// User prompt to append to the running turn.
    pub(crate) prompt: TurnPrompt,
    /// Accepted-prompt log of the inbox the prompt came from.
    accepted: AcceptedSteers,
    /// Acknowledgement channel awaited by the steering caller.
    reply: oneshot::Sender<Result<(), AppServerError>>,
}

impl TurnSteer {
    /// Answers the steering caller. An accepted prompt is also logged in its
    /// inbox so a retried turn can send it again.
    pub(crate) fn acknowledge(self, result: Result<(), AppServerError>) {
        if result.is_ok() {
            self.accepted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(self.prompt);
        }
        let _ = self.reply.send(result);
    }
}

/// FIFO of steering prompts for one in-flight turn.
///
/// The running turn drains the inbox while it reads provider output. Closing
/// the inbox rejects prompts the turn never delivered, so callers can submit
/// them as a later turn instead.
pub(crate) struct TurnSteeringInbox {
    accepted: AcceptedSteers,
    notify: Notify,
    /// Undelivered prompts, or `None` once the turn stopped accepting them.
    pending: Mutex<Option<VecDeque<TurnSteer>>>,
}

impl TurnSteeringInbox {
    /// Creates an open inbox for one turn.
    pub(crate) fn new() -> Self {
        Self {
            accepted: Arc::default(),
            notify: Notify::new(),
            pending: Mutex::new(Some(VecDeque::new())),
        }
    }

    /// Queues one prompt for the running turn and returns its acknowledgement.
    ///
    /// # Errors
    /// Returns an error when the turn no longer accepts prompts.
    pub(crate) fn submit(
        &self,
        prompt: TurnPrompt,
    ) -> Result<oneshot::Receiver<Result<(), AppServerError>>, AppServerError> {
        let (reply, acknowledgement) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(pending) = pending.as_mut() else {
                return Err(turn_not_steerable_error());
            };
            pending.push_back(TurnSteer {
                accepted: Arc::clone(&self.accepted),
                prompt,
                reply,
            });
        }
        self.notify.notify_one();

        Ok(acknowledgement)
    }

    /// Waits for the next undelivered prompt. Never completes after closing.
    pub(crate) async fn next(&self) -> TurnSteer {
        loop {
            if let Some(steer) = self
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
                .and_then(VecDeque::pop_front)
            {
                return steer;
            }

            self.notify.notified().await;
        }
    }

    /// Returns `prompt` followed by every prompt the turn accepted so far,
    /// for a turn attempt that starts over.
    ///
    /// The caller already treats accepted prompts as delivered, so a retried
    /// attempt that sent only `prompt` would silently lose them.
    pub(crate) fn prompt_with_accepted(&self, prompt: &TurnPrompt) -> TurnPrompt {
        let mut prompt = prompt.clone();
        for accepted in self
            .accepted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            prompt.text.push_str("\n\n");
            prompt.text.push_str(&accepted.text);
            prompt
                .attachments
                .extend(accepted.attachments.iter().cloned());
        }

        prompt
    }

    /// Stops accepting prompts and rejects every undelivered one.
    pub(crate) fn close(&self) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_default();
        for steer in pending {
            let _ = steer.reply.send(Err(turn_not_steerable_error()));
        }
    }
}

/// Error returned when no running turn can receive a steering prompt.
pub(crate) fn turn_not_steerable_error() -> AppServerError {
    AppServerError::Provider("No running turn can receive this message".to_string())
}

#[cfg(test)]
#[path = "steering_test.rs"]
mod tests;
