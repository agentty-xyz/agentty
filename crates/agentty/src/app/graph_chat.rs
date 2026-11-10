//! Loads the visible session's graph without changing normal chat admission.

use crate::app::App;
use crate::app::view::visible_review_session_id;
use crate::domain::session::SessionId;
use crate::presentation::graph_chat::GraphChatSnapshot;

impl App {
    /// Refreshes the bounded graph sidebar through repository boundaries.
    pub(crate) async fn refresh_graph_chat_if_needed(&mut self) -> bool {
        if !self.experimental_multi_agent {
            return false;
        }
        let Some(session_id) = visible_review_session_id(&self.mode).map(SessionId::from) else {
            return false;
        };
        let version = self.session_update_version(&session_id);
        let now = self.services.clock().now_instant();
        if !self.graph_chat.needs_refresh(&session_id, version, now) {
            return false;
        }
        let result = self.load_graph_chat(&session_id).await;

        self.graph_chat.complete(session_id, version, now, result)
    }

    async fn load_graph_chat(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<GraphChatSnapshot>, String> {
        let Some(proposal) = self
            .services
            .db()
            .graph_proposals()
            .latest(session_id)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let progress = if let Some(approval) = &proposal.version.approval {
            self.execution_graph_progress(&approval.graph_id)
                .await
                .map_err(|error| error.to_string())?
        } else {
            None
        };

        Ok(Some(GraphChatSnapshot {
            error: None,
            progress,
            proposal,
        }))
    }
}

#[cfg(test)]
#[path = "graph_chat_test.rs"]
mod tests;
