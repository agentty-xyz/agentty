//! Graph dispatch invariant tests for input readiness, execution gates,
//! per-Env admission conflicts, and settlement rollback.

use ag_protocol::ProtocolRequestProfile;
use ag_session::{
    EnvId, GraphCompletionGate, GraphExecutionRef, GraphWorkspaceAccess, TaskWorktreeId,
};
use ag_worker::{GraphAdmission, GraphDispatch, RunState};

use crate::execution_graph::SqliteGraphRepository;
use crate::execution_graph::support_test::{
    evidence, graph, produced, reader, record, reference, run, workspace,
};

#[tokio::test]
async fn ready_inputs_checks_result_availability_after_execution_success() {
    // Arrange
    let database = produced().await;
    let mut graph = graph();
    graph.nodes[1].dependencies[0].gate = GraphCompletionGate::ExecutionSucceeded;
    let mut connection = database.pool().acquire().await.expect("connection");
    sqlx::query(
        "INSERT INTO execution_graph_response (execution_graph_execution_id, body) SELECT id, \
         '{\"answer\":\"done\"}' FROM execution_graph_execution",
    )
    .execute(&mut *connection)
    .await
    .expect("settled response");

    // Act
    let inputs = SqliteGraphRepository::ready_inputs(&mut connection, &graph, &graph.nodes[1])
        .await
        .expect("readiness");

    // Assert
    assert!(inputs.is_none(), "an unvalidated result cannot be bound");
}

#[tokio::test]
async fn ready_inputs_rejects_a_validated_reference_with_the_wrong_schema() {
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
    let mut incompatible = reference();
    incompatible.schema = ProtocolRequestProfile::FocusedReview;
    let mut connection = database.pool().acquire().await.expect("connection");
    sqlx::query("UPDATE execution_graph_result SET reference = ?")
        .bind(serde_json::json!(incompatible).to_string())
        .execute(&mut *connection)
        .await
        .expect("corrupt stored schema");
    let graph = graph();

    // Act
    let result =
        SqliteGraphRepository::ready_inputs(&mut connection, &graph, &graph.nodes[1]).await;

    // Assert
    assert!(
        result
            .expect_err("incompatible result")
            .to_string()
            .contains("output schema")
    );
}

#[tokio::test]
async fn execution_gates_require_success_a_link_and_a_declared_node() {
    // Arrange
    let database = produced().await;
    run(&database, "pending", Some("session"), RunState::Queued).await;
    run(&database, "unlinked", Some("session"), RunState::Completed).await;
    let graph = graph();
    let mut connection = database.pool().acquire().await.expect("connection");

    // Act
    let unfinished =
        SqliteGraphRepository::settled_response(&mut connection, &graph, &record("pending"))
            .await
            .expect("unfinished response");
    let unlinked =
        SqliteGraphRepository::settled_response(&mut connection, &graph, &record("unlinked")).await;
    sqlx::query(
        "INSERT INTO execution_graph_response (execution_graph_execution_id, body) SELECT id, \
         '{\"answer\":\"done\"}' FROM execution_graph_execution",
    )
    .execute(&mut *connection)
    .await
    .expect("response");
    let mut missing_node = graph.clone();
    missing_node.nodes.clear();
    let undeclared =
        SqliteGraphRepository::settled_response(&mut connection, &missing_node, &record("run"))
            .await;
    sqlx::query("UPDATE execution_graph_response SET body = 'invalid response'")
        .execute(&mut *connection)
        .await
        .expect("corrupt response");
    let malformed =
        SqliteGraphRepository::settled_response(&mut connection, &graph, &record("run")).await;

    // Assert
    assert!(!unfinished);
    assert!(
        unlinked
            .expect_err("unlinked execution")
            .to_string()
            .contains("Missing execution")
    );
    assert!(
        undeclared
            .expect_err("undeclared node")
            .to_string()
            .contains("Missing node")
    );
    assert!(malformed.is_err());
}

