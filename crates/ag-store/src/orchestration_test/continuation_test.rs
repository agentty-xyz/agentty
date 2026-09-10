use ag_session::{FocusedReviewStatus, OrchestrationStatus, OrchestrationTaskStatus};

use super::support::{controller_fixture, insert_orchestration_child, planned_task};
use crate::orchestration::SessionOrchestrationTaskRow;
use crate::{AppRepositories, SessionRow};

#[tokio::test]
async fn recoverable_focused_reviews_exclude_outstanding_continuations() {
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
        .upsert_orchestration_task(planned_task(orchestration_id, "recoverable"))
        .await
        .expect("failed to insert recoverable task");
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("failed to claim recoverable task")
    );
    insert_orchestration_child(&database, project_id, "child-recoverable", task_id).await;
    assert!(
        database
            .orchestrations()
            .link_orchestration_task_child(task_id, "child-recoverable")
            .await
            .expect("failed to link recoverable child")
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

    // Act
    let incomplete = database
        .orchestrations()
        .load_recoverable_focused_review_session_ids(project_id)
        .await
        .expect("failed to load incomplete review");
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::ContinuationPending.to_string(),
            None,
        )
        .await
        .expect("failed to mark continuation pending");
    let pending = database
        .orchestrations()
        .load_recoverable_focused_review_session_ids(project_id)
        .await
        .expect("failed to inspect pending continuation recovery");
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::ReviewApplying.to_string(),
            None,
        )
        .await
        .expect("failed to mark review application pending");
    let applying = database
        .orchestrations()
        .load_recoverable_focused_review_session_ids(project_id)
        .await
        .expect("failed to inspect review application recovery");
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::Reviewing.to_string(),
            None,
        )
        .await
        .expect("failed to restore reviewing state");
    database
        .sessions()
        .update_session_focused_review(
            "child-recoverable",
            Some(FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("### Suggestions\n\n- None".to_string()),
        )
        .await
        .expect("failed to complete focused review");
    let completed = database
        .orchestrations()
        .load_recoverable_focused_review_session_ids(project_id)
        .await
        .expect("failed to reload completed review");

    // Assert
    assert_eq!(incomplete, ["child-recoverable"]);
    assert!(pending.is_empty() && applying.is_empty() && completed.is_empty());
}

#[tokio::test]
async fn managed_child_continuation_questions_and_detach_are_durable() {
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
        .upsert_orchestration_task(planned_task(orchestration_id, "alpha"))
        .await
        .expect("failed to insert task");
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("failed to claim task")
    );
    insert_orchestration_child(&database, project_id, "child-alpha", task_id).await;
    assert!(
        database
            .orchestrations()
            .link_orchestration_task_child(task_id, "child-alpha")
            .await
            .expect("failed to link child")
    );
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::WaitingForInput.to_string(),
            None,
        )
        .await
        .expect("failed to wait for child question");

    // Act
    let surfaced = surface_and_clear_orchestration_questions(
        &database,
        orchestration_id,
        task_id,
        r#"[{"text":"Choose one"}]"#,
    )
    .await;
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::Ready.to_string(),
            None,
        )
        .await
        .expect("failed to settle child after its question");
    let queued = database
        .orchestrations()
        .queue_orchestration_continuation(
            task_id,
            "Add the missing edge case",
            r#"["The edge case is tested"]"#,
            r#"["docs/"]"#,
        )
        .await
        .expect("failed to queue continuation");
    let duplicate_queue = database
        .orchestrations()
        .queue_orchestration_continuation(
            task_id,
            "Duplicate",
            r#"["Duplicate"]"#,
            r#"["ignored/"]"#,
        )
        .await
        .expect("failed to inspect duplicate continuation");
    let detached = database
        .orchestrations()
        .detach_orchestration_child("child-alpha")
        .await
        .expect("failed to detach child");
    let duplicate_detach = database
        .orchestrations()
        .detach_orchestration_child("child-alpha")
        .await
        .expect("failed to inspect duplicate detach");
    let (task, child, controller) = load_detached_campaign_state(&database, orchestration_id).await;
    let continuation_prompt = task.continuation_prompt.as_deref();

    // Assert
    assert!(queued && surfaced && detached);
    assert!(!duplicate_queue && !duplicate_detach);
    assert_eq!(task.status, OrchestrationTaskStatus::Detached.to_string());
    assert_eq!(task.child_session_id, None);
    assert_eq!(task.continuation_generation, 1);
    assert_eq!(continuation_prompt, Some("Add the missing edge case"));
    assert_eq!(task.touched_areas, r#"["docs/"]"#);
    assert_eq!(child.role.as_deref(), Some("Worker"));
    assert_eq!(controller.status, "Review");
    assert_eq!(controller.questions.as_deref(), Some(""));
}

