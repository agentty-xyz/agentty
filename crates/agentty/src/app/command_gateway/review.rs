//! Review publication admission and worker acknowledgement.

use std::sync::Arc;

use ag_session::{ReviewRequest, SessionError, SessionId};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::app::App;
use crate::app::branch_publish::{branch_publish_loading_label, review_request_queued_label};
use crate::domain::session::PublishBranchAction;
use crate::domain::transient_message::TransientMessageBody;

/// Publication inputs and durable worker operation identity. The enclosing
/// session command carries the separate admission-attempt identity.
pub(crate) struct ReviewRequestCommand {
    pub(super) operation_id: String,
    pub(super) session_id: SessionId,
    source: ReviewRequestSource,
}

impl ReviewRequestCommand {
    /// Captures a terminal request that returns after admission.
    pub(crate) fn from_session_view(
        session_id: SessionId,
        remote_branch_name: Option<String>,
    ) -> Self {
        Self {
            operation_id: Uuid::new_v4().to_string(),
            session_id,
            source: ReviewRequestSource::SessionView { remote_branch_name },
        }
    }

    /// Captures an API request whose response is owned by the worker until
    /// publication completes, fails, or is canceled.
    pub(crate) fn from_session_api(
        session_id: SessionId,
        response_tx: oneshot::Sender<Result<ReviewRequest, SessionError>>,
    ) -> Self {
        Self {
            operation_id: Uuid::new_v4().to_string(),
            session_id,
            source: ReviewRequestSource::SessionApi { response_tx },
        }
    }

    /// Supplies provenance for direct foreground publication calls.
    pub(super) fn initiator(&self) -> super::CommandInitiator {
        match self.source {
            ReviewRequestSource::SessionView { .. } => super::CommandInitiator::SessionView,
            ReviewRequestSource::SessionApi { .. } => super::CommandInitiator::SessionApi,
        }
    }
}

/// Delivery and acknowledgement policy, independent of mailbox provenance.
enum ReviewRequestSource {
    SessionView {
        remote_branch_name: Option<String>,
    },
    SessionApi {
        response_tx: oneshot::Sender<Result<ReviewRequest, SessionError>>,
    },
}

impl App {
    /// Admits publication through the same lookup, branch reservation,
    /// persistence, and worker queue for both entry points.
    ///
    /// The terminal calls directly on the foreground task; API commands arrive
    /// through the bounded mailbox. Neither path waits for a busy branch here.
    /// Status eligibility remains worker-owned and is checked at execution,
    /// after any earlier turn and its post-processing have completed.
    ///
    /// # Errors
    /// Returns `NotFound` when the session or its handles are unavailable,
    /// allowing the terminal to restore its existing missing-session popup.
    /// Queue failures return an operation error and retain the inline notice
    /// and API worker response.
    pub(super) async fn execute_review_request(
        &mut self,
        command: ReviewRequestCommand,
    ) -> Result<(), SessionError> {
        let ReviewRequestCommand {
            operation_id,
            session_id,
            source,
        } = command;
        let (remote_branch_name, response_tx) = match source {
            ReviewRequestSource::SessionView { remote_branch_name } => (remote_branch_name, None),
            ReviewRequestSource::SessionApi { response_tx } => (None, Some(response_tx)),
        };
        let Some(context) = self.branch_publish_task_context(&session_id) else {
            if let Some(response_tx) = response_tx {
                let _ = response_tx.send(Err(SessionError::NotFound));
            }

            return Err(SessionError::NotFound);
        };
        // Reserve an idle branch before persistence. An existing owner already
        // serializes worker execution, so the foreground never waits here.
        let branch_operation_lock = Arc::clone(&context.branch_operation_lock);
        let _branch_operation_guard = branch_operation_lock.try_lock_owned().ok();
        let enqueue_result = self
            .sessions
            .enqueue_review_request_creation(
                &self.services,
                context.session,
                operation_id,
                remote_branch_name,
                response_tx,
            )
            .await;
        match enqueue_result {
            Err(error) => {
                let _ = self.sessions.finish_branch_publish(
                    &session_id,
                    TransientMessageBody::Markdown(format!(
                        "**Review request publish failed**\n\n{error}"
                    )),
                );

                return Err(SessionError::Operation(error.to_string()));
            }
            Ok(Some(queued_order)) => self.sessions.queue_branch_publish(
                &session_id,
                queued_order,
                review_request_queued_label(),
            ),
            Ok(None) => self.sessions.start_branch_publish(
                &session_id,
                branch_publish_loading_label(PublishBranchAction::PublishPullRequest),
            ),
        }

        Ok(())
    }
}
