use ag_orchestration::OrchestrationApprovalOutcome;

use super::support::seed_active_orchestration_child;
use crate::domain::orchestration::{IntegrationApproach, OrchestrationStatus};

#[tokio::test]
async fn orchestration_approvals_and_detach_update_campaign() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let fixture = seed_active_orchestration_child(&mut app, true).await;
    app.services
        .db()
        .orchestrations()
        .update_orchestration_status(
            fixture.orchestration,
            &OrchestrationStatus::AwaitingApproval.to_string(),
        )
        .await
        .expect("failed to park campaign");

    // Act / Assert
    assert_eq!(
        app.approve_orchestration(&fixture.controller, None).await,
        OrchestrationApprovalOutcome::Approved
    );
    assert_eq!(
        app.approve_orchestration(&fixture.controller, None).await,
        OrchestrationApprovalOutcome::Unavailable
    );
    app.services
        .db()
        .orchestrations()
        .update_orchestration_status(
            fixture.orchestration,
            &OrchestrationStatus::AwaitingIntegration.to_string(),
        )
        .await
        .expect("failed to park integration");
    assert_eq!(
        app.approve_orchestration(&fixture.controller, None).await,
        OrchestrationApprovalOutcome::IntegrationApproachRequired
    );
    assert_eq!(
        app.approve_orchestration(&fixture.controller, Some(IntegrationApproach::LocalMerge),)
            .await,
        OrchestrationApprovalOutcome::Approved
    );
    assert!(app.detach_managed_child(&fixture.child).await);
    assert!(!app.detach_managed_child(&fixture.child).await);
    assert_eq!(
        app.approve_orchestration("missing", None).await,
        OrchestrationApprovalOutcome::Unavailable
    );
}
