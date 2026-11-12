//! Natural-language chat replies to a session's pending graph proposal.
//!
//! While the latest proposal is neither cancelled nor approved, every composer
//! message is a reply to it. The reply is shown in chat, a read-only utility
//! run on the Run Worker decides whether it approves, cancels, or revises the
//! exact inspected version, and the decided action is admitted through the
//! command gateway. Only one reply per proposal is decided at a time.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use ag_protocol::GraphProposalOutcome;
use ag_session::{
    GraphProposalAction, GraphProposalState, GraphProposalVersionOutcome, QuestionItem,
    SessionError,
};

use crate::app::App;
use crate::app::graph_node_reply::QUESTIONS_NOT_PENDING;
use crate::app::graph_proposal_decision::{GraphProposalReply, GraphProposalReplyDecision};
use crate::app::graph_question::GraphAnswerOutcome;
use crate::app::prompt_intent::PromptWorkflowOutcome;
use crate::app::session::SessionTaskService;
use crate::domain::session::SessionId;
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;

/// Run purpose recorded for the reply decision.
const DECISION_RUN_PURPOSE: &str = "graph proposal reply decision";

/// Rejection shown for a reply carrying attachments.
const ATTACHMENTS_UNSUPPORTED: &str =
    "Graph proposals accept text replies only. Remove the attachments and reply again.";

/// Rejection shown while the previous reply is still being decided.
const DECISION_PENDING: &str =
    "Still deciding on your previous reply to the graph proposal. Wait for it to finish.";

impl App {
    /// Routes one composer message to the session's pending graph proposal.
    ///
    /// Returns `None` when the session has no pending proposal, so the caller
    /// continues ordinary routing. Rejections keep the composer and its draft
    /// open; an accepted reply is decided in the background.
    pub(crate) async fn submit_graph_proposal_reply(
        &mut self,
        session_id: &SessionId,
        prompt: &TurnPrompt,
    ) -> Option<PromptWorkflowOutcome> {
        let state = match self.pending_graph_proposal(session_id).await {
            Ok(state) => state?,
            Err(error) => {
                return Some(self.reject_graph_proposal_reply(session_id, &error).await);
            }
        };

        Some(
            self.reply_to_graph_proposal(session_id, &state, prompt)
                .await,
        )
    }

