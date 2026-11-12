//! Graph clarification questions answered through the session question panel.
//!
//! A pending graph proposal's clarification questions, or the questions of
//! the graph node a chat reply would answer, open the same question panel as
//! an ordinary turn's questions while their session chat is viewed. Completed
//! answers use the ordinary clarification reply format and are sent to the
//! exact proposal version or node execution that asked. Answers that work no
//! longer waits for close the panel, so the questions pending now can open.
//! Dismissing the panel keeps those questions closed until the session is
//! reopened; a typed chat reply still answers them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ag_session::{GraphExecutionRef, QuestionAnswer};

use crate::app::App;
use crate::app::graph_proposal_reply::clarification_questions;
use crate::app::prompt_intent::PromptWorkflowOutcome;
use crate::domain::question::{QuestionItem, question_answer_message};
use crate::domain::session::{SessionId, Status};
use crate::domain::turn_prompt::TurnPrompt;
use crate::presentation::app_mode::AppMode;

/// Longest wait before an unchanged viewed session is checked again; node
/// questions become answerable when their execution settles, which does not
/// always publish a session update.
const RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Graph work whose clarification questions a question panel answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GraphQuestionTarget {
    /// Latest execution of a generic graph node.
    Node(GraphExecutionRef),
    /// One exact version of a pending graph proposal.
    Proposal { proposal_id: String, version: u32 },
}

/// Pending clarification questions and the graph work that asked them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphQuestions {
    pub(crate) questions: Vec<QuestionItem>,
    pub(crate) target: GraphQuestionTarget,
}

/// Result of sending one completed answer set to graph work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphAnswerOutcome {
    /// The graph work that asked received the answers.
    Sent,
    /// The answers were rejected, but the same graph work may accept them on
    /// retry.
    Retry,
    /// The graph work that asked no longer waits for these answers.
    Stale,
}

impl GraphAnswerOutcome {
    /// Classifies one admitted-or-rejected reply to graph work that still
    /// waited when the reply was routed.
    pub(crate) fn from_reply(outcome: &PromptWorkflowOutcome) -> Self {
        if *outcome == PromptWorkflowOutcome::KeepPrompt {
            return Self::Retry;
        }

        Self::Sent
    }
}

/// Tracks which graph work the open question panel answers.
#[derive(Debug, Default)]
pub(crate) struct GraphQuestionState {
    /// Session and graph work answered by the open question panel; `None`
    /// while the panel answers an ordinary session turn.
    active: Option<(SessionId, GraphQuestionTarget)>,
    /// Session, update version, and time of the latest pending-question check.
    checked: Option<(SessionId, u64, Instant)>,
    /// Questions closed without answering, per session, until it is reopened.
    dismissed: HashMap<SessionId, GraphQuestionTarget>,
    /// Graph work answered by each session's saved partial answers; absent
    /// when the saved answers belong to an ordinary session turn.
    progress_targets: HashMap<SessionId, GraphQuestionTarget>,
}

impl GraphQuestionState {
    /// Records that the open question panel answers an ordinary session turn.
    pub(crate) fn clear_active(&mut self) {
        self.active = None;
    }

    /// Records the graph work, if any, answered by the question panel that
    /// opens for `session_id`.
    pub(crate) fn activate(&mut self, session_id: &str, target: Option<GraphQuestionTarget>) {
        self.active = target.map(|target| (SessionId::from(session_id), target));
    }

    /// Binds partial answers saved for `session_id` to the graph work its open
    /// panel answers, so they are restored only for that same work.
    pub(crate) fn record_progress_target(&mut self, session_id: &str) {
        match self.active_target(session_id).cloned() {
            Some(target) => {
                self.progress_targets
                    .insert(SessionId::from(session_id), target);
            }
            None => {
                self.progress_targets.remove(session_id);
            }
        }
    }

    /// Removes and returns the graph work answered by the partial answers
    /// saved for `session_id`.
    pub(crate) fn take_progress_target(&mut self, session_id: &str) -> Option<GraphQuestionTarget> {
        self.progress_targets.remove(session_id)
    }

    /// Reopens dismissed questions when their session is opened again.
    pub(crate) fn forget_dismissal(&mut self, session_id: &str) {
        self.dismissed.remove(session_id);
    }

    /// Returns the graph work answered by the open panel for `session_id`.
    pub(crate) fn active_target(&self, session_id: &str) -> Option<&GraphQuestionTarget> {
        self.active
            .as_ref()
            .filter(|(active_session_id, _)| active_session_id == session_id)
            .map(|(_, target)| target)
    }

