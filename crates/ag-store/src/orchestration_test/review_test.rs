use ag_session::{
    FocusedReviewStatus, IntegrationApproach, OrchestrationStatus, OrchestrationTaskStatus,
};

use super::support::{controller_fixture, insert_orchestration_child, planned_task};

#[tokio::test]
async fn approval_persists_plan_and_releases_proposed_tasks() {
    // Arrange
    let database = controller_fixture().await;
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration(
            "controller",
            &OrchestrationStatus::AwaitingApproval.to_string(),
            2,
        )
        .await
        .expect("failed to insert orchestration");
    let task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "follow-up"))
        .await
        .expect("failed to insert proposed task");
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::Proposed.to_string(),
            None,
        )
        .await
        .expect("failed to propose task");

    // Act
    database
        .orchestrations()
        .update_orchestration_plan(orchestration_id, "Ship the campaign", 4)
        .await
        .expect("failed to update campaign plan");
    let approved = database
        .orchestrations()
        .approve_orchestration_plan(orchestration_id)
        .await
        .expect("failed to approve campaign");
    let duplicate_approval = database
        .orchestrations()
        .approve_orchestration_plan(orchestration_id)
        .await
        .expect("failed to inspect duplicate approval");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load campaign")
        .expect("campaign should exist");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load campaign tasks")
        .remove(0);

    // Assert
    assert!(approved);
    assert!(!duplicate_approval);
    assert_eq!(orchestration.goal_statement, "Ship the campaign");
    assert_eq!(orchestration.max_parallelism, 4);
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::Running.to_string()
    );
    assert_eq!(task.status, OrchestrationTaskStatus::Planned.to_string());
}

#[tokio::test]
async fn integration_approval_persists_selected_approach_atomically() {
    // Arrange
    let database = controller_fixture().await;
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration(
            "controller",
            &OrchestrationStatus::AwaitingIntegration.to_string(),
            2,
        )
        .await
        .expect("failed to insert orchestration");

    // Act
    let approved = database
        .orchestrations()
        .approve_orchestration_integration(orchestration_id, IntegrationApproach::ReviewRequest)
        .await
        .expect("failed to approve integration");
    let duplicate_approval = database
        .orchestrations()
        .approve_orchestration_integration(orchestration_id, IntegrationApproach::LocalMerge)
        .await
        .expect("failed to inspect duplicate approval");
    let approach = database
        .orchestrations()
        .load_orchestration_integration_approach(orchestration_id)
        .await
        .expect("failed to load integration approach");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load orchestration")
        .expect("orchestration should exist");

    // Assert
    assert!(approved);
    assert!(!duplicate_approval);
    assert_eq!(approach, IntegrationApproach::ReviewRequest.to_string());
    assert_eq!(
        orchestration.status,
        OrchestrationStatus::Integrating.to_string()
    );
}

#[tokio::test]
async fn review_application_claim_is_bounded_and_clears_consumed_review() {
    // Arrange
    let database = controller_fixture().await;
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", None)
        .await
        .expect("failed to reload project");
    let orchestration_id = database
        .orchestrations()
        .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
        .await
        .expect("failed to insert orchestration");
    let task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(orchestration_id, "reviewed"))
        .await
        .expect("failed to insert reviewed task");
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("failed to claim reviewed task")
    );
    insert_orchestration_child(&database, project_id, "child-reviewed", task_id).await;
    assert!(
        database
            .orchestrations()
            .link_orchestration_task_child(task_id, "child-reviewed")
            .await
            .expect("failed to link reviewed child")
    );
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::Reviewing.to_string(),
            None,
        )
        .await
        .expect("failed to mark child reviewing");
    database
        .sessions()
        .update_session_focused_review(
            "child-reviewed",
            Some(FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("### Suggestions\n\n- Fix it".to_string()),
        )
        .await
        .expect("failed to seed focused review");

    // Act
    let claimed = database
        .orchestrations()
        .claim_orchestration_review_application(task_id, "Verify then apply", 3)
        .await
        .expect("failed to claim review application");
    let duplicate_claim = database
        .orchestrations()
        .claim_orchestration_review_application(task_id, "Duplicate", 3)
        .await
        .expect("failed to inspect duplicate review application");
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load reviewed task")
        .remove(0);
    let review_cache = database
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load consumed review cache");

    // Assert
    assert!(claimed);
    assert!(!duplicate_claim);
    assert_eq!(
        task.status,
        OrchestrationTaskStatus::ReviewApplying.to_string()
    );
    assert_eq!(task.continuation_generation, 1);
    assert_eq!(
        task.continuation_prompt.as_deref(),
        Some("Verify then apply")
    );
    assert_eq!(task.review_iteration, 1);
    assert_eq!(review_cache, [] as [crate::SessionFocusedReviewRow; 0]);
    assert_eq!(task.child_focused_review_status, None);
    assert_eq!(task.child_focused_review_text, None);
}
