//! Routes one chat message to the graph work that waits for it.
//!
//! Terminal composer messages and session API messages, including those from
//! remote control, share this routing, so a reply reaches the same graph work
//! whichever caller sent it. A message that no graph work waits for continues
//! through ordinary message admission.

use ag_session::SessionError;

use crate::app::App;
use crate::domain::session::SessionId;
use crate::domain::turn_prompt::TurnPrompt;

/// Caller-neutral result of a chat message routed to waiting graph work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GraphReplyOutcome {
    /// The reply was recorded or admitted; its decision or continuation runs
    /// in the background.
    Accepted,
    /// The reply was not recorded. Any notice is already in the session
    /// transcript, so callers only report the error.
    Rejected(SessionError),
}

impl GraphReplyOutcome {
    /// Converts the outcome into an admission acknowledgement.
    ///
    /// # Errors
    /// Returns the rejection error when the reply was not recorded.
    pub(crate) fn into_result(self) -> Result<(), SessionError> {
        match self {
            Self::Accepted => Ok(()),
            Self::Rejected(error) => Err(error),
        }
    }
}

impl App {
    /// Routes one chat message to the session's waiting graph work.
    ///
    /// A pending graph proposal receives the message first, then the graph
    /// node whose question was asked last, then a graph output awaiting
    /// acceptance. Returns `None` when no graph work waits, so the caller
    /// continues ordinary routing.
    pub(crate) async fn submit_graph_reply(
        &mut self,
        session_id: &SessionId,
        prompt: &TurnPrompt,
    ) -> Option<GraphReplyOutcome> {
        if let Some(outcome) = self.submit_graph_proposal_reply(session_id, prompt).await {
            return Some(outcome);
        }
        if let Some(outcome) = self.submit_graph_node_reply(session_id, prompt).await {
            return Some(outcome);
        }

        self.submit_graph_output_reply(session_id, prompt).await
    }
}