    fn needs_check(&self, session_id: &str, version: u64, now: Instant) -> bool {
        self.checked
            .as_ref()
            .is_none_or(|(checked_session_id, checked_version, checked_at)| {
                checked_session_id != session_id
                    || *checked_version != version
                    || now.duration_since(*checked_at) >= RECHECK_INTERVAL
            })
    }
}

impl App {
    /// Opens the question panel for the viewed session's pending graph
    /// questions, so they are answered like an ordinary turn's questions.
    ///
    /// Only the plain session view switches, never an open composer or
    /// overlay, and never for archived sessions. An ordinary turn's pending
    /// questions take precedence, and
    /// lookup failures leave the view unchanged; a typed reply reports them.
    /// Returns whether the mode changed.
    pub(crate) async fn reconcile_graph_question_mode(&mut self) -> bool {
        let AppMode::View { session_id, .. } = &self.mode else {
            return false;
        };
        let session_id = session_id.clone();
        // Archived sessions keep settled graph questions but no longer
        // accept answers, so their chat stays in place.
        if self
            .sessions
            .session_for_id(&session_id)
            .is_none_or(|session| {
                matches!(
                    session.status,
                    Status::Question | Status::Done | Status::Canceled
                )
            })
        {
            return false;
        }
        let version = self.session_update_version(&session_id);
        let now = self.services.clock().now_instant();
        if !self.graph_questions.needs_check(&session_id, version, now) {
            return false;
        }
        self.graph_questions.checked = Some((session_id.clone(), version, now));
        let Ok(Some(pending)) = self.pending_graph_questions(&session_id).await else {
            return false;
        };
        if self.graph_questions.dismissed.get(&session_id) == Some(&pending.target) {
            return false;
        }

        self.open_question_panel(&session_id, pending.questions, Some(pending.target));

        true
    }

    /// Returns the clarification questions a chat reply in this session
    /// would answer now, following the same precedence as chat routing: a
    /// pending proposal receives every reply, otherwise the node whose
    /// question was asked last.
    ///
    /// # Errors
    /// Returns an error when the proposal or graph lookup fails.
    pub(crate) async fn pending_graph_questions(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<GraphQuestions>, String> {
        if let Some(state) = self.pending_graph_proposal(session_id).await? {
            let questions = clarification_questions(&state);
            if questions.is_empty() || self.is_deciding_graph_proposal_reply(&state.input.id) {
                return Ok(None);
            }

            return Ok(Some(GraphQuestions {
                questions: questions.to_vec(),
                target: GraphQuestionTarget::Proposal {
                    proposal_id: state.input.id.clone(),
                    version: state.version.number,
                },
            }));
        }

        Ok(self
            .pending_graph_node_questions(session_id)
            .await?
            .map(|(execution, questions)| GraphQuestions {
                questions,
                target: GraphQuestionTarget::Node(execution),
            }))
    }

    /// Closes the open graph question panel without answering.
    ///
    /// Returns `false`, changing nothing, when the panel answers an ordinary
    /// session turn instead.
    pub(crate) fn dismiss_graph_questions(&mut self, session_id: &SessionId) -> bool {
        let Some(target) = self.graph_questions.active_target(session_id).cloned() else {
            return false;
        };
        self.graph_questions.active = None;
        self.graph_questions
            .dismissed
            .insert(session_id.clone(), target);
        self.mode = AppMode::View {
            session_id: session_id.clone(),
            scroll_offset: None,
        };

        true
    }

    /// Sends one completed answer set to the graph work that asked.
    ///
    /// Only a retryable rejection keeps the panel's target. Sent and stale
    /// answers release it and check again on the next tick, so the questions
    /// pending now open instead of a target that can never accept answers.
    pub(crate) async fn answer_graph_questions(
        &mut self,
        session_id: &SessionId,
        target: &GraphQuestionTarget,
        answers: &[QuestionAnswer],
    ) -> GraphAnswerOutcome {
        let prompt = TurnPrompt::from_text(question_answer_message(answers));
        let outcome = match target {
            GraphQuestionTarget::Node(execution) => {
                self.answer_graph_node_questions(session_id, execution, &prompt)
                    .await
            }
            GraphQuestionTarget::Proposal {
                proposal_id,
                version,
            } => {
                self.answer_graph_proposal_questions(session_id, proposal_id, *version, &prompt)
                    .await
            }
        };
        if outcome != GraphAnswerOutcome::Retry {
            self.graph_questions.active = None;
            // Check again on the next tick so pending questions open promptly.
            self.graph_questions.checked = None;
        }

        outcome
    }
}

#[cfg(test)]
#[path = "graph_question_test.rs"]
pub(crate) mod tests;
