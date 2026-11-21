//! Delivery of queued chat prompts into a session's running turn.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use ag_contracts::AgentError;
use ag_telemetry::QueuedTrace;
use ag_worker::SessionRunClient;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::SessionTaskService;
use crate::app::AppEvent;
use crate::app::service::SessionUpdateVersionMap;
use crate::domain::session::{QueuedMessage, SessionId};
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::db::AppRepositories;

/// A prompt taken off the queue for delivery into the running turn.
///
/// It holds the prompt's queued-turn trace while the prompt is in flight,
/// because the worker ends the traces of prompts that left the queue.
pub(super) struct SteeredMessage {
    message: QueuedMessage,
    trace: Option<QueuedTrace>,
}

/// Background delivery of queued prompts through the worker's own runtime.
///
/// The foreground removes the prompts from the queue first, so the worker
/// cannot also start them as a follow-up turn. Each accepted prompt joins the
/// transcript as a user message. The first rejection returns it and every
/// later prompt to the queue in submission order, with their traces, and
/// wakes the worker, so they run after the current turn as before. When the
/// user stopped the turn, the rejected prompts are discarded instead, like
/// the rest of the queue.
///
/// The worker waits for the delivery before it selects more work, so the
/// prompts can only join the turn they were taken for.
pub(super) struct QueuedMessageSteering {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// The session's current-turn token, replaced when each turn starts.
    pub(super) cancel_token: Arc<Mutex<CancellationToken>>,
    pub(super) db: AppRepositories,
    /// Keeps the worker on the steered turn until this delivery settles.
    pub(super) delivery: SteeringDelivery,
    pub(super) message_traces: Arc<Mutex<HashMap<(SessionId, u64), QueuedTrace>>>,
    pub(super) queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    pub(super) session_id: SessionId,
    pub(super) session_run: SessionRunClient,
    pub(super) session_update_versions: SessionUpdateVersionMap,
    pub(super) transcript: Arc<Mutex<SessionTranscript>>,
    /// The session's turn token when the delivery was built. It belongs to
    /// the steered turn unless that turn had not installed its own yet.
    pub(super) turn_cancel_token: CancellationToken,
    /// Wakes the session worker after rejected prompts return to the queue.
    pub(super) wake_worker: Box<dyn Fn() + Send + Sync>,
}

impl QueuedMessageSteering {
    /// Takes the queued prompts to steer, in submission order up to the
    /// first one that must wait, such as a prompt with images.
    ///
    /// Their traces leave the shared map under the queue lock, so the worker
    /// can neither start the prompts nor end their traces meanwhile.
    pub(super) fn take_steerable_messages(&self) -> Vec<SteeredMessage> {
        let mut queued_messages = self
            .queued_messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let steerable_count = queued_messages
            .iter()
            .take_while(|message| message.is_steerable())
            .count();
        let mut message_traces = self
            .message_traces
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        queued_messages
            .drain(..steerable_count)
            .map(|message| {
                let trace = message_traces.remove(&(self.session_id.clone(), message.order()));

                SteeredMessage { message, trace }
            })
            .collect()
    }

    /// Steers `messages` in order, stopping at the first rejection.
    ///
    /// Waits for earlier deliveries of the same session. When one of them
    /// returned an older prompt to the queue, `messages` follow it there
    /// instead of overtaking it.
    pub(super) async fn run(self, messages: Vec<SteeredMessage>) {
        let _order = self.delivery.deliveries.order.lock().await;
        if messages
            .first()
            .is_some_and(|steered| self.has_older_queued_message(&steered.message))
        {
            self.restore(messages.into_iter());

            return;
        }

        let mut messages = messages.into_iter();
        while let Some(steered) = messages.next() {
            if let Err(error) = self
                .session_run
                .steer(steered.message.prompt().clone())
                .await
            {
                if self.restore(std::iter::once(steered).chain(messages)) {
                    self.report_rejection(&error).await;
                }

                return;
            }

            self.record_delivered(steered).await;
        }
    }

