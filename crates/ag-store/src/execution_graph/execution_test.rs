use std::num::NonZeroU64;

use ag_session::{GraphContext, GraphExecutionRef, GraphResolvedInput};
use ag_worker::RunState;

use crate::execution_graph::support_test::{
    evidence, graph, produced, reader, record, reference, run,
};
use crate::test_support::task_database_fixture;

#[tokio::test]
async fn corrupt_execution_revisions_fail_closed() {
    // Arrange
    let database = produced().await;
    for revision in [-1_i64, 0] {
        let mut connection = database.pool().acquire().await.expect("connection");
        // Simulate a damaged database without changing production constraints.
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await
            .expect("allow corruption");
        sqlx::query("UPDATE execution_graph_execution SET revision = ?")
            .bind(revision)
            .execute(&mut *connection)
            .await
            .expect("corrupt revision");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await
            .expect("restore constraints");
        drop(connection);

        // Act
        let result = database
            .graphs()
            .load_execution(&record("run").execution)
            .await;

        // Assert
        assert!(
            matches!(result, Err(crate::DbError::InvalidData { reason, .. })
            if reason == "Invalid persisted graph revision")
        );
    }
}

#[tokio::test]
async fn input_binding_uses_persisted_evidence_and_rolls_back_partial_links() {
    // Arrange
    let database = produced().await;
    run(&database, "consumer", Some("session"), RunState::Queued).await;
    let mut consumer = record("consumer");
    consumer.node_id = "second".into();
    consumer.inputs = vec![GraphResolvedInput {
        name: "evidence".into(),
        result: reference(),
    }];

    // Act / Assert
    assert!(database.graphs().link_execution(&consumer).await.is_err());
    assert!(
        database
            .graphs()
            .load_execution(&consumer.execution)
            .await
            .expect("rollback")
            .is_none()
    );
    database
        .graphs()
        .validate_result(&reference(), &evidence())
        .await
        .expect("validate");
    assert!(database.graphs().link_execution(&consumer).await.is_err());
    database
        .graphs()
        .accept_result(&reference(), &evidence())
        .await
        .expect("accept");
    assert!(
        database
            .graphs()
            .link_execution(&consumer)
            .await
            .expect("bind")
    );
    assert!(
        !database
            .graphs()
            .link_execution(&consumer)
            .await
            .expect("duplicate")
    );
    assert_eq!(
        database
            .graphs()
            .load_execution(&consumer.execution)
            .await
            .expect("load"),
        Some(consumer.clone())
    );
    consumer.inputs[0].result.id = "newer".into();
    assert!(database.graphs().link_execution(&consumer).await.is_err());
}

#[tokio::test]
async fn links_reject_missing_foreign_and_mismatched_authorities() {
    // Arrange
    let database = task_database_fixture().await;
    database
        .graphs()
        .save_graph(&graph(), None)
        .await
        .expect("graph");
    run(&database, "run", Some("session"), RunState::Queued).await;
    run(&database, "foreign", Some("other"), RunState::Queued).await;
    run(&database, "unowned", None, RunState::Queued).await;
    let mut cases = vec![
        record("missing"),
        record("foreign"),
        record("unowned"),
        record(""),
    ];
    let mut unknown = record("run");
    unknown.graph_id = "missing".into();
    cases.push(unknown);
    let mut stale = record("run");
    stale.graph_revision = NonZeroU64::new(2).expect("revision");
    cases.push(stale);
    let mut node = record("run");
    node.node_id = "missing".into();
    cases.push(node);
    let mut missing_inputs = record("run");
    missing_inputs.node_id = "second".into();
    cases.push(missing_inputs);
    let mut wrong_kind = record("run");
    wrong_kind.execution = GraphExecutionRef::TaskAttempt {
        attempt_id: "attempt".into(),
        operation_id: "op".into(),
        task_id: "task".into(),
    };
    cases.push(wrong_kind);

    // Act / Assert
    for invalid in cases {
        assert!(
            database.graphs().link_execution(&invalid).await.is_err(),
            "{invalid:?}"
        );
    }
    assert!(
        database
            .graphs()
            .link_execution(&record("run"))
            .await
            .expect("valid")
    );
    let mut duplicate = record("run");
    duplicate.node_id = "second".into();
    assert!(database.graphs().link_execution(&duplicate).await.is_err());
}

