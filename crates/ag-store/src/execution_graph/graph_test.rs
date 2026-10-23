use std::num::NonZeroU64;

use ag_protocol::TurnPromptAttachment;
use ag_session::EnvId;

use super::support_test::{graph, produced};
use crate::connection::{Database, MIGRATOR};
use crate::test_support::task_database_fixture;

#[tokio::test]
async fn migration_preserves_existing_tasks_and_sessions() {
    // Arrange
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(crate::connection::in_memory_connect_options())
        .await
        .expect("pool");
    let prior = sqlx::migrate::Migrator::with_migrations(
        MIGRATOR
            .iter()
            .filter(|migration| migration.version < 102)
            .cloned()
            .collect(),
    );
    prior.run(&pool).await.expect("previous schema");
    let database = Database::from_pool(pool, std::sync::Arc::new(|| 123));
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
        .create_task(
            "session",
            &ag_session::CreateTaskRequest {
                task_id: "task".into(),
                objective: "Existing outcome".into(),
            },
        )
        .await
        .expect("task");
    let before = database.tasks().load_task("task").await.expect("before");

    // Act
    MIGRATOR.run(database.pool()).await.expect("upgrade");

    // Assert
    assert_eq!(
        database.tasks().load_task("task").await.expect("after"),
        before
    );
    assert_eq!(
        database
            .graphs()
            .list_graphs("session")
            .await
            .expect("empty"),
        Vec::new()
    );
    assert!(
        database
            .graphs()
            .save_graph(&graph(), None)
            .await
            .expect("graph after upgrade")
    );
}

#[tokio::test]
async fn revisions_are_atomic_immutable_ordered_and_session_scoped() {
    // Arrange
    let database = task_database_fixture().await;
    let mut original = graph();
    original.nodes[0]
        .request
        .attachments
        .push(TurnPromptAttachment {
            local_image_path: "reference.png".into(),
            placeholder: "[Image #1]".into(),
        });
    let mut revised = original.clone();
    revised.revision = NonZeroU64::new(2).expect("revision");
    revised.nodes[0].definition.prompt = "Revised instructions".into();

    // Act
    assert!(
        database
            .graphs()
            .save_graph(&original, None)
            .await
            .expect("create")
    );
    assert!(
        database
            .graphs()
            .save_graph(&revised, Some(original.revision))
            .await
            .expect("revise")
    );
    assert!(
        !database
            .graphs()
            .save_graph(&original, None)
            .await
            .expect("old redelivery")
    );
    assert!(
        !database
            .graphs()
            .save_graph(&revised, Some(original.revision))
            .await
            .expect("redelivery")
    );
    let historical = database
        .graphs()
        .load_graph("graph", Some(original.revision))
        .await
        .expect("history");
    let current = database
        .graphs()
        .load_graph("graph", None)
        .await
        .expect("current");

    // Assert
    assert_eq!(historical, Some(original));
    assert_eq!(current, Some(revised.clone()));
    assert_eq!(
        database
            .graphs()
            .list_graphs("session")
            .await
            .expect("list"),
        vec![revised]
    );
    assert_eq!(
        database.graphs().list_graphs("other").await.expect("other"),
        Vec::new()
    );
    assert!(
        database
            .graphs()
            .load_graph("absent", None)
            .await
            .expect("missing")
            .is_none()
    );
    assert!(
        database
            .graphs()
            .load_graph("graph", NonZeroU64::new(3))
            .await
            .expect("missing revision")
            .is_none()
    );
}

