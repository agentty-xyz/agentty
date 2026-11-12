//! Natural-language chat replies to a session's pending graph proposal.
//!
//! While the latest proposal is neither cancelled nor approved, every composer
//! message is a reply to it. The reply is recorded once with its visible
//! prompt, bound to the exact inspected version. A read-only utility run on
//! the Run Worker decides whether it approves, cancels, or revises that
//! version; the first decision is durable, and its action is admitted through
//! the command gateway. Recovery after a restart or a project switch decides
//! or applies every reply whose action was not attempted, including one whose
//! session was unloaded, or whose session runtime stopped, before the action
//! was answered. Only one reply per proposal is decided at a time. Pasted
//! images travel with a revision into every later generation; only the reply
//! text is recorded, so a recovered reply acts without its images.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ag_protocol::{GraphProposalOutcome, TurnPromptAttachment};
use ag_session::{
    GraphProposalAction, GraphProposalReplyOutcome, GraphProposalState,
    GraphProposalVersionOutcome, QuestionItem, SessionError, SessionMessageKind,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::app::graph_node_reply::QUESTIONS_NOT_PENDING;
use crate::app::graph_proposal_decision::GraphProposalReplyDecision;
use crate::app::graph_question::GraphAnswerOutcome;
use crate::app::prompt_intent::PromptWorkflowOutcome;
use crate::app::session::SessionTaskService;
use crate::app::{App, AppServices, SessionRuntimeHandle};
use crate::domain::session::SessionId;
use crate::domain::session_message::SessionTranscript;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::GraphProposalReplyRecord;

/// Run purpose recorded for the reply decision.
const DECISION_RUN_PURPOSE: &str = "graph proposal reply decision";

/// Rejection shown while the previous reply is still being decided.
const DECISION_PENDING: &str =
    "Still deciding on your previous reply to the graph proposal. Wait for it to finish.";

/// Notice shown when stopping the session abandons an undecided reply.
const DECISION_CANCELED: &str =
    "Graph proposal: the reply decision was canceled. Reply again to act on the proposal.";

/// A background decision, cancelled with its session or at shutdown.
pub(super) struct ProposalReplyDecision {
    cancellation: CancellationToken,
    is_abandoned: Arc<AtomicBool>,
    session_id: SessionId,
}

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
        if !self.experimental_graph {
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
            .contains_key(proposal_id)
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

    /// Decides or applies the unsettled replies of loaded sessions after a
    /// restart or a project switch, at most one per proposal. A decided reply
    /// reapplies its recorded decision instead of deciding again.
    pub(crate) async fn recover_graph_proposal_replies(&mut self) {
        if !self.experimental_graph {
            return;
        }
        let session_ids: Vec<SessionId> = self
            .sessions
            .sessions()
            .iter()
            .map(|session| session.id.clone())
            .collect();
        for session_id in session_ids {
            // A failed read leaves the replies unsettled for the next start.
            let replies = self
                .services
                .db()
                .graph_proposals()
                .unsettled_replies(&session_id)
                .await
                .unwrap_or_default();
            for reply in replies {
                if self.is_deciding_graph_proposal_reply(&reply.proposal_id) {
                    continue;
                }
                if let Ok(Some(state)) = self
                    .services
                    .db()
                    .graph_proposals()
                    .get(&reply.proposal_id, Some(reply.version))
                    .await
                    && let Some((folder, transcript)) = self.reply_session_handles(&session_id)
                {
                    self.spawn_graph_proposal_reply_decision(
                        reply,
                        Vec::new(),
                        &state,
                        folder,
                        transcript,
                    );
                }
            }
        }
    }

    /// Records one reply with its visible prompt and decides it in the
    /// background.
    async fn reply_to_graph_proposal(
        &mut self,
        session_id: &SessionId,
        state: &GraphProposalState,
        prompt: &TurnPrompt,
    ) -> PromptWorkflowOutcome {
        let Some((folder, transcript)) = self.reply_session_handles(session_id) else {
            return PromptWorkflowOutcome::KeepPrompt;
        };
        if self.is_deciding_graph_proposal_reply(&state.input.id) {
            return self
                .reject_graph_proposal_reply(session_id, DECISION_PENDING)
                .await;
        }
        let reply = GraphProposalReplyRecord {
            id: Uuid::new_v4().to_string(),
            is_settled: false,
            message: prompt.transcript_text(),
            outcome: None,
            proposal_id: state.input.id.clone(),
            session_id: session_id.to_string(),
            version: state.version.number,
        };
        if let Err(error) = self
            .services
            .db()
            .graph_proposals()
            .record_reply(&reply.id, &reply.proposal_id, reply.version, &reply.message)
            .await
        {
            return self
                .reject_graph_proposal_reply(session_id, &format!("Graph proposal: {error}"))
                .await;
        }
        transcript
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .append_message(SessionMessageKind::UserPrompt, &reply.message);
        SessionTaskService::emit_session_updated(
            &self.services.event_sender(),
            &self.services.session_update_versions(),
            session_id,
        );
        self.spawn_graph_proposal_reply_decision(
            reply,
            prompt.attachments.clone(),
            state,
            folder,
            transcript,
        );

        PromptWorkflowOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    }

    /// Reads the workspace and live transcript a decision reports into.
    fn reply_session_handles(
        &self,
        session_id: &SessionId,
    ) -> Option<(PathBuf, Arc<Mutex<SessionTranscript>>)> {
        self.sessions
            .session_and_handles_or_err(session_id)
            .ok()
            .map(|(session, handles)| (session.folder.clone(), Arc::clone(&handles.transcript)))
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

    /// Decides with the proposal's captured selection, session ownership,
    /// and a bounded provider budget, then admits the action for the exact
    /// replied-to version. A version that changed meanwhile rejects the
    /// action instead of retargeting it. Stopping the session abandons an
    /// undecided reply; shutdown leaves it for restart recovery.
    ///
    /// Only a revision keeps the reply `images`, so a fresh approval of a
    /// reply with images is decided as a revision instead and lets generation
    /// consider them first. At the generation limit, where no revision is
    /// admitted, it approves and reports that the images were not used.
    fn spawn_graph_proposal_reply_decision(
        &self,
        reply: GraphProposalReplyRecord,
        images: Vec<TurnPromptAttachment>,
        state: &GraphProposalState,
        folder: PathBuf,
        transcript: Arc<Mutex<SessionTranscript>>,
    ) {
        let cancellation = CancellationToken::new();
        let is_abandoned = Arc::new(AtomicBool::new(false));
        self.services
            .proposal_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                reply.proposal_id.clone(),
                ProposalReplyDecision {
                    cancellation: cancellation.clone(),
                    is_abandoned: Arc::clone(&is_abandoned),
                    session_id: state.input.session_id.clone(),
                },
            );
        let decision = GraphProposalReplyDecision::new(state, folder, reply.message.clone());
        let revises_to_keep_images =
            !images.is_empty() && state.version.number < state.limits.generations;
        let run_client = ag_worker::scoped_client(
            self.services.run_client(),
            ag_worker::RunScope {
                cancellation: Some(cancellation.clone()),
                purpose: Some(DECISION_RUN_PURPOSE.to_string()),
                session_id: Some(state.input.session_id.to_string()),
                ..ag_worker::RunScope::default()
            },
        );
        let handle = self.sessions.handle();
        let services = self.services.clone();
        let session_id = state.input.session_id.clone();
        self.services.track_cleanup_task(tokio::spawn(async move {
            let outcome = match reply.outcome {
                Some(outcome) => outcome,
                None => match decision.decide(run_client.as_ref()).await {
                    GraphProposalReplyOutcome::Approve if revises_to_keep_images => {
                        GraphProposalReplyOutcome::Revise
                    }
                    decided => decided,
                },
            };
            let notice = if !cancellation.is_cancelled() {
                services
                    .apply_proposal_reply(&handle, &cancellation, &reply, images, outcome)
                    .await
            } else if is_abandoned.load(Ordering::Acquire) {
                // A failed write leaves the reply for restart recovery.
                let _ = services.settle_proposal_reply(&reply.id).await;

                Some(DECISION_CANCELED.to_string())
            } else {
                None
            };
            services.finish_proposal_reply_decision(&reply.proposal_id);
            // A reply left for recovery reports once its action is attempted.
            let Some(notice) = notice else {
                return;
            };
            SessionTaskService::append_workflow_notice(
                &transcript,
                services.db(),
                &services.event_sender(),
                &services.session_update_versions(),
                &session_id,
                &notice,
            )
            .await;
        }));
    }
}

impl AppServices {
    /// Cancels background reply decisions. Stopping one session abandons its
    /// undecided replies; stopping all at shutdown keeps them recoverable.
    pub(super) fn cancel_proposal_reply_decisions(&self, session_id: Option<&str>) {
        for decision in self
            .proposal_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
        {
            if let Some(session_id) = session_id {
                if decision.session_id.as_str() != session_id {
                    continue;
                }
                decision.is_abandoned.store(true, Ordering::Release);
            }
            decision.cancellation.cancel();
        }
    }

    /// Records the first decision and admits its action for the replied-to
    /// version, then settles the reply whether or not the attempted action
    /// succeeded, and returns the notice to show.
    ///
    /// A project switch can unload the session while its reply is decided,
    /// and the session runtime can stop before it answers. The action then
    /// was not attempted, so the reply stays unsettled without a notice and
    /// recovery applies the recorded decision once the session is loaded
    /// again. A reply recovered at startup waits for the runtime to accept
    /// commands; shutdown ends that wait and leaves the reply unsettled.
    ///
    /// A revision carries the reply `images`; an approval reports that it
    /// could not use them.
    async fn apply_proposal_reply(
        &self,
        handle: &SessionRuntimeHandle,
        cancellation: &CancellationToken,
        reply: &GraphProposalReplyRecord,
        images: Vec<TurnPromptAttachment>,
        outcome: GraphProposalReplyOutcome,
    ) -> Option<String> {
        let proposals = self.db().graph_proposals();
        // An earlier recorded decision wins over this one.
        let outcome = match proposals.decide_reply(&reply.id, outcome).await {
            Ok(recorded) => recorded.outcome.unwrap_or(outcome),
            Err(error) => return Some(reply_failure(&error)),
        };
        let version = reply.version;
        let (action, success) = match outcome {
            GraphProposalReplyOutcome::Approve if images.is_empty() => (
                GraphProposalAction::Approve,
                format!("Graph proposal v{version} approved · execution started"),
            ),
            GraphProposalReplyOutcome::Approve => (
                GraphProposalAction::Approve,
                format!(
                    "Graph proposal v{version} approved · execution started · reply images not \
                     used: generation limit reached"
                ),
            ),
            GraphProposalReplyOutcome::Cancel => (
                GraphProposalAction::Cancel,
                format!("Graph proposal v{version} cancelled."),
            ),
            GraphProposalReplyOutcome::Revise => {
                let mut feedback = TurnPrompt::from_text(reply.message.clone());
                feedback.attachments = images;

                (
                    GraphProposalAction::Revise(feedback),
                    format!("Generating graph proposal v{}…", version + 1),
                )
            }
        };
        tokio::select! {
            biased;
            () = handle.wait_until_available() => {}
            () = cancellation.cancelled() => return None,
        }
        let result = handle
            .act_on_graph_proposal(&reply.proposal_id, version, action)
            .await;
        if result.as_ref().is_err_and(|error| {
            *error == SessionError::NotFound || SessionRuntimeHandle::is_unavailable(error)
        }) {
            return None;
        }
        let settled = self.settle_proposal_reply(&reply.id).await;

        Some(
            result
                .map_err(|error| error.to_string())
                .and(settled)
                .map_or_else(|error| reply_failure(&error), |()| success),
        )
    }

    async fn settle_proposal_reply(&self, reply_id: &str) -> Result<(), String> {
        self.db()
            .graph_proposals()
            .settle_reply(reply_id)
            .await
            .map_err(|error| error.to_string())
    }

    fn finish_proposal_reply_decision(&self, proposal_id: &str) {
        self.proposal_reply_decisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(proposal_id);
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
fn reply_failure(error: &dyn std::fmt::Display) -> String {
    TranscriptNotice::ReplyError.format(format!("Graph proposal: {error}"))
}

#[cfg(test)]
#[path = "graph_proposal_reply_test.rs"]
pub(crate) mod tests;
