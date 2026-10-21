//! Orchestrator decision for a reply to a plan awaiting acceptance.

use std::path::PathBuf;

use ag_session::{AgentSelection, TaskChainReplyOutcome};
use ag_worker::RunClient;
use askama::Template;
use tracing::warn;

use crate::domain::agent::{ReasoningLevel, SpeedMode};

/// Askama view model whose inputs are already JSON-encoded strings.
#[derive(Template)]
#[template(path = "task_chain_reply_decision_prompt.md", escape = "none")]
struct TaskChainReplyDecisionPromptTemplate<'a> {
    plan: &'a str,
    reply: &'a str,
}

/// Inputs for deciding whether a reply approves the latest Planner plan.
pub(crate) struct TaskChainReplyDecision {
    /// Provider and model captured by the workflow.
    pub(crate) agent: AgentSelection,
    /// Session workspace used as the read-only run folder.
    pub(crate) folder: PathBuf,
    /// Latest Planner answer awaiting acceptance.
    pub(crate) plan: String,
    /// User-authored reply to classify.
    pub(crate) reply: String,
}

impl TaskChainReplyDecision {
    /// Decides through one read-only utility run on the Run Worker. Only an
    /// explicit `implement` accepts the plan; run failures, unparsable output,
    /// and unknown decisions revise it, so implementation never starts on an
    /// uncertain decision.
    pub(crate) async fn decide(&self, run_client: &dyn RunClient) -> TaskChainReplyOutcome {
        let submission = run_client
            .submit(ag_contracts::OneShotRequest {
                activity_tx: None,
                execution_policy: ag_contracts::ExecutionPolicy::default(),
                provider_call_budget: None,
                harness: self.agent.kind().to_string(),
                child_pid: None,
                folder: self.folder.clone(),
                model: self.agent.model().as_str().to_string(),
                permission_mode: ag_contracts::PermissionMode::ReadOnly,
                prompt: self.prompt(),
                request_kind: ag_contracts::AgentRequestKind::UtilityPrompt,
                reasoning_level: ReasoningLevel::default(),
                speed_mode: SpeedMode::Normal,
            })
            .await;
        let answer = match submission {
            Ok(submission) => submission.response.to_answer_display_text(),
            Err(error) => {
                warn!(error = %error, "task chain reply decision failed; revising the plan");

                return TaskChainReplyOutcome::PlanRevised;
            }
        };

        Self::parse(&answer).unwrap_or_else(|| {
            let answer_bytes = answer.len();
            warn!(answer_bytes, "unknown reply decision; revising plan");

            TaskChainReplyOutcome::PlanRevised
        })
    }

    /// Renders the task contract with plan and reply passed as JSON data.
    fn prompt(&self) -> String {
        let plan = serde_json::json!(self.plan).to_string();
        let reply = serde_json::json!(self.reply).to_string();
        let template = TaskChainReplyDecisionPromptTemplate {
            plan: &plan,
            reply: &reply,
        };

        // Rendering borrowed strings into a `String` cannot fail. A blank
        // prompt would still degrade safely to a revision.
        template.render().unwrap_or_default()
    }

    /// Accepts only the two contract words, tolerating surrounding quotes,
    /// code ticks, a trailing period, and letter case.
    fn parse(answer: &str) -> Option<TaskChainReplyOutcome> {
        match answer
            .trim()
            .trim_matches(['`', '"', '\'', '.'])
            .to_ascii_lowercase()
            .as_str()
        {
            "implement" => Some(TaskChainReplyOutcome::PlanAccepted),
            "revise" => Some(TaskChainReplyOutcome::PlanRevised),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "task_chain_decision_test.rs"]
mod tests;
