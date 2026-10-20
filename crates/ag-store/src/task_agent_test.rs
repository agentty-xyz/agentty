use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, DurableMessageVisibility,
    SubmitAgentTaskRequest, SubmitTaskRequest, Task, TaskAgentInput,
};

use crate::SessionMessageRow;
use crate::test_support::{
    agent_task_request as request, task_database_fixture as database_fixture,
};

#[tokio::test]
async fn role_attempts_keep_one_context_and_preserve_explicit_input() {
    // Arrange
    let database = database_fixture().await;
    let first = request("first");
    let tasks = database.tasks();
    assert!(
        !tasks
            .contains_agent_attempt("session", &first)
            .await
            .expect("new delivery")
    );

    // Act
    assert!(
        tasks
            .claim_agent_attempt("session", &first, "reply")
            .await
            .expect("admission")
    );
    let original = tasks
        .load_agent_context("task")
        .await
        .expect("load")
        .expect("context");
    sqlx::query("UPDATE task_attempt SET message = ? WHERE id = 'first'")
        .bind(serde_json::to_string_pretty(&first.input).expect("formatted old input"))
        .execute(database.pool())
        .await
        .expect("alternate JSON formatting");
    assert!(
        !tasks
            .claim_agent_attempt("session", &first, "reply")
            .await
            .expect("redelivery")
    );
    database
        .operations()
        .mark_session_operation_done("task-attempt:first")
        .await
        .expect("settle");
    let mut continued = request("continued");
    continued.input.clarification = Some("  Keep literal new guidance.\n".to_string());
    assert!(
        tasks
            .claim_agent_attempt("session", &continued, "reply")
            .await
            .expect("continue")
    );
    let retained = tasks
        .load_agent_context("task")
        .await
        .expect("load")
        .expect("context");
    let task = tasks
        .load_task("task")
        .await
        .expect("task")
        .expect("present");

    // Assert
    assert_eq!(retained, original);
    assert_eq!(retained.context.assignment, first.assignment);
    assert_eq!(retained.context.env_id(), task.env_id());
    assert_eq!(retained.input.user_request, first.input.user_request);
    assert_eq!(retained.input.accepted_plan, first.input.accepted_plan);
    assert_eq!(retained.input.clarification, None);
    assert_eq!(retained.context.id.len(), 32);
    let restored: TaskAgentInput = serde_json::from_str(&task.attempts[1].message).expect("input");
    assert_eq!(restored, continued.input);
    assert_eq!(
        database
            .sessions()
            .load_session_messages("session")
            .await
            .expect("transcript"),
        Vec::<SessionMessageRow>::new()
    );
}

#[tokio::test]
async fn conflicting_assignment_or_original_input_cannot_change_a_task() {
    // Arrange
    let database = database_fixture().await;
    let original = request("first");
    database
        .tasks()
        .claim_agent_attempt("session", &original, "reply")
        .await
        .expect("admission");
    let before = database
        .tasks()
        .load_agent_context("task")
        .await
        .expect("context");
    let mut changed_role = original.clone();
    changed_role.assignment.definition = BuiltinAgentDefinition::Planner;
    changed_role.input.accepted_plan = None;
    let mut changed_model = original.clone();
    changed_model.assignment.agent = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Astra);
    let mut changed_input = original.clone();
    changed_input.input.user_request.push_str(" changed");
    let mut changed_plan = original.clone();
    changed_plan
        .input
        .accepted_plan
        .as_mut()
        .expect("plan")
        .text
        .push_str(" changed");
    let mut changed_objective = original.clone();
    changed_objective.objective.push_str(" changed");
    let mut changed_visibility = original.clone();
    changed_visibility.visibility = DurableMessageVisibility::Hidden;

    for candidate in [
        changed_role,
        changed_model,
        changed_input,
        changed_plan,
        changed_objective,
        changed_visibility,
    ] {
        // Act
        let duplicate = database
            .tasks()
            .contains_agent_attempt("session", &candidate)
            .await;
        let admission = database
            .tasks()
            .claim_agent_attempt("session", &candidate, "reply")
            .await;

        // Assert
        assert!(duplicate.is_err());
        assert!(admission.is_err());
    }
    assert!(
        database
            .tasks()
            .contains_agent_attempt("other", &original)
            .await
            .is_err()
    );
    assert!(
        database
            .tasks()
            .claim_agent_attempt("other", &original, "reply")
            .await
            .is_err()
    );
    assert_eq!(
        database
            .tasks()
            .load_agent_context("task")
            .await
            .expect("context"),
        before
    );
}

#[tokio::test]
async fn ordinary_and_role_aware_delivery_cannot_impersonate_each_other() {
    // Arrange
    let database = database_fixture().await;
    let agent = request("agent");
    let ordinary = SubmitTaskRequest {
        attempt_id: agent.attempt_id.clone(),
        message: serde_json::json!(agent.input).to_string(),
        objective: agent.objective.clone(),
        task_id: agent.task_id.clone(),
        visibility: agent.visibility,
    };
    database
        .tasks()
        .claim_agent_attempt("session", &agent, "reply")
        .await
        .expect("agent");

    // Act
    let ordinary_check = database
        .tasks()
        .contains_attempt("session", &ordinary)
        .await;
    let ordinary_claim = database
        .tasks()
        .claim_attempt("session", &ordinary, "reply")
        .await;

    // Assert
    assert!(ordinary_check.is_err());
    assert!(ordinary_claim.is_err());

    // Arrange
    let database = database_fixture().await;
    database
        .tasks()
        .claim_attempt("session", &ordinary, "reply")
        .await
        .expect("ordinary");
    database
        .operations()
        .mark_session_operation_done("task-attempt:agent")
        .await
        .expect("settle");

    // Act
    let agent_check = database
        .tasks()
        .contains_agent_attempt("session", &agent)
        .await;
    let agent_claim = database
        .tasks()
        .claim_agent_attempt("session", &request("new"), "reply")
        .await;

    // Assert
    assert!(agent_check.is_err());
    assert!(agent_claim.is_err());
    assert_eq!(
        database
            .tasks()
            .load_agent_context("task")
            .await
            .expect("context"),
        None
    );
}

