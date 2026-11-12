use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{OneShotClient, OneShotError, OneShotRequest, OneShotSubmission};
use crate::{AgentRequestKind, ExecutionPolicy, PermissionMode, ReasoningLevel, SpeedMode};

struct StatelessClient(AtomicUsize);

#[async_trait]
impl OneShotClient for StatelessClient {
    async fn submit(&self, _: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        self.0.fetch_add(1, Ordering::SeqCst);

        Err(OneShotError::new("unexpected submission"))
    }
}

#[tokio::test]
async fn default_close_is_repeatable_without_submitting_work() {
    // Arrange
    let client = StatelessClient(AtomicUsize::new(0));

    // Act
    let cleanup = tokio::time::timeout(Duration::from_secs(1), async {
        client.close().await;
        client.close().await;
    })
    .await;

    // Assert
    cleanup.expect("stateless cleanup completes without provider work");
    assert_eq!(client.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn default_submit_cancellable_prefers_cancellation_and_otherwise_submits() {
    // Arrange
    let client = StatelessClient(AtomicUsize::new(0));
    let canceled = CancellationToken::new();
    canceled.cancel();

    // Act
    client.force_shutdown();
    let canceled_result = client.submit_cancellable(request(), canceled).await;
    let canceled_submissions = client.0.load(Ordering::SeqCst);
    let submitted_result = client
        .submit_cancellable(request(), CancellationToken::new())
        .await;

    // Assert
    assert_eq!(
        canceled_result,
        Err(OneShotError::new("[Stopped] Agent run canceled"))
    );
    assert_eq!(canceled_submissions, 0);
    assert_eq!(
        submitted_result,
        Err(OneShotError::new("unexpected submission"))
    );
    assert_eq!(client.0.load(Ordering::SeqCst), 1);
}

fn request() -> OneShotRequest {
    OneShotRequest {
        activity_tx: None,
        attachments: Vec::new(),
        child_pid: None,
        execution_policy: ExecutionPolicy::default(),
        folder: PathBuf::from("."),
        harness: "codex".to_string(),
        model: "gpt-5.6-sol".to_string(),
        permission_mode: PermissionMode::default(),
        prompt: "Summarize changes".to_string(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::UtilityPrompt,
        speed_mode: SpeedMode::Normal,
    }
}
