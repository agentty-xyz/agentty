//! Shared decision run and background registry behind graph chat replies.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ag_session::AgentSelection;
use ag_worker::RunClient;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::app::AppServices;
use crate::domain::agent::{ReasoningLevel, SpeedMode};
use crate::domain::session::SessionId;

/// Provider calls one decision may spend, including transport retries.
pub(super) const DECISION_PROVIDER_CALLS: usize = 3;

/// Replies whose background decision is in flight, keyed by the proposal or
/// output they act on.
pub(super) type ReplyDecisions = Arc<Mutex<HashMap<String, ReplyDecision>>>;

/// A background decision, cancelled with its session or at shutdown.
pub(super) struct ReplyDecision {
    pub(super) cancellation: CancellationToken,
    pub(super) is_abandoned: Arc<AtomicBool>,
    pub(super) session_id: SessionId,
}

impl AppServices {
    /// Cancels background reply decisions. Stopping one session abandons its
    /// undecided replies; stopping all at shutdown keeps them recoverable.
    pub(super) fn cancel_graph_reply_decisions(&self, session_id: Option<&str>) {
        for decisions in [&self.proposal_reply_decisions, &self.output_reply_decisions] {
            for decision in decisions
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
    }
}

/// Submits `prompt` as one read-only utility run on the Run Worker, within a
/// bounded provider budget, and parses the answer with `parse`.
///
/// Returns `None` when the run fails or `parse` rejects the answer, logging a
/// warning tagged with `subject`, so callers can fall back to their safe
/// action.
pub(super) async fn decide<Reply>(
    run_client: &dyn RunClient,
    agent: AgentSelection,
    folder: &Path,
    prompt: String,
    subject: &'static str,
    parse: fn(&str) -> Option<Reply>,
) -> Option<Reply> {
    let submission = run_client
        .submit(ag_contracts::OneShotRequest {
            activity_tx: None,
            attachments: Vec::new(),
            child_pid: None,
            execution_policy: ag_contracts::ExecutionPolicy::default(),
            folder: folder.to_path_buf(),
            harness: agent.kind().to_string(),
            model: agent.model().as_str().to_string(),
            permission_mode: ag_contracts::PermissionMode::ReadOnly,
            prompt,
            provider_call_budget: Some(ag_contracts::ProviderCallBudget::new(
                DECISION_PROVIDER_CALLS,
            )),
            reasoning_level: ReasoningLevel::default(),
            request_kind: ag_contracts::AgentRequestKind::UtilityPrompt,
            speed_mode: SpeedMode::Normal,
        })
        .await;
    let answer = match submission {
        Ok(submission) => submission.response.to_answer_display_text(),
        Err(error) => {
            warn!(error = %error, subject, "graph reply decision failed; revising");

            return None;
        }
    };
    let reply = parse(&answer);

    if reply.is_none() {
        let answer_bytes = answer.len();
        warn!(answer_bytes, %subject, "unknown graph reply decision; revising");
    }

    reply
}

/// Normalizes a one-word contract answer, tolerating surrounding quotes, code
/// ticks, a trailing period, and letter case.
pub(super) fn contract_word(answer: &str) -> String {
    answer
        .trim()
        .trim_matches(['`', '"', '\'', '.'])
        .to_ascii_lowercase()
}
