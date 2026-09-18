//! Durable reuse of successful calls for retries of unchanged review input.

use std::sync::Arc;

use ag_agent::{
    AgentRequestKind, OneShotError, OneShotRequest, OneShotSubmission, SessionStats,
    is_input_size_error,
};
use ag_protocol::{AgentResponse, FocusedReview};
use ag_store::AppRepositories;
use ag_worker::RunClient;
use async_trait::async_trait;
use serde::Deserialize;

use super::review::diff_content_hash;

/// Size rejections retain the partition plan so retries do not spend their
/// entire budget rediscovering smaller provider limits for completed batches.
#[derive(Deserialize)]
enum CachedReviewCall {
    Answer(String),
    InputLimit(String),
}

/// Replays validated evidence without charging a second provider turn. Fresh
/// reasoning always goes through the injected worker. Full request keys include
/// model and profile, while generation keys bind evidence to diff and history.
pub(super) struct ReviewResumeClient {
    client: Arc<dyn RunClient>,
    generation: String,
    repositories: AppRepositories,
    request_id: String,
    session_id: String,
}

impl ReviewResumeClient {
    /// Activates the generation during serialized review creation, before
    /// background work can reorder the first provider submissions.
    pub(super) async fn new(
        client: Arc<dyn RunClient>,
        repositories: &AppRepositories,
        session_id: &str,
        diff_hash: u64,
        history: &str,
        request_id: uuid::Uuid,
    ) -> Result<Self, OneShotError> {
        let request_id = request_id.to_string();
        let generation = format!("{diff_hash}:{}", diff_content_hash(history));
        repositories
            .sessions()
            .begin_review_generation(session_id, &generation, &request_id)
            .await
            .map_err(|error| OneShotError::new(error.to_string()))?;

        Ok(Self {
            request_id,
            client,
            repositories: repositories.clone(),
            session_id: session_id.to_string(),
            generation,
        })
    }

    fn cacheable(request: &OneShotRequest, answer: &str) -> bool {
        match request.request_kind {
            AgentRequestKind::FocusedReview => {
                serde_json::from_str::<FocusedReview>(answer).is_ok()
            }
            AgentRequestKind::UtilityPrompt => request
                .prompt
                .split_once("Keep answer within ")
                .and_then(|(_, suffix)| suffix.split_whitespace().next())
                .and_then(|limit| limit.parse::<usize>().ok())
                .is_some_and(|limit| !answer.trim().is_empty() && answer.trim().len() <= limit),
            _ => false,
        }
    }
}

#[async_trait]
impl RunClient for ReviewResumeClient {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let key = format!(
            "{:?}\n{}\n{}\n{:?}\n{:?}\n{:?}\n{}",
            request.request_kind,
            request.harness,
            request.model,
            request.reasoning_level,
            request.speed_mode,
            request.permission_mode,
            request.prompt
        );
        if let Some(saved) = self
            .repositories
            .sessions()
            .load_review_fragment(&self.session_id, &self.generation, &key)
            .await
            .map_err(|error| OneShotError::new(error.to_string()))?
        {
            return match serde_json::from_str::<CachedReviewCall>(&saved)
                .map_err(|error| OneShotError::new(format!("Invalid review checkpoint: {error}")))?
            {
                CachedReviewCall::Answer(answer) => Ok(OneShotSubmission {
                    response: AgentResponse::plain(answer),
                    stats: SessionStats::default(),
                }),
                CachedReviewCall::InputLimit(error) => Err(OneShotError::new(error)),
            };
        }
        let response = self.client.submit(request.clone()).await;
        let checkpoint = match &response {
            Ok(response) if Self::cacheable(&request, &response.response.answer) => {
                Some(CachedReviewCall::Answer(response.response.answer.clone()))
            }
            Err(error)
                if is_input_size_error(&error.to_string())
                    && request
                        .provider_call_budget
                        .as_ref()
                        .is_none_or(|budget| budget.ensure_available().is_ok()) =>
            {
                Some(CachedReviewCall::InputLimit(error.to_string()))
            }
            _ => None,
        };
        if let Some(checkpoint) = checkpoint {
            let saved = match checkpoint {
                CachedReviewCall::Answer(answer) => serde_json::json!({"Answer": answer}),
                CachedReviewCall::InputLimit(error) => serde_json::json!({"InputLimit": error}),
            }
            .to_string();
            self.repositories
                .sessions()
                .save_review_fragment(
                    &self.session_id,
                    &self.generation,
                    &self.request_id,
                    &key,
                    &saved,
                )
                .await
                .map_err(|error| OneShotError::new(error.to_string()))?;
        }
        response
    }
}

#[cfg(test)]
#[path = "review_resume_test.rs"]
mod tests;