#[tokio::test]
async fn invalid_revisions_never_change_the_head_or_leave_partial_records() {
    // Arrange
    let database = task_database_fixture().await;
    let original = graph();
    database
        .graphs()
        .save_graph(&original, None)
        .await
        .expect("graph");
    let mut cases = Vec::new();
    let mut conflicting = original.clone();
    conflicting.nodes[0].objective = "Different".into();
    cases.push((conflicting, None));
    for (revision, expected) in [(2, None), (3, Some(1)), (2, Some(2)), (u64::MAX, Some(1))] {
        let mut revised = original.clone();
        revised.revision = NonZeroU64::new(revision).expect("revision");
        cases.push((revised, expected.and_then(NonZeroU64::new)));
    }
    let mut foreign = original.clone();
    foreign.revision = NonZeroU64::new(2).expect("revision");
    foreign.session_id = "other".into();
    for node in &mut foreign.nodes {
        node.env_id = EnvId::SessionWorktree("other".into());
    }
    cases.push((foreign, Some(original.revision)));
    let mut invalid = original.clone();
    invalid.id = "invalid".into();
    invalid.nodes.clear();
    cases.push((invalid, None));
    let mut missing_session = original.clone();
    missing_session.id = "missing-session".into();
    missing_session.session_id = "missing".into();
    for node in &mut missing_session.nodes {
        node.env_id = EnvId::SessionWorktree("missing".into());
    }
    cases.push((missing_session, None));
    for (revision, expected) in [(1, Some(1)), (2, None)] {
        let mut new_graph = original.clone();
        new_graph.id = "new".into();
        new_graph.revision = NonZeroU64::new(revision).expect("revision");
        cases.push((new_graph, expected.and_then(NonZeroU64::new)));
    }

    // Act / Assert
    for (invalid, expected) in cases {
        assert!(
            database
                .graphs()
                .save_graph(&invalid, expected)
                .await
                .is_err(),
            "{invalid:?}"
        );
        assert_eq!(
            database
                .graphs()
                .list_graphs("session")
                .await
                .expect("unchanged"),
            vec![original.clone()]
        );
    }
    assert!(
        database
            .graphs()
            .load_graph("new", None)
            .await
            .expect("rolled back")
            .is_none()
    );
}

#[tokio::test]
async fn executions_freeze_revisions_and_corrupt_snapshots_fail_closed() {
    // Arrange
    let database = produced().await;
    let mut revised = graph();
    revised.revision = NonZeroU64::new(2).expect("revision");

    // Act / Assert
    assert!(
        database
            .graphs()
            .save_graph(&revised, Some(NonZeroU64::MIN))
            .await
            .is_err()
    );
    for snapshot in ["not json", "{}"] {
        sqlx::query("UPDATE execution_graph_revision SET snapshot = ?")
            .bind(snapshot)
            .execute(database.pool())
            .await
            .expect("corrupt");
        assert!(database.graphs().load_graph("graph", None).await.is_err());
        assert!(database.graphs().list_graphs("session").await.is_err());
    }
    let mut invalid = graph();
    invalid.nodes.clear();
    sqlx::query("UPDATE execution_graph_revision SET snapshot = ?")
        .bind(serde_json::json!(invalid).to_string())
        .execute(database.pool())
        .await
        .expect("invalid");
    assert!(database.graphs().load_graph("graph", None).await.is_err());
    assert!(database.graphs().list_graphs("session").await.is_err());
}

#[tokio::test]
async fn failed_revision_insert_rolls_back_the_head() {
    // Arrange
    let database = task_database_fixture().await;
    let original = graph();
    database
        .graphs()
        .save_graph(&original, None)
        .await
        .expect("graph");
    sqlx::query(
        "CREATE TRIGGER reject_graph_revision BEFORE INSERT ON execution_graph_revision BEGIN \
         SELECT RAISE(ABORT, 'injected'); END",
    )
    .execute(database.pool())
    .await
    .expect("trigger");
    let mut revised = original.clone();
    revised.revision = NonZeroU64::new(2).expect("revision");

    // Act
    let result = database
        .graphs()
        .save_graph(&revised, Some(original.revision))
        .await;

    // Assert
    assert!(result.is_err());
    assert_eq!(
        database
            .graphs()
            .load_graph("graph", None)
            .await
            .expect("head"),
        Some(original)
    );
}
