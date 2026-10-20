use ag_session::{
    AcceptTaskChainPlanRequest, AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition,
    ContinueTaskChainRequest, CreateTaskRequest, StartPlanThenImplementRequest, SubmitTaskRequest,
    TaskChain, TaskChainPhase,
};

use super::TaskChainStore;
use crate::test_support::task_database_fixture;
use crate::{Database, TaskAgentTurnMetadata};

#[tokio::test]
async fn workflow_admits_two_isolated_roles_only_after_explicit_acceptance() {
    // Arrange
    let database = task_database_fixture().await;
    let request = start_request();
    assert_eq!(
        database
            .tasks()
            .load_task_chain("missing")
            .await
            .expect("missing"),
        None
    );

    // Act
    assert!(
        database
            .tasks()
            .start_task_chain("session", &request, "reply")
            .await
            .expect("start")
    );
    assert!(
        !database
            .tasks()
            .start_task_chain("session", &request, "reply")
            .await
            .expect("redelivery")
    );
    let planning = chain(&database).await;
    response(
        &database,
        "plan",
        r#"{"answer":"  Exact plan.\n","questions":[]}"#,
        false,
    )
    .await;
    assert_eq!(chain(&database).await.phase, TaskChainPhase::Planning);
    database
        .operations()
        .mark_session_operation_done("task-attempt:plan")
        .await
        .expect("settle planner");
    let awaiting = chain(&database).await;
    assert!(
        database
            .tasks()
            .accept_task_chain_plan("chain", &accept_request(), "reply")
            .await
            .expect("accept and admit")
    );
    let implementing = chain(&database).await;
    response(
        &database,
        "implement",
        r#"{"answer":"Implemented.","questions":[]}"#,
        true,
    )
    .await;

    // Assert
    assert_eq!(planning.phase, TaskChainPhase::Planning);
    assert_eq!(awaiting.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(implementing.phase, TaskChainPhase::Implementing);
    assert_eq!(
        implementing.accepted_plan.as_ref().expect("accepted").text,
        "  Exact plan.\n"
    );
    assert_eq!(
        implementing.current_request.assignment.definition,
        BuiltinAgentDefinition::Implementer
    );
    assert_eq!(implementing.current_request.assignment.agent, request.agent);
    assert_eq!(chain(&database).await.phase, TaskChainPhase::Succeeded);
    assert_ne!(
        planning.current_request.task_id,
        implementing.current_request.task_id
    );
    assert_role_contexts(&database, &planning).await;
    assert_eq!(
        database
            .tasks()
            .list_task_chains("session")
            .await
            .expect("list")
            .len(),
        1
    );
    assert_eq!(
        database
            .tasks()
            .list_task_chains("other")
            .await
            .expect("other")
            .len(),
        0
    );
    assert_eq!(
        database
            .tasks()
            .load_task(&planning.planner_task_id)
            .await
            .expect("load")
            .expect("task")
            .completed_at,
        None
    );
}

async fn assert_role_contexts(database: &Database, planning: &TaskChain) {
    let planner_context = database
        .tasks()
        .load_agent_context(&planning.planner_task_id)
        .await
        .expect("load")
        .expect("context");
    let implementer_context = database
        .tasks()
        .load_agent_context(&planning.implementer_task_id)
        .await
        .expect("load")
        .expect("context");
    assert_ne!(planner_context.context.id, implementer_context.context.id);
    assert_eq!(
        planner_context.context.env_id(),
        implementer_context.context.env_id()
    );
}

#[tokio::test]
async fn historical_receipts_are_idempotent_after_stage_changes_and_later_retries() {
    // Arrange
    let database = task_database_fixture().await;
    let start = start_request();
    let revision = revised_plan(&database).await;
    let accepted = AcceptTaskChainPlanRequest {
        plan_attempt_id: "revision".into(),
        ..accept_request()
    };
    database
        .tasks()
        .accept_task_chain_plan("chain", &accepted, "reply")
        .await
        .expect("handoff");
    response(
        &database,
        "implement",
        r#"{"answer":"","questions":[{"text":"Which file?","options":[]}]}"#,
        true,
    )
    .await;
    assert_eq!(
        chain(&database).await.phase,
        TaskChainPhase::ImplementerQuestion
    );
    let retry = ContinueTaskChainRequest {
        attempt_id: "retry".into(),
        clarification: Some("Use file A".into()),
    };
    database
        .tasks()
        .continue_task_chain("chain", &retry, "reply")
        .await
        .expect("continue implementer");

    // Act
    let launch_again = database
        .tasks()
        .start_task_chain("session", &start, "reply")
        .await
        .expect("launch repeat");
    let old_planner = database
        .tasks()
        .continue_task_chain("chain", &revision, "reply")
        .await
        .expect("old Planner receipt");
    let old_acceptance = database
        .tasks()
        .accept_task_chain_plan("chain", &accepted, "reply")
        .await
        .expect("old acceptance receipt");
    let retry_again = database
        .tasks()
        .continue_task_chain("chain", &retry, "reply")
        .await
        .expect("repeat retry");

    // Assert
    assert!(!launch_again && !old_planner && !old_acceptance && !retry_again);
    assert_eq!(chain(&database).await.current_request.attempt_id, "retry");
    assert!(
        database
            .tasks()
            .continue_task_chain(
                "chain",
                &ContinueTaskChainRequest {
                    clarification: None,
                    ..revision
                },
                "reply"
            )
            .await
            .is_err()
    );
    assert!(
        database
            .tasks()
            .accept_task_chain_plan(
                "chain",
                &AcceptTaskChainPlanRequest {
                    attempt_id: "new-handoff".into(),
                    ..accepted.clone()
                },
                "reply"
            )
            .await
            .is_err()
    );
    assert!(
        database
            .tasks()
            .accept_task_chain_plan(
                "chain",
                &AcceptTaskChainPlanRequest {
                    plan_attempt_id: "plan".into(),
                    ..accepted
                },
                "reply"
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_is_scoped_and_requires_a_new_explicit_delivery_to_resume() {
    // Arrange
    let database = task_database_fixture().await;
    let request = start_request();
    database
        .tasks()
        .start_task_chain("session", &request, "reply")
        .await
        .expect("start");
    database
        .tasks()
        .claim_attempt(
            "session",
            &SubmitTaskRequest {
                attempt_id: "ordinary".into(),
                message: "unrelated".into(),
                objective: "unrelated".into(),
                task_id: "ordinary".into(),
                visibility: ag_session::DurableMessageVisibility::Hidden,
            },
            "reply",
        )
        .await
        .expect("ordinary queued");

    // Act
    database
        .tasks()
        .cancel_task_chain("chain")
        .await
        .expect("cancel");
    database
        .tasks()
        .cancel_task_chain("chain")
        .await
        .expect("repeat cancel");
    assert!(
        !database
            .tasks()
            .start_task_chain("session", &request, "reply")
            .await
            .expect("redelivery")
    );
    assert!(
        !database
            .tasks()
            .continue_task_chain(
                "chain",
                &ContinueTaskChainRequest {
                    attempt_id: "plan".into(),
                    clarification: None
                },
                "reply"
            )
            .await
            .expect("old receipt")
    );

    // Assert
    assert_eq!(chain(&database).await.phase, TaskChainPhase::Canceled);
    assert!(
        database
            .tasks()
            .accept_task_chain_plan("chain", &accept_request(), "reply")
            .await
            .is_err()
    );
    let statuses: Vec<(String, String, bool)> =
        sqlx::query_as("SELECT id, status, cancel_requested FROM session_operation ORDER BY id")
            .fetch_all(database.pool())
            .await
            .expect("operations");
    assert_eq!(
        statuses,
        [
            ("task-attempt:ordinary".into(), "queued".into(), false),
            ("task-attempt:plan".into(), "canceled".into(), true)
        ]
    );
    assert!(
        database
            .tasks()
            .continue_task_chain(
                "chain",
                &ContinueTaskChainRequest {
                    attempt_id: "explicit-retry".into(),
                    clarification: Some("Retry".into())
                },
                "reply"
            )
            .await
            .expect("explicit retry")
    );
    assert_eq!(chain(&database).await.phase, TaskChainPhase::Planning);
}

#[tokio::test]
async fn running_cancellation_must_settle_before_a_new_attempt() {
    // Arrange
    let database = task_database_fixture().await;
    database
        .tasks()
        .start_task_chain("session", &start_request(), "reply")
        .await
        .expect("start");
    database
        .operations()
        .mark_session_operation_running("task-attempt:plan")
        .await
        .expect("running");
    // Act
    database
        .tasks()
        .cancel_task_chain("chain")
        .await
        .expect("cancel running");
    // Assert
    assert!(
        database
            .tasks()
            .continue_task_chain(
                "chain",
                &ContinueTaskChainRequest {
                    attempt_id: "too-soon".into(),
                    clarification: None
                },
                "reply"
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handoff_failure_rolls_back_acceptance_stage_and_attempt_together() {
    // Arrange
    let database = task_database_fixture().await;
    database
        .tasks()
        .start_task_chain("session", &start_request(), "reply")
        .await
        .expect("start");
    response(
        &database,
        "plan",
        r#"{"answer":"Plan","questions":[]}"#,
        true,
    )
    .await;
    sqlx::query(
        "CREATE TRIGGER reject_handoff BEFORE INSERT ON task_attempt WHEN NEW.id = 'implement' \
         BEGIN SELECT RAISE(ABORT,'handoff failed'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure injection");

    // Act
    let failed = database
        .tasks()
        .accept_task_chain_plan("chain", &accept_request(), "reply")
        .await;

    // Assert
    assert!(failed.is_err());
    assert_eq!(
        chain(&database).await.phase,
        TaskChainPhase::AwaitingPlanAcceptance
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_plan_acceptance")
        .fetch_one(database.pool())
        .await
        .expect("ledger");
    assert_eq!(count, 0);
    assert_eq!(
        database
            .tasks()
            .load_agent_context("chain:chain:implementer")
            .await
            .expect("context"),
        None
    );
    sqlx::query("DROP TRIGGER reject_handoff")
        .execute(database.pool())
        .await
        .expect("restore");
    assert!(
        database
            .tasks()
            .accept_task_chain_plan("chain", &accept_request(), "reply")
            .await
            .expect("retry exact handoff")
    );
}

#[tokio::test]
async fn launch_conflicts_and_reserved_task_bypasses_are_rejected() {
    // Arrange
    let database = task_database_fixture().await;
    let original = start_request();
    database
        .tasks()
        .start_task_chain("session", &original, "reply")
        .await
        .expect("start");
    let changes = [
        StartPlanThenImplementRequest {
            user_request: "changed".into(),
            ..original.clone()
        },
        StartPlanThenImplementRequest {
            planner_attempt_id: "changed".into(),
            ..original.clone()
        },
        StartPlanThenImplementRequest {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt6Astra),
            ..original.clone()
        },
    ];

    // Act and Assert
    for changed in changes {
        assert!(
            database
                .tasks()
                .start_task_chain("session", &changed, "reply")
                .await
                .is_err()
        );
    }
    assert!(
        database
            .tasks()
            .start_task_chain("other", &original, "reply")
            .await
            .is_err()
    );
    assert!(
        database
            .tasks()
            .start_task_chain(
                "session",
                &StartPlanThenImplementRequest {
                    chain_id: "  ".into(),
                    ..original
                },
                "reply"
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn workflow_tasks_cannot_be_admitted_outside_the_chain() {
    // Arrange
    let database = task_database_fixture().await;
    database
        .tasks()
        .start_task_chain("session", &start_request(), "reply")
        .await
        .expect("start");

    // Act and Assert
    let mut bypass = chain(&database).await.current_request;
    bypass.attempt_id = "bypass".into();
    assert!(
        database
            .tasks()
            .claim_agent_attempt("session", &bypass, "reply")
            .await
            .is_err()
    );
    let ordinary = SubmitTaskRequest {
        attempt_id: "ordinary-bypass".into(),
        message: "unrelated".into(),
        objective: bypass.objective,
        task_id: "chain:chain:implementer".into(),
        visibility: ag_session::DurableMessageVisibility::Hidden,
    };
    assert!(
        database
            .tasks()
            .claim_attempt("session", &ordinary, "reply")
            .await
            .is_err()
    );
    let occupied = StartPlanThenImplementRequest {
        chain_id: "occupied".into(),
        planner_attempt_id: "occupied-plan".into(),
        ..start_request()
    };
    database
        .tasks()
        .create_task(
            "session",
            &CreateTaskRequest {
                objective: occupied.user_request.clone(),
                task_id: occupied.implementer_task_id(),
            },
        )
        .await
        .expect("reserved collision fixture");
    assert!(
        database
            .tasks()
            .start_task_chain("session", &occupied, "reply")
            .await
            .is_err()
    );
    assert!(
        database
            .tasks()
            .accept_task_chain_plan("missing", &accept_request(), "reply")
            .await
            .is_err()
    );
    assert!(
        database
            .tasks()
            .continue_task_chain(
                "missing",
                &ContinueTaskChainRequest {
                    attempt_id: "missing".into(),
                    clarification: None
                },
                "reply"
            )
            .await
            .is_err()
    );
    assert!(database.tasks().cancel_task_chain("missing").await.is_err());
}

#[test]
fn phases_use_terminal_operation_state_and_never_infer_plan_acceptance() {
    // Arrange
    let cases = [
        (
            BuiltinAgentDefinition::Planner,
            "queued",
            false,
            None,
            TaskChainPhase::Planning,
        ),
        (
            BuiltinAgentDefinition::Implementer,
            "running",
            false,
            None,
            TaskChainPhase::Implementing,
        ),
        (
            BuiltinAgentDefinition::Planner,
            "failed",
            false,
            None,
            TaskChainPhase::Failed,
        ),
        (
            BuiltinAgentDefinition::Planner,
            "canceled",
            false,
            None,
            TaskChainPhase::Canceled,
        ),
        (
            BuiltinAgentDefinition::Implementer,
            "done",
            true,
            None,
            TaskChainPhase::Canceled,
        ),
        (
            BuiltinAgentDefinition::Planner,
            "done",
            false,
            Some(r#"{"answer":"","questions":[]}"#),
            TaskChainPhase::Failed,
        ),
        (
            BuiltinAgentDefinition::Planner,
            "done",
            false,
            Some(
                r#"{"answer":"plan","questions":[],"review_comment_outcomes":[{"reply":"done","resolution":"fixed","thread_id":"thread"}]}"#,
            ),
            TaskChainPhase::Failed,
        ),
    ];
    for (role, status, canceled, response, expected) in cases {
        // Act and Assert
        assert_eq!(
            TaskChainStore::phase(role, status, canceled, response).expect("phase"),
            expected
        );
    }
    for (status, response) in [
        ("done", None),
        ("done", Some("bad JSON")),
        ("unknown", None),
    ] {
        assert!(
            TaskChainStore::phase(BuiltinAgentDefinition::Planner, status, false, response)
                .is_err()
        );
    }
}

#[tokio::test]
async fn corrupted_workflow_snapshots_and_historical_receipts_fail_closed() {
    // Arrange
    let changes = [
        "UPDATE task_chain SET agent = 'unknown'",
        "UPDATE task_chain SET model = 'unknown'",
        "DELETE FROM task_plan_acceptance",
        "DELETE FROM task_agent_context WHERE task_id = 'chain:chain:implementer'",
        "UPDATE task_chain SET user_request = 'changed'",
        "UPDATE task_attempt SET message = 'invalid' WHERE id = 'implement'",
        "UPDATE task_attempt SET message = json_set(message, '$.user_request', 'changed') WHERE \
         id = 'implement'",
        "UPDATE task_attempt SET is_visible = 1 WHERE id = 'implement'",
        "DELETE FROM task_attempt WHERE id = 'implement'",
    ];
    for change in changes {
        let database = accepted_database().await;
        sqlx::query(change)
            .execute(database.pool())
            .await
            .expect("corruption fixture");

        // Act
        let snapshot = database.tasks().load_task_chain("chain").await;

        // Assert
        assert!(snapshot.is_err(), "{change}");
    }
    let database = accepted_database().await;
    sqlx::query("UPDATE task_attempt SET message = 'invalid' WHERE id = 'plan'")
        .execute(database.pool())
        .await
        .expect("corrupt receipt");
    assert!(
        database
            .tasks()
            .continue_task_chain(
                "chain",
                &ContinueTaskChainRequest {
                    attempt_id: "plan".into(),
                    clarification: None
                },
                "reply"
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn launch_failure_rolls_back_both_reserved_tasks_and_operation() {
    // Arrange
    let database = task_database_fixture().await;
    sqlx::query(
        "CREATE TRIGGER reject_chain BEFORE INSERT ON task_chain BEGIN SELECT RAISE(ABORT,'launch \
         failed'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure injection");

    // Act
    let result = database
        .tasks()
        .start_task_chain("session", &start_request(), "reply")
        .await;

    // Assert
    assert!(result.is_err());
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM task), (SELECT COUNT(*) FROM task_agent_context), (SELECT \
         COUNT(*) FROM session_operation)",
    )
    .fetch_one(database.pool())
    .await
    .expect("counts");
    assert_eq!(counts, (0, 0, 0));
}

async fn response(database: &Database, attempt_id: &str, text: &str, settled: bool) {
    let operation_id = format!("task-attempt:{attempt_id}");
    database
        .operations()
        .mark_session_operation_running(&operation_id)
        .await
        .expect("running");
    database
        .tasks()
        .persist_agent_turn(
            attempt_id,
            &TaskAgentTurnMetadata {
                assistant_message: text.into(),
                harness_turn_report: None,
                instruction_conversation_id: None,
                input_tokens: 0,
                output_tokens: 0,
                provider_conversation_id: None,
            },
        )
        .await
        .expect("response");
    if settled {
        database
            .operations()
            .mark_session_operation_done(&operation_id)
            .await
            .expect("done");
    }
}

async fn chain(database: &Database) -> TaskChain {
    database
        .tasks()
        .load_task_chain("chain")
        .await
        .expect("chain snapshot")
        .expect("chain")
}

fn start_request() -> StartPlanThenImplementRequest {
    StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "chain".into(),
        planner_attempt_id: "plan".into(),
        user_request: "  Original objective.\n".into(),
    }
}

async fn accepted_database() -> Database {
    let database = task_database_fixture().await;
    database
        .tasks()
        .start_task_chain("session", &start_request(), "reply")
        .await
        .expect("start");
    response(
        &database,
        "plan",
        r#"{"answer":"Plan","questions":[]}"#,
        true,
    )
    .await;
    database
        .tasks()
        .accept_task_chain_plan("chain", &accept_request(), "reply")
        .await
        .expect("accept");

    database
}

fn accept_request() -> AcceptTaskChainPlanRequest {
    AcceptTaskChainPlanRequest {
        attempt_id: "implement".into(),
        plan_attempt_id: "plan".into(),
    }
}

async fn revised_plan(database: &Database) -> ContinueTaskChainRequest {
    database
        .tasks()
        .start_task_chain("session", &start_request(), "reply")
        .await
        .expect("start");
    response(
        database,
        "plan",
        r#"{"answer":"Plan?","questions":[{"text":"Which?","options":[]}]}"#,
        true,
    )
    .await;
    assert_eq!(chain(database).await.phase, TaskChainPhase::PlannerQuestion);
    let revision = ContinueTaskChainRequest {
        attempt_id: "revision".into(),
        clarification: Some("Use option one".into()),
    };
    database
        .tasks()
        .continue_task_chain("chain", &revision, "reply")
        .await
        .expect("revision");
    response(
        database,
        "revision",
        r#"{"answer":"Revised plan","questions":[]}"#,
        true,
    )
    .await;

    revision
}
