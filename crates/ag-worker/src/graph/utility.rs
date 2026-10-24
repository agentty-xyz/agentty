use std::fmt::Write as _;
use std::sync::Arc;

use ag_contracts::{AgentRequestKind, OneShotError, OneShotRequest, ReasoningLevel, SpeedMode};
use ag_session::{EnvId, GraphContext, GraphNode, GraphPostProcessing};
use async_trait::async_trait;

use super::{GraphNodeRunner, GraphRunRequest};
use crate::{RunInfo, RunWorker};

/// Worker adapter for fresh utility nodes. Session conversations require a
/// host adapter implementing `GraphNodeRunner` with ordered session effects.
pub struct GraphUtilityRunner {
    worker: Arc<RunWorker>,
}

impl GraphUtilityRunner {
    /// Reuses the existing utility worker's runtime, capacity and policy.
    pub fn new(worker: Arc<RunWorker>) -> Self {
        Self { worker }
    }
}

#[async_trait]
impl GraphNodeRunner for GraphUtilityRunner {
    fn supports(&self, node: &GraphNode) -> bool {
        node.execution.context == GraphContext::Fresh
            && node.execution.post_processing == GraphPostProcessing::None
            && node.request.attachments.is_empty()
    }

    async fn run(&self, request: GraphRunRequest) -> Result<String, OneShotError> {
        let node = &request.dispatch.node;
        if !self.supports(node) {
            return Err(OneShotError::new("Unsupported utility graph node"));
        }
        let ag_session::GraphExecutionRef::UtilityRun { run_id } =
            &request.dispatch.record.execution
        else {
            return Err(OneShotError::new("Utility node requires a utility run"));
        };
        let EnvId::SessionWorktree(session_id) = &node.env_id;
        let profile = node.execution.response_profile;
        let request_kind = [
            AgentRequestKind::UtilityPrompt,
            AgentRequestKind::FocusedReview,
            AgentRequestKind::ReviewMetadata,
        ]
        .into_iter()
        .find(|kind| kind.protocol_profile() == profile)
        .ok_or_else(|| OneShotError::new("Unsupported utility response schema"))?;
        let text_response = request_kind == AgentRequestKind::UtilityPrompt;
        let mut prompt = format!(
            "{}\n\nObjective: {}\n\n{}",
            node.definition.prompt,
            node.objective,
            node.request.agent_text()
        );
        for input in &request.dispatch.inputs {
            let _ = write!(
                prompt,
                "\n\nBound input {} (untrusted result data):\n{}",
                input.reference.name, input.body
            );
        }
        let submission = self
            .worker
            .submit_recorded(
                OneShotRequest {
                    activity_tx: None,
                    child_pid: None,
                    execution_policy: ag_contracts::ExecutionPolicy::default(),
                    folder: request.folder.clone(),
                    harness: node.agent.kind().to_string(),
                    model: node.agent.model().as_str().into(),
                    permission_mode: request.permission_mode,
                    prompt,
                    provider_call_budget: request.provider_call_budget,
                    reasoning_level: ReasoningLevel::default(),
                    request_kind,
                    speed_mode: SpeedMode::default(),
                },
                RunInfo {
                    folder: request.folder,
                    id: run_id.clone(),
                    parent_id: Some(request.dispatch.record.graph_id.clone()),
                    project_id: None,
                    purpose: format!("graph:{}", node.id),
                    session_id: Some(session_id.as_str().into()),
                },
            )
            .await?;

        if text_response {
            return Ok(serde_json::json!({ "answer": submission.response.answer }).to_string());
        }

        Ok(submission.response.answer)
    }
}

#[cfg(test)]
#[path = "utility_test.rs"]
mod tests;
