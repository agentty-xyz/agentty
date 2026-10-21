//! Public static workflow durability and explicit recovery contract.

use ag_session::{
    AcceptTaskChainPlanRequest, AgentAnswerContent, AgentKind, AgentModel, AgentSelection,
    BuiltinAgentDefinition, ContinueTaskChainRequest, SessionMessageKind,
    StartPlanThenImplementRequest, TaskChainPhase,
};
use ag_store::{Database, DbError, TaskAgentTurnMetadata};
use tempfile::tempdir;

#[tokio::test]
async fn launch_transcript_failures_roll_back_workflow_admission_before_retry() {
    for failed_kind in [
        SessionMessageKind::UserPrompt,
        SessionMessageKind::AgentGraph,
    ] {
        // Arrange
        let database = chain_database().await.expect("database");
        let mut request = chain_request();
        request.chain_id = " chain \n".into();
        let before = durable_counts(&database).await.expect("counts");
        reject_transcript_kind(&database, failed_kind)
            .await
            .expect("reject transcript");

        // Act
        let failed = database
            .tasks()
            .start_task_chain("session", &request, "reply")
            .await;

        // Assert
        assert!(failed.is_err(), "{failed_kind}");
        assert_eq!(
            durable_counts(&database).await.expect("counts"),
            before,
            "{failed_kind}"
        );
        assert_eq!(
            database
                .tasks()
                .load_task_chain(&request.chain_id)
                .await
                .expect("chain"),
            None
        );

        // Act: retry the same immutable launch after storage recovers.
        allow_transcript(&database).await.expect("recover storage");
        assert!(
            database
                .tasks()
                .start_task_chain("session", &request, "reply")
                .await
                .expect("retry")
        );
        assert!(
            !database
                .tasks()
                .start_task_chain("session", &request, "reply")
                .await
                .expect("redelivery")
        );

        // Assert
        let messages = database
            .sessions()
            .load_session_messages("session")
            .await
            .expect("transcript");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].kind, SessionMessageKind::UserPrompt.as_str());
        assert_eq!(messages[0].content, request.user_request);
        assert_eq!(messages[1].kind, SessionMessageKind::AgentGraph.as_str());
        assert_eq!(messages[1].content, request.chain_id);
    }
}

#[tokio::test]
async fn role_transcript_failure_rolls_back_response_usage_and_continuation_before_retry() {
    // Arrange
    let database = chain_database().await.expect("database");
    let request = chain_request();
    database
        .tasks()
        .start_task_chain("session", &request, "reply")
        .await
        .expect("launch");
    database
        .operations()
        .mark_session_operation_running("task-attempt:plan")
        .await
        .expect("running");
    let task_id = request.planner_request().task_id;
    let before_history = database
        .tasks()
        .load_agent_history(&task_id)
        .await
        .expect("history");
    let before_counts = durable_counts(&database).await.expect("counts");
    reject_transcript_kind(&database, SessionMessageKind::AgentAnswer)
        .await
        .expect("reject transcript");
    let metadata = TaskAgentTurnMetadata {
        assistant_message: r#"{"answer":"Durable plan","questions":[]}"#.into(),
        harness_turn_report: Some("{}".into()),
        instruction_conversation_id: Some("bootstrap".into()),
        input_tokens: 5,
        output_tokens: 2,
        provider_conversation_id: Some("thread".into()),
    };

    // Act
    let failed = database.tasks().persist_agent_turn("plan", &metadata).await;

    // Assert
    assert!(failed.is_err());
    assert_eq!(
        durable_counts(&database).await.expect("counts"),
        before_counts
    );
    assert_eq!(
        database
            .tasks()
            .load_agent_history(&task_id)
            .await
            .expect("history"),
        before_history
    );

    // Act: no receipt survived the failed projection, so retry writes all
    // effects once.
    allow_transcript(&database).await.expect("recover storage");
    assert!(
        database
            .tasks()
            .persist_agent_turn("plan", &metadata)
            .await
            .expect("retry")
    );
    assert!(
        !database
            .tasks()
            .persist_agent_turn("plan", &metadata)
            .await
            .expect("redelivery")
    );

    // Assert
    let history = database
        .tasks()
        .load_agent_history(&task_id)
        .await
        .expect("history")
        .expect("context");
    assert_eq!(history.turns.len(), 1);
    assert_eq!(
        history.provider_conversation_id,
        metadata.provider_conversation_id
    );
    assert_eq!(
        history.instruction_conversation_id,
        metadata.instruction_conversation_id
    );
    let usage: (i64, i64, i64) = sqlx::query_as(
        "SELECT input_tokens, output_tokens, invocation_count FROM session_usage WHERE session_id \
         = 'session'",
    )
    .fetch_one(database.pool())
    .await
    .expect("usage");
    assert_eq!(usage, (5, 2, 1));
    let messages = database
        .sessions()
        .load_session_messages("session")
        .await
        .expect("transcript");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2].kind, SessionMessageKind::AgentAnswer.as_str());
    assert_eq!(
        AgentAnswerContent::decode(&messages[2].content),
        Some(AgentAnswerContent {
            agent: BuiltinAgentDefinition::Planner,
            text: "Durable plan".into()
        })
    );
}