#[tokio::test]
async fn exact_input_references_cannot_be_forged_or_duplicated() {
    // Arrange
    let database = produced().await;
    database
        .graphs()
        .validate_result(&reference(), &evidence())
        .await
        .expect("validate");
    database
        .graphs()
        .accept_result(&reference(), &evidence())
        .await
        .expect("accept");
    run(&database, "consumer", Some("session"), RunState::Queued).await;
    let mut consumer = record("consumer");
    consumer.node_id = "second".into();
    consumer.inputs = vec![GraphResolvedInput {
        name: "evidence".into(),
        result: reference(),
    }];
    let mut forged = consumer.clone();
    forged.inputs[0].result.version = NonZeroU64::new(2).expect("version");
    let mut missing = consumer.clone();
    missing.inputs[0].result.id = "missing".into();
    let mut slot = consumer.clone();
    slot.inputs[0].name = "undeclared".into();

    // Act / Assert
    for invalid in [forged, missing, slot] {
        assert!(database.graphs().link_execution(&invalid).await.is_err());
        assert!(
            database
                .graphs()
                .load_execution(&invalid.execution)
                .await
                .expect("rollback")
                .is_none()
        );
    }
    assert!(
        database
            .graphs()
            .link_execution(&consumer)
            .await
            .expect("valid")
    );
}

#[tokio::test]
async fn task_context_requires_its_exact_attempt_operation_and_session() {
    // Arrange
    let database = task_database_fixture().await;
    let mut snapshot = graph();
    snapshot.nodes.truncate(1);
    snapshot.nodes[0].execution.context = GraphContext::TaskConversation {
        task_id: "task".into(),
    };
    snapshot.nodes[0].execution.response_profile = ag_protocol::ProtocolRequestProfile::SessionTurn;
    snapshot.nodes[0].outputs[0].schema = ag_protocol::ProtocolRequestProfile::SessionTurn;
    database
        .graphs()
        .save_graph(&snapshot, None)
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
    let task = database
        .tasks()
        .load_task("task")
        .await
        .expect("task")
        .expect("exists");
    let operation_id = task.attempts[0].operation_id.clone();
    let mut receipt = record("unused");
    receipt.execution = GraphExecutionRef::TaskAttempt {
        attempt_id: "missing".into(),
        operation_id: operation_id.clone(),
        task_id: "task".into(),
    };

    // Act / Assert
    assert!(database.graphs().link_execution(&receipt).await.is_err());
    receipt.execution = GraphExecutionRef::TaskAttempt {
        attempt_id: "attempt".into(),
        operation_id: "wrong".into(),
        task_id: "task".into(),
    };
    assert!(database.graphs().link_execution(&receipt).await.is_err());
    receipt.execution = GraphExecutionRef::TaskAttempt {
        attempt_id: "attempt".into(),
        operation_id,
        task_id: "task".into(),
    };
    assert!(
        database
            .graphs()
            .link_execution(&receipt)
            .await
            .expect("task link")
    );
    let mut wrong = receipt.execution.clone();
    if let GraphExecutionRef::TaskAttempt { task_id, .. } = &mut wrong {
        *task_id = "other".into();
    }
    assert!(database.graphs().load_execution(&wrong).await.is_err());
}

