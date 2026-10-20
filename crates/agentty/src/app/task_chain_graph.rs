//! Foreground snapshot of the visible session's latest task-chain graph.

use ag_session::SessionMessageKind;

use crate::app::view::visible_review_session_id;
use crate::app::{App, AppEvent};
use crate::domain::session::SessionId;
use crate::presentation::task_chain_graph::TaskChainGraph;

/// Finished background graph load for one session update version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskChainGraphCompletion {
    /// Projection of the session's latest chain, or the load error.
    pub(crate) result: Result<Option<TaskChainGraph>, String>,
    /// Session whose chains were loaded.
    pub(crate) session_id: SessionId,
    /// Reduced update version observed when the load started.
    pub(crate) update_version: u64,
}

impl App {
    /// Reloads the visible session's latest task-chain graph after its
    /// reduced update version changes.
    ///
    /// Sessions whose transcript has no execution-graph marker resolve to no
    /// graph without querying persistence, so ordinary chats stay cheap.
    pub(crate) fn refresh_task_chain_graph_if_needed(&mut self) {
        let Some(session_id) = visible_review_session_id(&self.mode).map(SessionId::from) else {
            return;
        };
        let update_version = self.session_update_version(&session_id);
        if !self
            .task_chain_graphs
            .needs_load(&session_id, update_version)
        {
            return;
        }
        self.task_chain_graphs
            .begin_load(&session_id, update_version);
        if !self.session_has_task_chain_graph_marker(&session_id) {
            self.task_chain_graphs
                .complete_load(&session_id, update_version, Ok(None));

            return;
        }
        let services = self.services.clone();
        tokio::spawn(async move {
            let result = services
                .db()
                .tasks()
                .list_task_chains(&session_id)
                .await
                .map(|chains| chains.last().map(TaskChainGraph::from_chain))
                .map_err(|error| error.to_string());
            services.emit_app_event(AppEvent::TaskChainGraphLoaded {
                completion: TaskChainGraphCompletion {
                    result,
                    session_id,
                    update_version,
                },
            });
        });
    }

    /// Applies one finished graph load to the bounded snapshot cache.
    pub(crate) fn complete_task_chain_graph(&mut self, completion: TaskChainGraphCompletion) {
        self.task_chain_graphs.complete_load(
            &completion.session_id,
            completion.update_version,
            completion.result,
        );
    }

    fn session_has_task_chain_graph_marker(&self, session_id: &str) -> bool {
        self.sessions
            .session_for_id(session_id)
            .and_then(|session| session.transcript.as_ref())
            .is_some_and(|transcript| {
                transcript
                    .messages()
                    .iter()
                    .any(|message| message.kind == SessionMessageKind::AgentGraph)
            })
    }
}

#[cfg(test)]
#[path = "task_chain_graph_test.rs"]
mod tests;