#[tokio::test]
async fn question_relay_preserves_controller_questions() {
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
    let task_id = insert_waiting_orchestration_task(
        &database,
        project_id,
        orchestration_id,
        "worker",
        "child-worker",
    )
    .await;
    let controller_question = r#"[{"text":"Controller question"}]"#;
    database
        .sessions()
        .update_session_questions("controller", controller_question)
        .await
        .expect("failed to seed controller question");

    // Act
    let blocked_by_controller = database
        .orchestrations()
        .surface_orchestration_questions(
            orchestration_id,
            task_id,
            r#"[{"text":"Child question"}]"#,
        )
        .await
        .expect("failed to inspect controller question");
    let orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load orchestration")
        .expect("orchestration should exist");
    let controller = database
        .sessions()
        .load_session("controller")
        .await
        .expect("failed to load controller")
        .expect("controller should exist");

    // Assert
    assert!(!blocked_by_controller);
    assert_eq!(orchestration.relayed_question_task_id, None);
    assert_eq!(controller.questions.as_deref(), Some(controller_question));
}

#[tokio::test]
async fn question_relay_claims_one_exact_task_at_a_time() {
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
    let first_task_id = insert_waiting_orchestration_task(
        &database,
        project_id,
        orchestration_id,
        "first",
        "child-first",
    )
    .await;
    let second_task_id = insert_waiting_orchestration_task(
        &database,
        project_id,
        orchestration_id,
        "second",
        "child-second",
    )
    .await;

    // Act
    let first_surfaced = database
        .orchestrations()
        .surface_orchestration_questions(
            orchestration_id,
            first_task_id,
            r#"[{"text":"First child question"}]"#,
        )
        .await
        .expect("failed to surface first child question");
    let second_blocked = database
        .orchestrations()
        .surface_orchestration_questions(
            orchestration_id,
            second_task_id,
            r#"[{"text":"Second child question"}]"#,
        )
        .await
        .expect("failed to inspect occupied relay");
    let claimed_orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load claimed orchestration")
        .expect("orchestration should exist");
    database
        .orchestrations()
        .clear_orchestration_questions(orchestration_id)
        .await
        .expect("failed to release first relay");
    let second_surfaced = database
        .orchestrations()
        .surface_orchestration_questions(
            orchestration_id,
            second_task_id,
            r#"[{"text":"Second child question"}]"#,
        )
        .await
        .expect("failed to surface second child question");
    let second_claimed_orchestration = database
        .orchestrations()
        .load_orchestration_for_controller("controller")
        .await
        .expect("failed to load second claimed orchestration")
        .expect("orchestration should exist");

    // Assert
    assert!(first_surfaced);
    assert!(!second_blocked);
    assert_eq!(
        claimed_orchestration.relayed_question_task_id,
        Some(first_task_id)
    );
    assert!(second_surfaced);
    assert_eq!(
        second_claimed_orchestration.relayed_question_task_id,
        Some(second_task_id)
    );
}

async fn insert_waiting_orchestration_task(
    database: &AppRepositories,
    project_id: i64,
    session_orchestration_id: i64,
    task_key: &str,
    child_session_id: &str,
) -> i64 {
    let task_id = database
        .orchestrations()
        .upsert_orchestration_task(planned_task(session_orchestration_id, task_key))
        .await
        .expect("failed to insert waiting task");
    assert!(
        database
            .orchestrations()
            .claim_orchestration_task(task_id)
            .await
            .expect("failed to claim waiting task")
    );
    insert_orchestration_child(database, project_id, child_session_id, task_id).await;
    assert!(
        database
            .orchestrations()
            .link_orchestration_task_child(task_id, child_session_id)
            .await
            .expect("failed to link waiting child")
    );
    database
        .orchestrations()
        .update_orchestration_task_status(
            task_id,
            &OrchestrationTaskStatus::WaitingForInput.to_string(),
            None,
        )
        .await
        .expect("failed to wait for child question");

    task_id
}

async fn surface_and_clear_orchestration_questions(
    database: &AppRepositories,
    session_orchestration_id: i64,
    task_id: i64,
    questions: &str,
) -> bool {
    let surfaced = database
        .orchestrations()
        .surface_orchestration_questions(session_orchestration_id, task_id, questions)
        .await
        .expect("failed to surface child question");
    database
        .orchestrations()
        .clear_orchestration_questions(session_orchestration_id)
        .await
        .expect("failed to clear child question");

    surfaced
}

async fn load_detached_campaign_state(
    database: &AppRepositories,
    orchestration_id: i64,
) -> (SessionOrchestrationTaskRow, SessionRow, SessionRow) {
    let task = database
        .orchestrations()
        .load_orchestration_tasks(orchestration_id)
        .await
        .expect("failed to load detached task")
        .remove(0);
    let child = database
        .sessions()
        .load_session("child-alpha")
        .await
        .expect("failed to load detached child")
        .expect("detached child should exist");
    let controller = database
        .sessions()
        .load_session("controller")
        .await
        .expect("failed to load controller")
        .expect("controller should exist");

    (task, child, controller)
}