    /// Returns the session's latest proposal while chat replies still act on
    /// it, or `None` when proposals are disabled or none is pending.
    ///
    /// # Errors
    /// Returns a displayable error when the proposal lookup fails.
    pub(crate) async fn pending_graph_proposal(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<GraphProposalState>, String> {
        if !self.experimental_multi_agent {
            return Ok(None);
        }

        self.services
            .db()
            .graph_proposals()
            .latest(session_id)
            .await
            .map(|state| state.filter(is_pending))
            .map_err(|error| format!("Graph proposal: {error}"))
    }

    /// Returns whether an earlier reply to the proposal is still being
    /// decided.
    pub(crate) fn is_deciding_graph_proposal_reply(&self, proposal_id: &str) -> bool {
        self.services
            .proposal_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(proposal_id)
    }

    /// Sends answers to the exact proposal version whose clarification
    /// questions the user answered, rejecting them as stale once that version
    /// no longer waits for answers.
    pub(crate) async fn answer_graph_proposal_questions(
        &mut self,
        session_id: &SessionId,
        proposal_id: &str,
        version: u32,
        prompt: &TurnPrompt,
    ) -> GraphAnswerOutcome {
        let state = match self.pending_graph_proposal(session_id).await {
            Ok(state) => state.filter(|state| {
                state.input.id == proposal_id
                    && state.version.number == version
                    && !clarification_questions(state).is_empty()
            }),
            Err(error) => {
                self.reject_graph_proposal_reply(session_id, &error).await;

                return GraphAnswerOutcome::Retry;
            }
        };
        let Some(state) = state else {
            self.reject_graph_proposal_reply(session_id, QUESTIONS_NOT_PENDING)
                .await;

            return GraphAnswerOutcome::Stale;
        };

        GraphAnswerOutcome::from_reply(
            &self
                .reply_to_graph_proposal(session_id, &state, prompt)
                .await,
        )
    }

    /// Echoes one reply in chat and decides it in the background.
    async fn reply_to_graph_proposal(
        &mut self,
        session_id: &SessionId,
        state: &GraphProposalState,
        prompt: &TurnPrompt,
    ) -> PromptWorkflowOutcome {
        if prompt.has_attachments() {
            return self
                .reject_graph_proposal_reply(session_id, ATTACHMENTS_UNSUPPORTED)
                .await;
        }
        let Some((folder, transcript)) = self
            .sessions
            .session_and_handles_or_err(session_id)
            .ok()
            .map(|(session, handles)| (session.folder.clone(), Arc::clone(&handles.transcript)))
        else {
            return PromptWorkflowOutcome::KeepPrompt;
        };
        if !self
            .services
            .proposal_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(state.input.id.clone())
        {
            return self
                .reject_graph_proposal_reply(session_id, DECISION_PENDING)
                .await;
        }
        SessionTaskService::append_user_prompt(
            &transcript,
            self.services.db(),
            &self.services.event_sender(),
            &self.services.session_update_versions(),
            session_id,
            &prompt.transcript_text(),
        )
        .await;
        self.spawn_graph_proposal_reply_decision(state, folder, prompt.text.clone(), transcript);

        PromptWorkflowOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    }

    /// Reports a rejected reply and keeps the composer open.
    async fn reject_graph_proposal_reply(
        &self,
        session_id: &SessionId,
        message: &str,
    ) -> PromptWorkflowOutcome {
        self.append_prompt_status_line(session_id, TranscriptNotice::ReplyError, message)
            .await;

        PromptWorkflowOutcome::KeepPrompt
    }

    /// Decides with the proposal's captured selection and session ownership,
    /// then admits the action for the exact inspected version. A version that
    /// changed meanwhile rejects the action instead of retargeting it.
    fn spawn_graph_proposal_reply_decision(
        &self,
        state: &GraphProposalState,
        folder: PathBuf,
        reply: String,
        transcript: Arc<Mutex<SessionTranscript>>,
    ) {
        let decision = GraphProposalReplyDecision::new(state, folder, reply.clone());
        let run_client = ag_worker::scoped_client(
            self.services.run_client(),
            ag_worker::RunScope {
                purpose: Some(DECISION_RUN_PURPOSE.to_string()),
                session_id: Some(state.input.session_id.to_string()),
                ..ag_worker::RunScope::default()
            },
        );
        let handle = self.sessions.handle();
        let services = self.services.clone();
        let id = state.input.id.clone();
        let session_id = state.input.session_id.clone();
        let version = state.version.number;
        tokio::spawn(async move {
            let (action, success) = match decision.decide(run_client.as_ref()).await {
                GraphProposalReply::Approve => (
                    GraphProposalAction::Approve,
                    format!("Graph proposal v{version} approved · execution started"),
                ),
                GraphProposalReply::Cancel => (
                    GraphProposalAction::Cancel,
                    format!("Graph proposal v{version} cancelled."),
                ),
                GraphProposalReply::Revise => (
                    GraphProposalAction::Revise(reply),
                    format!("Generating graph proposal v{}…", version + 1),
                ),
            };
            let result = handle.act_on_graph_proposal(&id, version, action).await;
            services
                .proposal_reply_decisions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            let notice = result.map_or_else(|error| reply_failure(&error), |()| success);
            SessionTaskService::append_workflow_notice(
                &transcript,
                services.db(),
                &services.event_sender(),
                &services.session_update_versions(),
                &session_id,
                &notice,
            )
            .await;
        });
    }
}

/// Returns whether chat replies still act on the proposal.
fn is_pending(state: &GraphProposalState) -> bool {
    !state.version.is_cancelled && state.version.approval.is_none()
}

/// Returns the clarification questions a generated proposal version asks,
/// or none when it generated a graph, failed, or is still generating.
pub(crate) fn clarification_questions(state: &GraphProposalState) -> &[QuestionItem] {
    match &state.version.outcome {
        GraphProposalVersionOutcome::Generated { response, .. } => match &response.outcome {
            GraphProposalOutcome::Clarification { questions } => questions,
            GraphProposalOutcome::Proposal { .. } => &[],
        },
        GraphProposalVersionOutcome::Failed(_) | GraphProposalVersionOutcome::Generating => &[],
    }
}

/// Describes a rejected decided action.
fn reply_failure(error: &SessionError) -> String {
    TranscriptNotice::ReplyError.format(format!("Graph proposal: {error}"))
}

#[cfg(test)]
#[path = "graph_proposal_reply_test.rs"]
pub(crate) mod tests;