    /// Returns whether the queue holds a prompt submitted before `message`.
    fn has_older_queued_message(&self, message: &QueuedMessage) -> bool {
        self.queued_messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|queued| queued.order() < message.order())
    }

    /// Returns undelivered prompts to their submission-order positions with
    /// their traces and wakes the worker. Returns `false` after discarding
    /// them instead, which ends their traces, because the user stopped the
    /// steered turn and its queue is cleared.
    fn restore(&self, messages: impl Iterator<Item = SteeredMessage>) -> bool {
        {
            // The stop check shares the queue lock with the worker's
            // post-stop clear, so a restore cannot land after that clear.
            let mut queued_messages = self
                .queued_messages
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self.turn_cancel_token.is_cancelled() || self.is_current_turn_cancelled() {
                return false;
            }

            // Traces return under the queue lock, before the worker can
            // start the prompts or end the traces of unqueued ones.
            let mut message_traces = self
                .message_traces
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for SteeredMessage { message, trace } in messages {
                if let Some(trace) = trace {
                    message_traces.insert((self.session_id.clone(), message.order()), trace);
                }
                let index =
                    queued_messages.partition_point(|queued| queued.order() < message.order());
                queued_messages.insert(index, message);
            }
        }

        (self.wake_worker)();

        true
    }

    /// Returns whether the user stopped the session's current turn, whose
    /// token differs from the captured one when the steered turn installed
    /// its own after the delivery was built.
    fn is_current_turn_cancelled(&self) -> bool {
        self.cancel_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_cancelled()
    }

    /// Explains why the remaining prompts stay queued.
    async fn report_rejection(&self, error: &AgentError) {
        let notice = TranscriptNotice::QueueError.format(format!(
            "Could not send the queued message now: {error}. It stays queued for the next turn."
        ));

        SessionTaskService::append_workflow_notice(
            &self.transcript,
            &self.db,
            &self.app_event_tx,
            &self.session_update_versions,
            &self.session_id,
            &notice,
        )
        .await;
    }

    /// Ends the queued-turn trace and records the prompt in the transcript.
    async fn record_delivered(&self, steered: SteeredMessage) {
        let SteeredMessage { message, trace } = steered;
        drop(trace);
        SessionTaskService::append_user_prompt(
            &self.transcript,
            &self.db,
            &self.app_event_tx,
            &self.session_update_versions,
            &self.session_id,
            message.transcript_text(),
        )
        .await;
    }
}

/// One session's steering deliveries, shared by the foreground that starts
/// them and the worker that waits for them.
///
/// The worker settles every delivery after each turn, before it selects more
/// work. A prompt taken for one turn therefore never reaches a later turn,
/// and a rejected prompt returns to the queue before a newer one can start.
#[derive(Default)]
pub(super) struct SteeringDeliveries {
    in_flight: watch::Sender<usize>,
    /// Serializes deliveries so prompts reach the turn in submission order.
    order: tokio::sync::Mutex<()>,
}

impl SteeringDeliveries {
    /// Counts one delivery as in flight until the returned guard drops.
    ///
    /// Callers begin the delivery first and only then check that the worker
    /// still runs a steerable turn, so the worker ending that turn either
    /// sees this delivery or is seen by that check.
    pub(super) fn begin(self: &Arc<Self>) -> SteeringDelivery {
        self.in_flight.send_modify(|in_flight| *in_flight += 1);

        SteeringDelivery {
            deliveries: Arc::clone(self),
        }
    }

    /// Waits until no delivery is in flight.
    pub(super) async fn settled(&self) {
        let _ = self
            .in_flight
            .subscribe()
            .wait_for(|in_flight| *in_flight == 0)
            .await;
    }
}

/// Marks one steering delivery as in flight until dropped.
pub(super) struct SteeringDelivery {
    deliveries: Arc<SteeringDeliveries>,
}

impl Drop for SteeringDelivery {
    fn drop(&mut self) {
        self.deliveries
            .in_flight
            .send_modify(|in_flight| *in_flight -= 1);
    }
}

#[cfg(test)]
#[path = "steering_test.rs"]
mod tests;
