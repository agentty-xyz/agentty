use ag_session::{
    AcceptTaskPlanRequest, AgentKind, AgentModel, AgentSelection, SubmitAgentTaskRequest,
    TaskAgentRole,
};

use crate::test_support::{agent_task_request, task_database_fixture};
use crate::{Database, TaskAgentTurnMetadata};

#[tokio::test]
async fn acceptance_uses_exact_response_and_leaves_task_completion_independent() {
    // Arrange
    let database = task_database_fixture().await;
    record_plan(
        &database,
        "plan",
        r#"{"answer":"  Exact plan.\n","questions":[]}"#,
        "done",
    )
    .await;
    let request = acceptance("plan");

    // Act
    let plan = database
        .tasks()
        .accept_agent_plan("session", &request)
        .await
        .expect("accept");
    let repeated = database
        .tasks()
        .accept_agent_plan("session", &request)
        .await
        .expect("repeat");
    let implementer = implementation(plan.clone());
    database
        .tasks()
        .verify_agent_plan("session", &implementer)
        .await
        .expect("verified");
    assert!(
        database
            .tasks()
            .claim_agent_attempt("session", &implementer, "reply")
            .await
            .expect("handoff")
    );

    // Assert
    assert_eq!(plan, repeated);
    assert_eq!(plan.text, "  Exact plan.\n");
    assert!(
        database
            .tasks()
            .accept_agent_plan(
                "session",
                &AcceptTaskPlanRequest {
                    task_id: "implementation".into(),
                    attempt_id: "implement".into(),
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        database
            .tasks()
            .load_task("task")
            .await
            .expect("load")
            .expect("planner")
            .completed_at,
        None
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_plan_acceptance")
        .fetch_one(database.pool())
        .await
        .expect("ledger");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn acceptance_rejects_ineligible_versions_and_responses() {
    // Arrange
    let cases = [
        ("queued", r#"{"answer":"plan","questions":[]}"#),
        ("running", r#"{"answer":"plan","questions":[]}"#),
        ("failed", r#"{"answer":"plan","questions":[]}"#),
        ("canceled", r#"{"answer":"plan","questions":[]}"#),
        ("cancel_requested", r#"{"answer":"plan","questions":[]}"#),
        ("missing_response", r#"{"answer":"plan","questions":[]}"#),
        ("done", "invalid JSON"),
        ("done", r#"{"answer":"  ","questions":[]}"#),
        (
            "done",
            r#"{"answer":"plan","questions":[{"text":"Which?","options":[]}]}"#,
        ),
        (
            "done",
            r#"{"answer":"plan","questions":[],"review_comment_outcomes":[{"reply":"changed","resolution":"fixed","thread_id":"thread"}]}"#,
        ),
    ];
    for (state, response) in cases {
        let database = task_database_fixture().await;
        record_plan(&database, "plan", response, state).await;

        // Act
        let result = database
            .tasks()
            .accept_agent_plan("session", &acceptance("plan"))
            .await;

        // Assert
        assert!(result.is_err(), "{state}: {response}");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_plan_acceptance")
            .fetch_one(database.pool())
            .await
            .expect("ledger");
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn acceptance_requires_same_session_planner_and_latest_attempt() {
    // Arrange
    let database = task_database_fixture().await;
    assert!(
        database
            .tasks()
            .accept_agent_plan("session", &acceptance("missing"))
            .await
            .is_err()
    );
    record_plan(
        &database,
        "first",
        r#"{"answer":"first","questions":[]}"#,
        "done",
    )
    .await;

    // Act and Assert
    assert!(
        database
            .tasks()
            .accept_agent_plan("other", &acceptance("first"))
            .await
            .is_err()
    );
    database
        .tasks()
        .complete_task("task")
        .await
        .expect("generic completion");
    assert!(
        database
            .tasks()
            .verify_agent_plan(
                "session",
                &implementation(ag_session::AcceptedTaskPlan {
                    attempt_id: "first".into(),
                    task_id: "task".into(),
                    text: "first".into()
                })
            )
            .await
            .is_err()
    );
    sqlx::query("UPDATE task SET completed_at = NULL WHERE id = 'task'")
        .execute(database.pool())
        .await
        .expect("allow revision");
    record_plan(
        &database,
        "second",
        r#"{"answer":"second","questions":[]}"#,
        "done",
    )
    .await;
    assert!(
        database
            .tasks()
            .accept_agent_plan("session", &acceptance("first"))
            .await
            .is_err()
    );
    sqlx::query("UPDATE task_agent_context SET role = 'implementer' WHERE task_id = 'task'")
        .execute(database.pool())
        .await
        .expect("invalid source role");
    assert!(
        database
            .tasks()
            .accept_agent_plan("session", &acceptance("second"))
            .await
            .is_err()
    );
    sqlx::query("UPDATE task_agent_context SET role = 'planner' WHERE task_id = 'task'")
        .execute(database.pool())
        .await
        .expect("restore source");
    sqlx::query("DELETE FROM task_attempt WHERE task_id = 'task'")
        .execute(database.pool())
        .await
        .expect("missing version fixture");
    assert!(
        database
            .tasks()
            .accept_agent_plan("session", &acceptance("second"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handoff_rejects_forged_or_stale_inputs_without_partial_admission() {
    // Arrange
    let database = task_database_fixture().await;
    record_plan(
        &database,
        "plan",
        r#"{"answer":"exact","questions":[]}"#,
        "done",
    )
    .await;
    let plan = database
        .tasks()
        .accept_agent_plan("session", &acceptance("plan"))
        .await
        .expect("accept");
    let original = implementation(plan);
    let mut text = original.clone();
    text.input
        .accepted_plan
        .as_mut()
        .expect("plan")
        .text
        .push('!');
    let mut provenance = original.clone();
    provenance
        .input
        .accepted_plan
        .as_mut()
        .expect("plan")
        .attempt_id = "forged".into();
    let mut selection = original.clone();
    selection.assignment.agent = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Astra);
    let mut input = original.clone();
    input.input.user_request.push('!');
    let mut missing = original.clone();
    missing.input.accepted_plan = None;
    for request in [text, provenance, selection, input, missing] {
        // Act
        let verification = database
            .tasks()
            .verify_agent_plan("session", &request)
            .await;
        let admission = database
            .tasks()
            .claim_agent_attempt("session", &request, "reply")
            .await;

        // Assert
        assert!(verification.is_err());
        assert!(admission.is_err());
        assert_eq!(
            database
                .tasks()
                .load_task("implementation")
                .await
                .expect("load"),
            None
        );
    }
    record_plan(
        &database,
        "revision",
        r#"{"answer":"new plan","questions":[]}"#,
        "queued",
    )
    .await;
    assert!(
        database
            .tasks()
            .claim_agent_attempt("session", &original, "reply")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn acceptance_conflicting_ledger_fails_closed_and_transaction_rolls_back() {
    // Arrange
    let database = task_database_fixture().await;
    record_plan(
        &database,
        "plan",
        r#"{"answer":"exact","questions":[]}"#,
        "done",
    )
    .await;
    database
        .tasks()
        .accept_agent_plan("session", &acceptance("plan"))
        .await
        .expect("accept");
    sqlx::query("UPDATE task_plan_acceptance SET plan = 'corrupt'")
        .execute(database.pool())
        .await
        .expect("corruption fixture");

    // Act
    let acceptance = database
        .tasks()
        .accept_agent_plan("session", &acceptance("plan"))
        .await;
    let implementation = implementation(ag_session::AcceptedTaskPlan {
        task_id: "task".into(),
        attempt_id: "plan".into(),
        text: "exact".into(),
    });
    let verification = database
        .tasks()
        .verify_agent_plan("session", &implementation)
        .await;

    // Assert
    assert!(acceptance.is_err());
    assert!(verification.is_err());
}

async fn record_plan(database: &Database, attempt_id: &str, response: &str, state: &str) {
    let request = agent_task_request(attempt_id);
    database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await
        .expect("planner");
    let operation_id = format!("task-attempt:{attempt_id}");
    if state == "queued" {
        return;
    }
    database
        .operations()
        .mark_session_operation_running(&operation_id)
        .await
        .expect("running");
    if state != "missing_response" {
        database
            .tasks()
            .persist_agent_turn(
                attempt_id,
                &TaskAgentTurnMetadata {
                    assistant_message: response.into(),
                    harness_turn_report: None,
                    instruction_conversation_id: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                },
            )
            .await
            .expect("response");
    }
    if state == "running" {
        return;
    }
    sqlx::query("UPDATE session_operation SET status = ?, cancel_requested = ? WHERE id = ?")
        .bind(
            if matches!(state, "cancel_requested" | "missing_response") {
                "done"
            } else {
                state
            },
        )
        .bind(state == "cancel_requested")
        .bind(operation_id)
        .execute(database.pool())
        .await
        .expect("settle fixture");
}

fn acceptance(attempt_id: &str) -> AcceptTaskPlanRequest {
    AcceptTaskPlanRequest {
        attempt_id: attempt_id.into(),
        task_id: "task".into(),
    }
}

fn implementation(plan: ag_session::AcceptedTaskPlan) -> SubmitAgentTaskRequest {
    let mut request = agent_task_request("implement");
    request.assignment.role = TaskAgentRole::Implementer;
    request.assignment.definition =
        ag_session::AgentDefinition::bundled("implementer").expect("bundled implementer");
    request.input.accepted_plan = Some(plan);
    request.task_id = "implementation".into();

    request
}