#[tokio::test]
async fn join_inputs_are_complete_unique_and_committed_together() {
    // Arrange
    let database = task_database_fixture().await;
    let mut graph = graph();
    graph.nodes.push(reader("third"));
    graph.nodes[1].inputs.push(ag_session::GraphInputBinding {
        name: "other-evidence".into(),
        output_name: "response".into(),
        producer_node_id: "third".into(),
    });
    graph.nodes[1]
        .dependencies
        .push(ag_session::GraphDependency {
            node_id: "third".into(),
            gate: ag_session::GraphCompletionGate::ResultValidated {
                output_name: "response".into(),
            },
        });
    database
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    let mut results = Vec::new();
    for (node, id) in [("first", "run"), ("third", "third-run")] {
        run(&database, id, Some("session"), RunState::Completed).await;
        let mut execution = record(id);
        execution.node_id = node.into();
        database
            .graphs()
            .link_execution(&execution)
            .await
            .expect("branch");
        let mut output = reference();
        output.id = format!("{node}-result");
        output.node_id = node.into();
        output.execution = execution.execution;
        database
            .graphs()
            .produce_result(&output, "{\"answer\":\"review\"}")
            .await
            .expect("output");
        database
            .graphs()
            .validate_result(&output, &evidence())
            .await
            .expect("validate");
        database
            .graphs()
            .accept_result(&output, &evidence())
            .await
            .expect("accept");
        results.push(output);
    }
    run(&database, "join", Some("session"), RunState::Queued).await;
    let mut join = record("join");
    join.node_id = "second".into();
    join.inputs = vec![
        GraphResolvedInput {
            name: "evidence".into(),
            result: results[0].clone()
        };
        2
    ];

    // Act / Assert
    assert!(database.graphs().link_execution(&join).await.is_err());
    assert!(
        database
            .graphs()
            .load_execution(&join.execution)
            .await
            .expect("rollback")
            .is_none()
    );
    join.inputs[1] = GraphResolvedInput {
        name: "other-evidence".into(),
        result: results[1].clone(),
    };
    assert!(database.graphs().link_execution(&join).await.expect("join"));
    assert_eq!(
        database
            .graphs()
            .load_execution(&join.execution)
            .await
            .expect("inputs"),
        Some(join)
    );
}

#[tokio::test]
async fn corrupt_execution_and_result_receipts_fail_closed() {
    // Arrange
    let database = produced().await;
    database
        .graphs()
        .validate_result(&reference(), &evidence())
        .await
        .expect("validate");
    database
        .graphs()
        .accept_result(&reference(), &evidence())
        .await
        .expect("accept");
    run(&database, "consumer", Some("session"), RunState::Queued).await;
    let mut consumer = record("consumer");
    consumer.node_id = "second".into();
    consumer.inputs.push(GraphResolvedInput {
        name: "evidence".into(),
        result: reference(),
    });
    database
        .graphs()
        .link_execution(&consumer)
        .await
        .expect("consumer");

    // Act / Assert
    sqlx::query(
        "UPDATE execution_graph_execution SET reference = 'not json' WHERE agent_run_id = 'run'",
    )
    .execute(database.pool())
    .await
    .expect("corrupt execution");
    assert!(
        database
            .graphs()
            .load_execution(&record("run").execution)
            .await
            .is_err()
    );
    assert!(
        database
            .graphs()
            .list_executions("graph", NonZeroU64::MIN)
            .await
            .is_err()
    );
    sqlx::query("UPDATE execution_graph_execution SET reference = ? WHERE agent_run_id = 'run'")
        .bind(serde_json::json!(record("missing").execution).to_string())
        .execute(database.pool())
        .await
        .expect("corrupt execution identity");
    assert!(matches!(
        database.graphs().list_executions("graph", NonZeroU64::MIN).await,
        Err(crate::DbError::InvalidData { reason, .. }) if reason == "Execution receipt disappeared"
    ));
    sqlx::query("UPDATE execution_graph_execution SET reference = ? WHERE agent_run_id = 'run'")
        .bind(serde_json::json!(record("run").execution).to_string())
        .execute(database.pool())
        .await
        .expect("restore execution");
    sqlx::query("UPDATE execution_graph_result SET reference = 'not json'")
        .execute(database.pool())
        .await
        .expect("corrupt result");
    assert!(database.graphs().load_result("result").await.is_err());
    assert!(
        database
            .graphs()
            .load_execution(&consumer.execution)
            .await
            .is_err()
    );
    let mut blank = reference();
    blank.id.clear();
    sqlx::query("UPDATE execution_graph_result SET reference = ?")
        .bind(serde_json::json!(blank).to_string())
        .execute(database.pool())
        .await
        .expect("invalid reference");
    assert!(database.graphs().load_result("result").await.is_err());
}