#[tokio::test]
async fn failed_admission_rolls_back_task_assignment_and_context() {
    // Arrange
    let database = database_fixture().await;
    database
        .operations()
        .insert_session_operation("task-attempt:first", "session", "reply")
        .await
        .expect("collision");

    // Act
    let result = database
        .tasks()
        .claim_agent_attempt("session", &request("first"), "reply")
        .await;

    // Assert
    assert!(result.is_err());
    assert_eq!(
        database.tasks().load_task("task").await.expect("task"),
        None
    );
    assert_eq!(
        database
            .tasks()
            .load_agent_context("task")
            .await
            .expect("context"),
        None
    );
}

#[tokio::test]
async fn invalid_role_input_is_rejected_before_storage() {
    // Arrange
    let database = database_fixture().await;
    let mut request = request("invalid");
    request.input.user_request = " \n".to_string();

    // Act
    let claim = database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await;
    let lookup = database
        .tasks()
        .contains_agent_attempt("session", &request)
        .await;

    // Assert
    assert!(claim.is_err());
    assert!(lookup.is_err());
    assert_eq!(
        database
            .tasks()
            .list_tasks("session")
            .await
            .expect("no tasks"),
        Vec::<Task>::new()
    );
}

#[tokio::test]
async fn malformed_persisted_assignments_fail_closed() {
    // Arrange
    let database = database_fixture().await;
    let request = request("first");
    database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await
        .expect("admit");
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(database.pool())
        .await
        .expect("corruption fixture");
    let valid_input = serde_json::json!({
        "accepted_plan": request.input.accepted_plan,
        "clarification": null,
        "user_request": ""
    })
    .to_string();
    let mut immutable_input = request.input.clone();
    immutable_input.clarification = None;
    let original_input = serde_json::json!(immutable_input).to_string();
    let cases = [
        (
            "UPDATE task_agent_context SET role = ? WHERE task_id = 'task'",
            "unknown",
            "implementer",
        ),
        (
            "UPDATE task_agent_context SET agent = ? WHERE task_id = 'task'",
            "unknown",
            "codex",
        ),
        (
            "UPDATE task_agent_context SET model = ? WHERE task_id = 'task'",
            "unknown",
            "gpt-6.1-sol",
        ),
        (
            "UPDATE task_agent_context SET input = ? WHERE task_id = 'task'",
            "not json",
            original_input.as_str(),
        ),
        (
            "UPDATE task_agent_context SET input = ? WHERE task_id = 'task'",
            valid_input.as_str(),
            original_input.as_str(),
        ),
    ];
    for (update, invalid, restore) in cases {
        sqlx::query(update)
            .bind(invalid)
            .execute(database.pool())
            .await
            .expect("corrupt row");

        // Act
        let result = database.tasks().load_agent_context("task").await;

        // Assert
        assert!(result.is_err(), "{update}: {invalid}");
        sqlx::query(update)
            .bind(restore)
            .execute(database.pool())
            .await
            .expect("restore row");
    }
}

#[tokio::test]
async fn malformed_attempt_input_cannot_be_acknowledged_as_an_identical_delivery() {
    // Arrange
    let database = database_fixture().await;
    let request = request("first");
    database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await
        .expect("admit");
    sqlx::query("UPDATE task_attempt SET message = 'not json' WHERE id = 'first'")
        .execute(database.pool())
        .await
        .expect("corrupt attempt");

    // Act
    let lookup = database
        .tasks()
        .contains_agent_attempt("session", &request)
        .await;
    let claim = database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await;

    // Assert
    assert!(lookup.is_err());
    assert!(claim.is_err());
}

#[tokio::test]
async fn independent_planner_tasks_get_distinct_contexts_and_delete_with_the_session() {
    // Arrange
    let database = database_fixture().await;
    let mut first = request("plan-first");
    first.assignment.definition = BuiltinAgentDefinition::Planner;
    first.input.accepted_plan = None;
    let second = SubmitAgentTaskRequest {
        task_id: "other-task".to_string(),
        attempt_id: "plan-second".to_string(),
        ..first.clone()
    };
    for request in [&first, &second] {
        database
            .tasks()
            .claim_agent_attempt("session", request, "reply")
            .await
            .expect("planner");
    }

    // Act
    let first_context = database
        .tasks()
        .load_agent_context(&first.task_id)
        .await
        .expect("load")
        .expect("context");
    let second_context = database
        .tasks()
        .load_agent_context(&second.task_id)
        .await
        .expect("load")
        .expect("context");
    database
        .sessions()
        .delete_session("session")
        .await
        .expect("delete");

    // Assert
    assert_ne!(first_context.context.id, second_context.context.id);
    assert_eq!(
        first_context.context.env_id(),
        second_context.context.env_id()
    );
    assert_eq!(
        database
            .tasks()
            .load_agent_context(&first.task_id)
            .await
            .expect("deleted"),
        None
    );
}