#[tokio::test]
async fn env_admission_rejects_malformed_or_dangling_active_execution_references() {
    // Arrange
    let database = produced().await;
    let graph = graph();
    let mut connection = database.pool().acquire().await.expect("connection");
    sqlx::query("UPDATE agent_run SET status = 'running'")
        .execute(&mut *connection)
        .await
        .expect("active authority");
    for (reference, expected) in [
        ("invalid reference".to_string(), "expected value"),
        (
            serde_json::json!(GraphExecutionRef::UtilityRun {
                run_id: "unlinked".into()
            })
            .to_string(),
            "Missing active execution",
        ),
    ] {
        sqlx::query("UPDATE execution_graph_execution SET reference = ?")
            .bind(reference)
            .execute(&mut *connection)
            .await
            .expect("corrupt reference");
        // A writer must learn the holder's Env too, so it fails like a reader.
        let mut writer = graph.nodes[0].clone();
        writer.execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;

        for candidate in [&graph.nodes[0], &writer] {
            // Act
            let result = SqliteGraphRepository::env_busy(&mut connection, &graph, candidate).await;

            // Assert
            assert!(
                result
                    .expect_err("invalid active execution")
                    .to_string()
                    .contains(expected)
            );
        }
    }
}

#[tokio::test]
async fn env_admission_treats_an_active_execution_of_an_unknown_node_as_a_conflict() {
    // Arrange: the active execution's node is missing from its stored graph.
    let database = crate::test_support::task_database_fixture().await;
    database
        .graphs()
        .save_graph(&graph(), None)
        .await
        .expect("graph");
    run(&database, "run", Some("session"), RunState::Running).await;
    database
        .graphs()
        .link_execution(&record("run"))
        .await
        .expect("link");
    let mut corrupt = graph();
    corrupt.nodes = vec![reader("elsewhere")];
    sqlx::query("UPDATE execution_graph_revision SET snapshot = ?")
        .bind(serde_json::json!(corrupt).to_string())
        .execute(database.pool())
        .await
        .expect("drop the active node");
    let mut connection = database.pool().acquire().await.expect("connection");

    for access in [
        GraphWorkspaceAccess::SharedRead,
        GraphWorkspaceAccess::BackgroundRead,
    ] {
        let mut candidate = reader("candidate");
        candidate.execution.workspace_access = access;

        // Act
        let busy = SqliteGraphRepository::env_busy(&mut connection, &graph(), &candidate)
            .await
            .expect("admission");

        // Assert
        assert!(busy, "{access:?} waits for an unknown holder");
    }
}

#[test]
fn env_conflicts_hold_only_within_one_env_under_the_access_rules() {
    // Arrange: `None` stands for session work that is not a graph node.
    let session = EnvId::SessionWorktree("session".into());
    let worktree = |id: &str| {
        EnvId::TaskWorktree(TaskWorktreeId {
            session_id: "session".into(),
            worktree_id: id.into(),
        })
    };
    let pairs = [
        (session.clone(), session.clone()),
        (worktree("first"), worktree("first")),
        (worktree("first"), session.clone()),
        (session, worktree("first")),
        (worktree("first"), worktree("second")),
    ];
    let same_env_rules = [
        (GraphWorkspaceAccess::ExclusiveWrite, None, true),
        (
            GraphWorkspaceAccess::ExclusiveWrite,
            Some(GraphWorkspaceAccess::SharedRead),
            true,
        ),
        (
            GraphWorkspaceAccess::ExclusiveWrite,
            Some(GraphWorkspaceAccess::BackgroundRead),
            true,
        ),
        (
            GraphWorkspaceAccess::ExclusiveWrite,
            Some(GraphWorkspaceAccess::ExclusiveWrite),
            true,
        ),
        (GraphWorkspaceAccess::SharedRead, None, true),
        (
            GraphWorkspaceAccess::SharedRead,
            Some(GraphWorkspaceAccess::SharedRead),
            false,
        ),
        (
            GraphWorkspaceAccess::SharedRead,
            Some(GraphWorkspaceAccess::BackgroundRead),
            false,
        ),
        (
            GraphWorkspaceAccess::SharedRead,
            Some(GraphWorkspaceAccess::ExclusiveWrite),
            true,
        ),
        (GraphWorkspaceAccess::BackgroundRead, None, false),
        (
            GraphWorkspaceAccess::BackgroundRead,
            Some(GraphWorkspaceAccess::SharedRead),
            false,
        ),
        (
            GraphWorkspaceAccess::BackgroundRead,
            Some(GraphWorkspaceAccess::BackgroundRead),
            false,
        ),
        (
            GraphWorkspaceAccess::BackgroundRead,
            Some(GraphWorkspaceAccess::ExclusiveWrite),
            true,
        ),
    ];
    for (access, holder, same_env_conflict) in same_env_rules {
        for (node_env, holder_env) in &pairs {
            let mut node = reader("candidate");
            node.env_id = node_env.clone();
            node.execution.workspace_access = access;

            // Act
            let conflict = SqliteGraphRepository::env_conflict(&node, holder_env, holder);

            // Assert
            assert_eq!(
                conflict,
                same_env_conflict && node_env == holder_env,
                "{access:?} in {node_env:?} against {holder:?} in {holder_env:?}"
            );
        }
    }
}