async fn chain_database() -> Result<Database, DbError> {
    let database = Database::open_in_memory().await?;
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;

    Ok(database)
}

fn chain_request() -> StartPlanThenImplementRequest {
    StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "chain".into(),
        planner_attempt_id: "plan".into(),
        user_request: "Implement this change".into(),
    }
}

async fn durable_counts(
    database: &Database,
) -> Result<(i64, i64, i64, i64, i64, i64, i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM task), (SELECT COUNT(*) FROM task_attempt), (SELECT \
         COUNT(*) FROM task_agent_context), (SELECT COUNT(*) FROM task_chain), (SELECT COUNT(*) \
         FROM task_agent_turn), (SELECT COUNT(*) FROM session_usage), (SELECT COUNT(*) FROM \
         harness_turn_report), (SELECT COUNT(*) FROM session_message)",
    )
    .fetch_one(database.pool())
    .await
}

async fn reject_transcript_kind(
    database: &Database,
    kind: SessionMessageKind,
) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE TABLE rejected_transcript_kind (kind TEXT NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query("INSERT INTO rejected_transcript_kind (kind) VALUES (?)")
        .bind(kind.as_str())
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER reject_transcript BEFORE INSERT ON session_message WHEN NEW.kind IN \
         (SELECT kind FROM rejected_transcript_kind) BEGIN SELECT RAISE(ABORT, 'transcript \
         unavailable'); END",
    )
    .execute(database.pool())
    .await?;
    Ok(())
}

async fn allow_transcript(database: &Database) -> Result<(), sqlx::Error> {
    sqlx::query("DROP TRIGGER reject_transcript")
        .execute(database.pool())
        .await?;
    Ok(())
}

#[tokio::test]
async fn reopening_never_replays_an_interrupted_implementation() {
    // Arrange
    let directory = tempdir().expect("directory");
    let path = directory.path().join("chain.sqlite");
    let database = Database::open(&path).await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await
        .expect("session");
    let start = StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "chain".into(),
        planner_attempt_id: "plan".into(),
        user_request: "Implement this change".into(),
    };
    begin_implementation(&database, &start)
        .await
        .expect("initial implementation");
    database.pool().close().await;

    // Act
    let reopened = Database::open(&path).await.expect("reopen");
    reopened
        .operations()
        .fail_unfinished_session_operations("Interrupted by restart")
        .await
        .expect("recovery");
    let recovered = reopened
        .tasks()
        .load_task_chain("chain")
        .await
        .expect("snapshot")
        .expect("chain");
    let repeated = reopened
        .tasks()
        .start_task_chain("session", &start, "reply")
        .await
        .expect("old launch receipt");

    // Assert
    assert!(!repeated);
    assert_eq!(recovered.phase, TaskChainPhase::Failed);
    assert_eq!(recovered.current_request.attempt_id, "implement");
    assert_eq!(
        recovered.accepted_plan.expect("accepted").text,
        "Exact plan"
    );
    assert_eq!(
        reopened
            .tasks()
            .load_task("chain:chain:implementer")
            .await
            .expect("task")
            .expect("present")
            .attempts
            .len(),
        1
    );
    let admitted = reopened
        .tasks()
        .continue_task_chain(
            "chain",
            &ContinueTaskChainRequest {
                attempt_id: "explicit-retry".into(),
                clarification: Some("Retry after inspection".into()),
                expected_attempt_id: "implement".into(),
            },
            "reply",
        )
        .await
        .expect("explicit retry");
    assert!(admitted);
    assert_eq!(
        reopened
            .tasks()
            .load_task_chain("chain")
            .await
            .expect("snapshot")
            .expect("chain")
            .phase,
        TaskChainPhase::Implementing
    );
    reopened
        .sessions()
        .delete_session("session")
        .await
        .expect("delete owner");
    assert_eq!(
        reopened
            .tasks()
            .load_task_chain("chain")
            .await
            .expect("deleted chain"),
        None
    );
}

async fn begin_implementation(
    database: &Database,
    start: &StartPlanThenImplementRequest,
) -> Result<(), DbError> {
    database
        .tasks()
        .start_task_chain("session", start, "reply")
        .await?;
    database
        .operations()
        .mark_session_operation_running("task-attempt:plan")
        .await?;
    database
        .tasks()
        .persist_agent_turn(
            "plan",
            &TaskAgentTurnMetadata {
                assistant_message: r#"{"answer":"Exact plan","questions":[]}"#.into(),
                harness_turn_report: None,
                instruction_conversation_id: None,
                input_tokens: 0,
                output_tokens: 0,
                provider_conversation_id: None,
            },
        )
        .await?;
    database
        .operations()
        .mark_session_operation_done("task-attempt:plan")
        .await?;
    database
        .tasks()
        .accept_task_chain_plan(
            "chain",
            &AcceptTaskChainPlanRequest {
                attempt_id: "implement".into(),
                plan_attempt_id: "plan".into(),
            },
            "reply",
        )
        .await?;
    database
        .operations()
        .mark_session_operation_running("task-attempt:implement")
        .await?;

    Ok(())
}
