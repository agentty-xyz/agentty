//! Public static workflow durability and explicit recovery contract.

use ag_session::{
    AcceptTaskChainPlanRequest, AgentKind, AgentModel, AgentSelection, ContinueTaskChainRequest,
    StartPlanThenImplementRequest, TaskChainPhase,
};
use ag_store::{Database, DbError, TaskAgentTurnMetadata};
use tempfile::tempdir;

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

#[tokio::test]
async fn reopening_keeps_reply_receipts_with_one_transcript_prompt() {
    // Arrange
    let directory = tempdir().expect("directory");
    let path = directory.path().join("reply.sqlite");
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
    database
        .tasks()
        .start_task_chain(
            "session",
            &StartPlanThenImplementRequest {
                agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
                chain_id: "chain".into(),
                planner_attempt_id: "plan".into(),
                user_request: "Implement this change".into(),
            },
            "reply",
        )
        .await
        .expect("start");
    let recorded = database
        .tasks()
        .record_task_chain_reply("chain", "reply-1", "Go ahead")
        .await
        .expect("record reply");
    database.pool().close().await;

    // Act
    let reopened = Database::open(&path).await.expect("reopen");
    let redelivered = reopened
        .tasks()
        .record_task_chain_reply("chain", "reply-1", "Go ahead")
        .await
        .expect("reply receipt");
    let messages = reopened
        .sessions()
        .load_session_messages("session")
        .await
        .expect("transcript");

    // Assert
    assert!(recorded);
    assert!(!redelivered);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "user_prompt");
    assert_eq!(messages[0].content, "Go ahead");
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
