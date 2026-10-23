use std::num::NonZeroU64;

use ag_session::{GraphExecutionRef, GraphResultState};
use ag_worker::RunState;

use super::support_test::{evidence, graph, produced, record, reference, run};
use crate::test_support::task_database_fixture;

#[tokio::test]
async fn result_production_rejects_a_node_removed_from_a_corrupt_snapshot() {
    // Arrange
    let database = produced().await;
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    snapshot.nodes[0].id = "replacement".into();
    assert_eq!(snapshot.validate(), Ok(()));
    sqlx::query("UPDATE execution_graph_revision SET snapshot = ?")
        .bind(serde_json::json!(snapshot).to_string())
        .execute(database.pool())
        .await
        .expect("corrupt snapshot");
    let mut output = reference();
    output.id = "missing-node-result".into();
    output.version = NonZeroU64::MIN.saturating_add(1);

    // Act
    let result = database
        .graphs()
        .produce_result(&output, r#"{"answer":"Complete response"}"#)
        .await;

    // Assert
    assert!(
        matches!(result, Err(crate::DbError::InvalidData { reason, .. })
        if reason == "Result node does not exist")
    );
    assert!(
        database
            .graphs()
            .load_result(&output.id)
            .await
            .expect("result")
            .is_none()
    );
}

#[tokio::test]
async fn successful_task_clarification_remains_produced_and_unaccepted() {
    // Arrange
    let database = task_database_fixture().await;
    let mut graph = graph();
    graph.nodes.truncate(1);
    graph.nodes[0].execution.context = ag_session::GraphContext::TaskConversation {
        task_id: "task".into(),
    };
    graph.nodes[0].execution.response_profile = ag_protocol::ProtocolRequestProfile::SessionTurn;
    graph.nodes[0].outputs[0].schema = ag_protocol::ProtocolRequestProfile::SessionTurn;
    database
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    let request = ag_session::SubmitTaskRequest {
        attempt_id: "attempt".into(),
        task_id: "task".into(),
        objective: "Review".into(),
        message: "review".into(),
        visibility: ag_session::DurableMessageVisibility::Hidden,
    };
    database
        .tasks()
        .claim_attempt("session", &request, "chat")
        .await
        .expect("attempt");
    let mut record = record("unused");
    record.execution = GraphExecutionRef::TaskAttempt {
        attempt_id: request.attempt_id.clone(),
        operation_id: request.operation_id(),
        task_id: request.task_id.clone(),
    };
    database
        .graphs()
        .link_execution(&record)
        .await
        .expect("link");
    database
        .operations()
        .mark_session_operation_running(&request.operation_id())
        .await
        .expect("start");
    database
        .operations()
        .mark_session_operation_done(&request.operation_id())
        .await
        .expect("settle");
    let mut reference = reference();
    reference.execution = record.execution;
    reference.schema = ag_protocol::ProtocolRequestProfile::SessionTurn;
    let body = r#"{"answer":"", "questions":[{"text":"Which target?", "options":[]}], "review_comment_outcomes":[]}"#;
    database
        .graphs()
        .produce_result(&reference, body)
        .await
        .expect("output");

    // Act
    let result = database
        .graphs()
        .validate_result(&reference, &evidence())
        .await;

    // Assert
    assert!(result.is_err());
    assert!(
        database
            .graphs()
            .accept_result(&reference, &evidence())
            .await
            .is_err()
    );
    assert_eq!(
        database
            .graphs()
            .load_result("result")
            .await
            .expect("result")
            .expect("exists")
            .result
            .state,
        GraphResultState::Produced
    );
}

#[tokio::test]
async fn evidence_write_failure_does_not_advance_result_state() {
    // Arrange
    let database = produced().await;
    sqlx::query(
        "CREATE TRIGGER reject_evidence BEFORE INSERT ON execution_graph_evidence BEGIN SELECT \
         RAISE(ABORT, 'injected'); END",
    )
    .execute(database.pool())
    .await
    .expect("trigger");

    // Act
    let result = database
        .graphs()
        .validate_result(&reference(), &evidence())
        .await;

    // Assert
    assert!(result.is_err());
    assert_eq!(
        database
            .graphs()
            .load_result("result")
            .await
            .expect("result")
            .expect("exists")
            .result
            .state,
        GraphResultState::Produced
    );
}

#[tokio::test]
async fn evidence_advances_exact_versions_once_and_preserves_original_body() {
    // Arrange
    let database = produced().await;
    let original = database
        .graphs()
        .load_result("result")
        .await
        .expect("load")
        .expect("result");

    // Act / Assert
    assert_eq!(original.result.state, GraphResultState::Produced);
    assert!(original.validation.is_none());
    assert!(original.acceptance.is_none());
    assert!(
        database
            .graphs()
            .accept_result(&reference(), &evidence())
            .await
            .is_err()
    );
    assert!(
        database
            .graphs()
            .validate_result(&reference(), &evidence())
            .await
            .expect("validate")
    );
    assert!(
        !database
            .graphs()
            .validate_result(&reference(), &evidence())
            .await
            .expect("duplicate validation")
    );
    let validated = database
        .graphs()
        .load_result("result")
        .await
        .expect("load")
        .expect("result");
    assert_eq!(validated.result.state, GraphResultState::Validated);
    assert!(
        database
            .graphs()
            .accept_result(&reference(), &evidence())
            .await
            .expect("accept")
    );
    assert!(
        !database
            .graphs()
            .accept_result(&reference(), &evidence())
            .await
            .expect("duplicate acceptance")
    );
    let accepted = database
        .graphs()
        .load_result("result")
        .await
        .expect("load")
        .expect("result");
    assert_eq!(accepted.result.state, GraphResultState::Accepted);
    assert_eq!(accepted.validation, validated.validation);
    assert_eq!(accepted.body, original.body);
    assert_eq!(accepted.result.reference, original.result.reference);
    assert!(
        !database
            .graphs()
            .produce_result(&reference(), &original.body)
            .await
            .expect("redeliver")
    );
    let mut changed = evidence();
    changed.reason = "different".into();
    assert!(
        database
            .graphs()
            .validate_result(&reference(), &changed)
            .await
            .is_err()
    );
    assert!(
        database
            .graphs()
            .accept_result(&reference(), &changed)
            .await
            .is_err()
    );
    assert!(
        database
            .graphs()
            .load_result("missing")
            .await
            .expect("missing")
            .is_none()
    );
}

#[tokio::test]
async fn output_identity_body_schema_authority_and_versions_are_immutable() {
    // Arrange
    let database = produced().await;
    run(&database, "new-run", Some("session"), RunState::Completed).await;
    database
        .graphs()
        .link_execution(&record("new-run"))
        .await
        .expect("next execution");
    let mut next = reference();
    next.id = "result-2".into();
    next.execution = record("new-run").execution;
    next.version = NonZeroU64::new(2).expect("version");
    let mut cases = Vec::new();
    let mut conflict = reference();
    conflict.output_name = "another".into();
    cases.push(conflict);
    let mut missing = next.clone();
    missing.execution = GraphExecutionRef::UtilityRun {
        run_id: "missing".into(),
    };
    cases.push(missing);
    let mut foreign = next.clone();
    foreign.node_id = "second".into();
    cases.push(foreign);
    let mut schema = next.clone();
    schema.schema = ag_protocol::ProtocolRequestProfile::FocusedReview;
    cases.push(schema);
    let mut version = next.clone();
    version.version = NonZeroU64::new(3).expect("version");
    cases.push(version);
    let mut overflow = next.clone();
    overflow.version = NonZeroU64::new(u64::MAX).expect("version");
    cases.push(overflow);
    let mut blank = next.clone();
    blank.id = " ".into();
    cases.push(blank);

    // Act / Assert
    assert!(
        database
            .graphs()
            .produce_result(&reference(), "different body")
            .await
            .is_err()
    );
    for invalid in cases {
        assert!(
            database
                .graphs()
                .produce_result(&invalid, "new")
                .await
                .is_err(),
            "{invalid:?}"
        );
    }
    assert!(
        database
            .graphs()
            .produce_result(&next, "{\"answer\":\"second\"}")
            .await
            .expect("next version")
    );
    assert_eq!(
        database
            .graphs()
            .load_result("result")
            .await
            .expect("load")
            .expect("result")
            .body,
        "{\"answer\":\"original\"}"
    );
    assert_eq!(
        database
            .graphs()
            .load_result("result-2")
            .await
            .expect("load")
            .expect("result")
            .result
            .state,
        GraphResultState::Produced
    );
    let mut second_output = next;
    second_output.id = "another-result".into();
    second_output.version = NonZeroU64::new(3).expect("version");
    assert!(
        database
            .graphs()
            .produce_result(&second_output, "duplicate output")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validation_rejects_unsettled_failed_canceled_and_malformed_outputs() {
    // Arrange
    let database = produced().await;
    let valid_body = r#"{"answer":"Complete response"}"#;

    for (index, (state, body)) in [
        (RunState::Queued, valid_body),
        (RunState::Running, valid_body),
        (RunState::Failed, valid_body),
        (RunState::Canceled, valid_body),
        (RunState::Completed, "malformed"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("run-{index}");
        run(&database, &id, Some("session"), state).await;
        database
            .graphs()
            .link_execution(&record(&id))
            .await
            .expect("link");
        let mut output = reference();
        output.id = format!("output-{index}");
        output.execution = record(&id).execution;
        output.version = NonZeroU64::new(index as u64 + 2).expect("version");
        database
            .graphs()
            .produce_result(&output, body)
            .await
            .expect("produced");

        // Act
        let result = database
            .graphs()
            .validate_result(&output, &evidence())
            .await;

        // Assert
        assert!(result.is_err(), "{state:?}: {body}");
        assert!(
            database
                .graphs()
                .load_result(&output.id)
                .await
                .expect("load")
                .expect("result")
                .validation
                .is_none()
        );
    }
}

#[tokio::test]
async fn attestations_reject_forged_missing_stale_and_blank_evidence() {
    // Arrange
    let database = produced().await;
    let mut forged = reference();
    forged.version = NonZeroU64::new(2).expect("version");
    let mut stale = reference();
    stale.graph_revision = NonZeroU64::new(2).expect("revision");
    let mut missing = reference();
    missing.id = "missing".into();

    // Act / Assert
    for invalid in [forged, stale, missing] {
        assert!(
            database
                .graphs()
                .validate_result(&invalid, &evidence())
                .await
                .is_err()
        );
        assert!(
            database
                .graphs()
                .accept_result(&invalid, &evidence())
                .await
                .is_err()
        );
    }
    for (actor, reason) in [("", "reason"), ("actor", " ")] {
        let mut invalid = evidence();
        invalid.actor = actor.into();
        invalid.reason = reason.into();
        assert!(
            database
                .graphs()
                .validate_result(&reference(), &invalid)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn session_deletion_cascades_all_graph_data_without_altering_other_sessions() {
    // Arrange
    let database = produced().await;
    database
        .graphs()
        .validate_result(&reference(), &evidence())
        .await
        .expect("validation");
    database
        .graphs()
        .accept_result(&reference(), &evidence())
        .await
        .expect("acceptance");

    // Act
    sqlx::query("DELETE FROM session WHERE id = 'session'")
        .execute(database.pool())
        .await
        .expect("delete");

    // Assert
    assert_eq!(
        database
            .graphs()
            .list_graphs("session")
            .await
            .expect("graphs"),
        Vec::new()
    );
    assert!(
        database
            .graphs()
            .load_execution(&reference().execution)
            .await
            .expect("execution")
            .is_none()
    );
    assert!(
        database
            .graphs()
            .load_result("result")
            .await
            .expect("result")
            .is_none()
    );
}
