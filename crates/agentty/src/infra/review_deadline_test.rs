use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_contracts::{
    AgentRequestKind, OneShotError, OneShotRequest, OneShotSubmission, PermissionMode,
    ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::AgentResponse;
use ag_worker::RunClient;
use async_trait::async_trait;

use super::ReviewDeadlineClient;

struct DeadlineFixture {
    calls: AtomicUsize,
}

#[async_trait]
impl RunClient for DeadlineFixture {
    async fn submit(&self, _: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(OneShotSubmission {
                response: AgentResponse::plain("done"),
                stats: SessionStats::default(),
            });
        }

        std::future::pending().await
    }
}

fn request() -> OneShotRequest {
    OneShotRequest {
        child_pid: None,
        folder: PathBuf::from("."),
        harness: "claude".into(),
        model: "fixture".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: String::new(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::High,
        request_kind: AgentRequestKind::FocusedReview,
        speed_mode: SpeedMode::Normal,
    }
}

#[tokio::test]
async fn one_deadline_cancels_a_hung_call_and_prevents_later_submissions() {
    // Arrange
    let fixture = DeadlineFixture {
        calls: AtomicUsize::new(0),
    };
    let client = ReviewDeadlineClient::new(&fixture, Duration::from_millis(50));

    // Act
    let first = client.submit(request()).await.expect("initial result");
    let hung = client.submit(request()).await.expect_err("deadline");
    let later = client
        .submit(request())
        .await
        .expect_err("deadline stays expired");

    // Assert
    assert_eq!(first.response.answer, "done");
    assert!(hung.to_string().contains("deadline exceeded"));
    assert_eq!(hung, later);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
}
