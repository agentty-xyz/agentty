//! Shared read-only utility run behind graph chat-reply decisions.

use std::path::Path;

use ag_session::AgentSelection;
use ag_worker::RunClient;
use tracing::warn;

use crate::domain::agent::{ReasoningLevel, SpeedMode};

/// Submits `prompt` as one read-only utility run on the Run Worker and parses
/// the answer with `parse`.
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
            provider_call_budget: None,
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