#[tokio::test]
async fn env_admission_ignores_session_worktree_work_for_a_node_in_another_env() {
    // Arrange: a graph writer, a chat turn, and a session utility are active
    // in the session worktree.
    let database = crate::test_support::task_database_fixture().await;
    let mut graph = graph();
    graph.nodes = vec![reader("writer")];
    graph.nodes[0].execution.workspace_access = GraphWorkspaceAccess::ExclusiveWrite;
    database
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    let writer = database
        .graph_dispatch()
        .claim(
            "graph",
            graph.revision,
            "writer",
            "writer-run",
            &workspace(),
        )
        .await
        .expect("admission");
    assert!(matches!(writer, GraphAdmission::Claimed(_)));
    database
        .operations()
        .insert_session_operation("turn", "session", "reply")
        .await
        .expect("chat turn");
    run(&database, "utility", Some("session"), RunState::Running).await;
    let mut connection = database.pool().acquire().await.expect("connection");

    for access in [
        GraphWorkspaceAccess::ExclusiveWrite,
        GraphWorkspaceAccess::SharedRead,
        GraphWorkspaceAccess::BackgroundRead,
    ] {
        let mut node = reader("isolated");
        node.execution.workspace_access = access;
        node.env_id = EnvId::TaskWorktree(TaskWorktreeId {
            session_id: "session".into(),
            worktree_id: "isolated".into(),
        });

        // Act
        let elsewhere = SqliteGraphRepository::env_busy(&mut connection, &graph, &node)
            .await
            .expect("task worktree admission");
        node.env_id = EnvId::SessionWorktree("session".into());
        let shared = SqliteGraphRepository::env_busy(&mut connection, &graph, &node)
            .await
            .expect("session worktree admission");

        // Assert
        assert!(!elsewhere, "{access:?} in a task worktree is free");
        assert!(shared, "{access:?} in the session worktree waits");
    }
}

#[tokio::test]
async fn settlement_rejects_unlinked_receipts_malformed_responses_and_conflicting_failures() {
    // Arrange
    let database = produced().await;
    let dispatch = GraphDispatch {
        inputs: Vec::new(),
        node: graph().nodes.remove(0),
        record: record("run"),
    };
    let mut unlinked = dispatch.clone();
    unlinked.record = record("missing");
    let mut conflicting = dispatch.record.clone();
    conflicting.node_id = "other".into();

    // Act
    let missing = database
        .graph_dispatch()
        .finish(&unlinked, r#"{"answer":"done"}"#)
        .await;
    let malformed = database
        .graph_dispatch()
        .finish(&dispatch, "invalid response")
        .await;
    let failure = database
        .graph_dispatch()
        .fail(&conflicting, "failure")
        .await;

    // Assert
    assert!(
        missing
            .expect_err("unlinked receipt")
            .to_string()
            .contains("Missing admitted execution")
    );
    assert!(malformed.is_err());
    assert!(
        failure
            .expect_err("mismatched receipt")
            .to_string()
            .contains("Failure differs")
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_graph_response")
        .fetch_one(database.pool())
        .await
        .expect("responses");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn exhausted_output_versions_roll_back_the_response_without_overflow() {
    // Arrange
    let database = produced().await;
    let dispatch = GraphDispatch {
        inputs: Vec::new(),
        node: graph().nodes.remove(0),
        record: record("run"),
    };
    sqlx::query("UPDATE execution_graph_result SET version = ?")
        .bind(i64::MAX)
        .execute(database.pool())
        .await
        .expect("last storage version");

    // Act
    let result = database
        .graph_dispatch()
        .finish(&dispatch, r#"{"answer":"next"}"#)
        .await;

    // Assert
    assert!(
        result
            .expect_err("version exhausted")
            .to_string()
            .contains("Invalid result version")
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_graph_response")
        .fetch_one(database.pool())
        .await
        .expect("responses");
    assert_eq!(count, 0);
    assert_eq!(
        database
            .graphs()
            .load_result("result")
            .await
            .expect("result")
            .expect("retained")
            .body,
        r#"{"answer":"original"}"#
    );
}
