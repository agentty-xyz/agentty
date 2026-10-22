use ag_session::{
    StartPlanThenImplementRequest, SubmitAgentTaskRequest, SubmitTaskRequest, TaskAttemptStatus,
};

use crate::test_support::{agent_task_request, task_database_fixture};
use crate::{Database, TaskAgentTurnMetadata};

#[tokio::test]
async fn chain_projection_rejects_invalid_responses_or_roles_and_omits_empty_display_text() {
    for (assistant_message, role, succeeds) in [
        ("not a structured response", "planner", false),
        (r#"{"answer":"A plan","questions":[]}"#, "unknown", false),
        (r#"{"answer":"  ","questions":[]}"#, "planner", true),
    ] {
        // Arrange
        let database = task_database_fixture().await;
        let base = agent_task_request("first");
        let launch = StartPlanThenImplementRequest {
            agents: Box::new(
                ag_session::PlanThenImplementAgents::bundled().expect("bundled agents"),
            ),
            attachments: Vec::new(),
            agent: base.assignment.agent,
            chain_id: "chain".into(),
            planner_attempt_id: "first".into(),
            user_request: base.input.user_request,
        };
        database
            .tasks()
            .start_task_chain("session", &launch, "agent_task")
            .await
            .expect("launch");
        database
            .operations()
            .mark_session_operation_running("task-attempt:first")
            .await
            .expect("running");
        let task_id = launch.planner_request().task_id;
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(database.pool())
            .await
            .expect("corruption fixture");
        sqlx::query("UPDATE task_agent_context SET role = ? WHERE task_id = ?")
            .bind(role)
            .bind(&task_id)
            .execute(database.pool())
            .await
            .expect("stored role");
        let metadata = TaskAgentTurnMetadata {
            assistant_message: assistant_message.into(),
            ..metadata("first")
        };

        // Act
        let result = database
            .tasks()
            .persist_agent_turn("first", &metadata)
            .await;

        // Assert
        assert_eq!(result.is_ok(), succeeds, "{assistant_message}");
        sqlx::query("UPDATE task_agent_context SET role = 'planner' WHERE task_id = ?")
            .bind(&task_id)
            .execute(database.pool())
            .await
            .expect("restore role");
        let history = database
            .tasks()
            .load_agent_history(&task_id)
            .await
            .expect("history")
            .expect("context");
        assert_eq!(history.turns.len(), usize::from(succeeds));
        assert_eq!(
            history.provider_conversation_id,
            if succeeds {
                metadata.provider_conversation_id.clone()
            } else {
                None
            }
        );
        let invocation_count: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(invocation_count), 0) FROM session_usage")
                .fetch_one(database.pool())
                .await
                .expect("usage count");
        assert_eq!(invocation_count, i64::from(succeeds));
        assert_eq!(
            database
                .sessions()
                .load_session_messages("session")
                .await
                .expect("messages")
                .len(),
            2
        );
    }
}

#[tokio::test]
async fn task_response_preserves_ordinary_conversation_and_uses_captured_model() {
    // Arrange
    let database = task_database_fixture().await;
    running_attempt(&database, "first").await;
    sqlx::query(
        "UPDATE session SET model = 'gpt-6-astra', provider_conversation_id = 'chat-thread', \
         app_server_instruction_provider_conversation_id = 'chat-bootstrap', questions = \
         'ordinary questions' WHERE id = 'session'",
    )
    .execute(database.pool())
    .await
    .expect("ordinary conversation");
    let before = ordinary_conversation(&database).await;
    let metadata = metadata("first");

    // Act
    database
        .tasks()
        .persist_agent_turn("first", &metadata)
        .await
        .expect("response");
    let history = database
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history")
        .expect("assigned task");
    let task = database
        .tasks()
        .load_task("task")
        .await
        .expect("task")
        .expect("present");

    // Assert
    assert_eq!(ordinary_conversation(&database).await, before);
    assert_eq!(
        history.provider_conversation_id,
        metadata.provider_conversation_id
    );
    assert_eq!(
        history.instruction_conversation_id,
        metadata.instruction_conversation_id
    );
    assert_eq!(history.turns.len(), 1);
    assert_eq!(history.turns[0].attempt_id, "first");
    assert_eq!(
        history.turns[0].assistant_message,
        metadata.assistant_message
    );
    assert_eq!(history.turns[0].input, agent_task_request("first").input);
    assert_eq!(
        (
            history.turns[0].input_tokens,
            history.turns[0].output_tokens
        ),
        (7, 3)
    );
    assert_eq!(task.completed_at, None);
    assert_eq!(task.attempts[0].status, TaskAttemptStatus::Running);
    assert_eq!(usage(&database).await, (7, 3, 1, "gpt-6.1-sol".to_string()));
    let reports: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_turn_report")
        .fetch_one(database.pool())
        .await
        .expect("reports");
    assert_eq!(reports, 1);
    assert_eq!(
        database
            .tasks()
            .load_agent_history("missing")
            .await
            .expect("missing"),
        None
    );
}

#[tokio::test]
async fn late_identical_response_never_rewinds_continuation_or_duplicates_usage() {
    // Arrange
    let database = task_database_fixture().await;
    running_attempt(&database, "first").await;
    let first = metadata("first");
    let first_recorded = database
        .tasks()
        .persist_agent_turn("first", &first)
        .await
        .expect("first response");
    database
        .operations()
        .mark_session_operation_done("task-attempt:first")
        .await
        .expect("settle first");
    running_attempt(&database, "second").await;
    let second = TaskAgentTurnMetadata {
        harness_turn_report: None,
        input_tokens: 0,
        instruction_conversation_id: None,
        output_tokens: 0,
        ..metadata("second")
    };
    database
        .tasks()
        .persist_agent_turn("second", &second)
        .await
        .expect("second response");

    // Act
    let duplicate_recorded = database
        .tasks()
        .persist_agent_turn("first", &first)
        .await
        .expect("late duplicate");
    let history = database
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history")
        .expect("context");

    // Assert
    assert!(first_recorded);
    assert!(!duplicate_recorded);
    assert_eq!(
        history.provider_conversation_id,
        second.provider_conversation_id
    );
    assert_eq!(history.instruction_conversation_id, None);
    assert_eq!(
        history
            .turns
            .iter()
            .map(|turn| turn.attempt_id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(usage(&database).await, (7, 3, 1, "gpt-6.1-sol".to_string()));
    let reports: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM harness_turn_report")
        .fetch_one(database.pool())
        .await
        .expect("reports");
    assert_eq!(reports, 1);
}

#[tokio::test]
async fn conflicting_response_cannot_replace_observed_output_or_provider_state() {
    // Arrange
    let database = task_database_fixture().await;
    running_attempt(&database, "first").await;
    let original = metadata("first");
    database
        .tasks()
        .persist_agent_turn("first", &original)
        .await
        .expect("response");
    let before = database
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history");
    let changes = [
        TaskAgentTurnMetadata {
            assistant_message: "changed".to_string(),
            ..original.clone()
        },
        TaskAgentTurnMetadata {
            provider_conversation_id: None,
            ..original.clone()
        },
        TaskAgentTurnMetadata {
            instruction_conversation_id: None,
            ..original.clone()
        },
        TaskAgentTurnMetadata {
            input_tokens: 9,
            ..original.clone()
        },
        TaskAgentTurnMetadata {
            harness_turn_report: None,
            ..original
        },
    ];
    for changed in changes {
        // Act
        let result = database.tasks().persist_agent_turn("first", &changed).await;

        // Assert
        assert!(result.is_err());
    }
    assert_eq!(
        database
            .tasks()
            .load_agent_history("task")
            .await
            .expect("history"),
        before
    );
    assert_eq!(usage(&database).await, (7, 3, 1, "gpt-6.1-sol".to_string()));
}

#[tokio::test]
async fn response_requires_a_running_uncanceled_role_attempt() {
    // Arrange
    for state in ["missing", "queued", "ordinary", "canceled", "finished"] {
        let database = task_database_fixture().await;
        let request = agent_task_request("first");
        if state == "ordinary" {
            database
                .tasks()
                .claim_attempt(
                    "session",
                    &SubmitTaskRequest {
                        attempt_id: request.attempt_id.clone(),
                        message: "ordinary input".to_string(),
                        objective: request.objective.clone(),
                        task_id: request.task_id.clone(),
                        visibility: request.visibility,
                    },
                    "reply",
                )
                .await
                .expect("ordinary task");
            database
                .operations()
                .mark_session_operation_running("task-attempt:first")
                .await
                .expect("running");
        } else if state == "queued" {
            database
                .tasks()
                .claim_agent_attempt("session", &request, "reply")
                .await
                .expect("queued");
        } else if state != "missing" {
            running_attempt(&database, "first").await;
            if state == "canceled" {
                database
                    .operations()
                    .request_cancel_for_session_operations("session")
                    .await
                    .expect("cancel");
            } else {
                database
                    .operations()
                    .mark_session_operation_done("task-attempt:first")
                    .await
                    .expect("finished");
            }
        }

        // Act
        let result = database
            .tasks()
            .persist_agent_turn("first", &metadata("first"))
            .await;

        // Assert
        assert!(result.is_err(), "{state}");
        let responses: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_agent_turn")
            .fetch_one(database.pool())
            .await
            .expect("responses");
        assert_eq!(responses, 0, "{state}");
    }
}

#[tokio::test]
async fn invalid_response_metadata_is_rejected_without_storage() {
    // Arrange
    let database = task_database_fixture().await;
    running_attempt(&database, "first").await;
    let valid = metadata("first");
    let invalid = [
        TaskAgentTurnMetadata {
            assistant_message: " \n".to_string(),
            ..valid.clone()
        },
        TaskAgentTurnMetadata {
            input_tokens: u64::MAX,
            ..valid.clone()
        },
        TaskAgentTurnMetadata {
            output_tokens: u64::MAX,
            ..valid
        },
    ];
    for metadata in invalid {
        // Act
        let result = database
            .tasks()
            .persist_agent_turn("first", &metadata)
            .await;

        // Assert
        assert!(result.is_err());
    }
    let history = database
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history")
        .expect("context");
    assert_eq!(history.turns.len(), 0);
    assert_eq!(history.provider_conversation_id, None);
}

#[tokio::test]
async fn failed_usage_write_rolls_back_response_continuation_and_counters() {
    // Arrange
    let database = task_database_fixture().await;
    running_attempt(&database, "first").await;
    sqlx::query(
        "CREATE TRIGGER reject_task_usage BEFORE INSERT ON session_usage BEGIN SELECT \
         RAISE(ABORT, 'usage write failed'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure injection");
    let metadata = metadata("first");

    // Act
    let result = database
        .tasks()
        .persist_agent_turn("first", &metadata)
        .await;
    let history = database
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history")
        .expect("context");

    // Assert
    assert!(result.is_err());
    assert_eq!(history.turns.len(), 0);
    assert_eq!(history.provider_conversation_id, None);
    let counts: (i64, i64) =
        sqlx::query_as("SELECT input_tokens, output_tokens FROM session WHERE id = 'session'")
            .fetch_one(database.pool())
            .await
            .expect("session counters");
    assert_eq!(counts, (0, 0));

    // Act
    sqlx::query("DROP TRIGGER reject_task_usage")
        .execute(database.pool())
        .await
        .expect("restore writes");
    database
        .tasks()
        .persist_agent_turn("first", &metadata)
        .await
        .expect("retry persistence");

    // Assert
    assert_eq!(usage(&database).await, (7, 3, 1, "gpt-6.1-sol".to_string()));
}

#[tokio::test]
async fn corrupted_task_replay_and_usage_fail_closed() {
    // Arrange
    let database = task_database_fixture().await;
    running_attempt(&database, "first").await;
    let metadata = metadata("first");
    database
        .tasks()
        .persist_agent_turn("first", &metadata)
        .await
        .expect("response");
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(database.pool())
        .await
        .expect("corruption fixture");
    let changes = [
        (
            "UPDATE task_agent_turn SET input_tokens = -1 WHERE task_attempt_id = 'first'",
            "UPDATE task_agent_turn SET input_tokens = 7 WHERE task_attempt_id = 'first'",
        ),
        (
            "UPDATE task_agent_turn SET output_tokens = -1 WHERE task_attempt_id = 'first'",
            "UPDATE task_agent_turn SET output_tokens = 3 WHERE task_attempt_id = 'first'",
        ),
    ];
    for (corrupt, restore) in changes {
        sqlx::query(corrupt)
            .execute(database.pool())
            .await
            .expect("corrupt counts");

        // Act
        let load = database.tasks().load_agent_history("task").await;
        let duplicate = database
            .tasks()
            .persist_agent_turn("first", &metadata)
            .await;

        // Assert
        assert!(load.is_err());
        assert!(duplicate.is_err());
        sqlx::query(restore)
            .execute(database.pool())
            .await
            .expect("restore counts");
    }

    // Arrange
    sqlx::query("UPDATE task_attempt SET message = 'not json' WHERE id = 'first'")
        .execute(database.pool())
        .await
        .expect("corrupt replay");

    // Act
    let history = database.tasks().load_agent_history("task").await;

    // Assert
    assert!(history.is_err());
}

#[tokio::test]
async fn responses_in_the_same_session_stay_in_their_own_task_histories() {
    // Arrange
    let database = task_database_fixture().await;
    running_attempt(&database, "first").await;
    let second = SubmitAgentTaskRequest {
        task_id: "other-task".to_string(),
        ..agent_task_request("second")
    };
    database
        .tasks()
        .claim_agent_attempt("session", &second, "reply")
        .await
        .expect("second task");
    database
        .operations()
        .mark_session_operation_running("task-attempt:second")
        .await
        .expect("second running");

    // Act
    for attempt in ["first", "second"] {
        database
            .tasks()
            .persist_agent_turn(attempt, &metadata(attempt))
            .await
            .expect("response");
    }
    let first_history = database
        .tasks()
        .load_agent_history("task")
        .await
        .expect("history")
        .expect("first");
    let second_history = database
        .tasks()
        .load_agent_history("other-task")
        .await
        .expect("history")
        .expect("second");

    // Assert
    assert_eq!(first_history.turns.len(), 1);
    assert_eq!(second_history.turns.len(), 1);
    assert_eq!(first_history.turns[0].attempt_id, "first");
    assert_eq!(second_history.turns[0].attempt_id, "second");
    assert_eq!(
        first_history.provider_conversation_id.as_deref(),
        Some("thread-first")
    );
    assert_eq!(
        second_history.provider_conversation_id.as_deref(),
        Some("thread-second")
    );
    assert_eq!(
        usage(&database).await,
        (14, 6, 2, "gpt-6.1-sol".to_string())
    );
}

async fn running_attempt(database: &Database, attempt_id: &str) {
    let request = agent_task_request(attempt_id);
    database
        .tasks()
        .claim_agent_attempt("session", &request, "reply")
        .await
        .expect("admission");
    database
        .operations()
        .mark_session_operation_running(&format!("task-attempt:{attempt_id}"))
        .await
        .expect("running");
}

fn metadata(label: &str) -> TaskAgentTurnMetadata {
    TaskAgentTurnMetadata {
        assistant_message: serde_json::json!({"answer": label, "questions": []}).to_string(),
        harness_turn_report: Some("content-free harness report".to_string()),
        instruction_conversation_id: Some(format!("bootstrap-{label}")),
        input_tokens: 7,
        output_tokens: 3,
        provider_conversation_id: Some(format!("thread-{label}")),
    }
}

async fn usage(database: &Database) -> (i64, i64, i64, String) {
    sqlx::query_as(
        "SELECT input_tokens, output_tokens, invocation_count, model FROM session_usage WHERE \
         session_id = 'session'",
    )
    .fetch_one(database.pool())
    .await
    .expect("usage")
}

async fn ordinary_conversation(
    database: &Database,
) -> (Option<String>, Option<String>, String, String) {
    sqlx::query_as(
        "SELECT provider_conversation_id, app_server_instruction_provider_conversation_id, \
         questions, model FROM session WHERE id = 'session'",
    )
    .fetch_one(database.pool())
    .await
    .expect("ordinary state")
}
