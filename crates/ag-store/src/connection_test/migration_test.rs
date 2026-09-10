use ag_session::{SessionMessageKind, SettingName};
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;

use super::support::insert_session_fixture;
use crate::connection::Database;

/// Verifies the current schema no longer persists agent session summaries.
#[tokio::test]
async fn test_session_schema_omits_summary_column() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");

    // Act
    let column_names =
        sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('session')")
            .fetch_all(database.pool())
            .await
            .expect("session columns should load");

    // Assert
    assert!(!column_names.iter().any(|name| name == "summary"));
}

/// Verifies the diff-presence migration preserves ambiguous legacy rows.
#[tokio::test]
async fn test_add_session_diff_presence_backfills_legacy_rows() {
    // Arrange
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("failed to open pre-migration database");
    sqlx::query!(
        r"
CREATE TABLE IF NOT EXISTS session (
    id TEXT PRIMARY KEY NOT NULL,
    added_lines INTEGER NOT NULL DEFAULT 0,
    deleted_lines INTEGER NOT NULL DEFAULT 0
)
"
    )
    .execute(&pool)
    .await
    .expect("failed to create pre-migration session table");
    sqlx::query!(
        r"
INSERT INTO session (id, added_lines, deleted_lines)
VALUES ('legacy-clean', 0, 0),
       ('legacy-added', 3, 0),
       ('legacy-deleted', 0, 2)
"
    )
    .execute(&pool)
    .await
    .expect("failed to seed pre-migration sessions");

    // Act
    rerun_embedded_migration(&pool, 61).await;
    let rows = sqlx::query!(
        r#"
SELECT id, has_diff AS "has_diff: bool"
FROM session
ORDER BY id
"#
    )
    .fetch_all(&pool)
    .await
    .expect("failed to load migrated diff presence")
    .into_iter()
    .map(|row| (row.id, row.has_diff))
    .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        rows,
        vec![
            ("legacy-added".to_string(), Some(true)),
            ("legacy-clean".to_string(), None),
            ("legacy-deleted".to_string(), Some(true)),
        ]
    );
}

/// Verifies legacy transcript checkpoints keep their old collapse semantics
/// before becoming workflow notices.
#[tokio::test]
async fn test_convert_legacy_transcript_messages_keeps_latest_checkpoint() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    insert_session_fixture(&database, "session-b", "main", "Review", project_id).await;
    insert_session_message_row(&database, "session-a", 0, "user_prompt", "old prompt").await;
    insert_session_message_row(&database, "session-a", 1, "assistant_answer", "old answer").await;
    insert_session_message_row(
        &database,
        "session-a",
        2,
        "legacy_transcript",
        "old prompt\nold answer\n",
    )
    .await;
    insert_session_message_row(&database, "session-a", 3, "assistant_answer", "new answer").await;
    insert_session_message_row(&database, "session-b", 0, "transcript_chunk", "chunk text").await;

    // Act
    rerun_embedded_migration(database.pool(), 55).await;

    // Assert
    let checkpoint_messages = database
        .sessions()
        .load_session_messages("session-a")
        .await
        .expect("failed to load session-a messages");
    assert_eq!(checkpoint_messages.len(), 2);
    assert_eq!(checkpoint_messages[0].position, 2);
    assert_eq!(
        checkpoint_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(checkpoint_messages[0].content, "old prompt\nold answer\n");
    assert_eq!(checkpoint_messages[1].position, 3);
    assert_eq!(
        checkpoint_messages[1].kind,
        SessionMessageKind::AssistantAnswer.as_str()
    );
    assert_eq!(checkpoint_messages[1].content, "new answer");

    let chunk_messages = database
        .sessions()
        .load_session_messages("session-b")
        .await
        .expect("failed to load session-b messages");
    assert_eq!(chunk_messages.len(), 1);
    assert_eq!(
        chunk_messages[0].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(chunk_messages[0].content, "chunk text");
}

#[tokio::test]
async fn test_migrate_hacker_theme_to_green_preserves_theme_selection() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    database
        .settings()
        .upsert_setting(SettingName::Theme, "hacker")
        .await
        .expect("failed to persist legacy theme setting");

    // Act
    rerun_embedded_migration(database.pool(), 60).await;
    let theme = database
        .settings()
        .get_setting(SettingName::Theme)
        .await
        .expect("failed to load migrated theme setting");

    // Assert
    assert_eq!(theme, Some("green".to_string()));
}

#[tokio::test]
async fn test_split_default_reasoning_level_migrates_each_project_role() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    sqlx::query(
        r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, 'ReasoningLevel', 'xhigh')
",
    )
    .bind(project_id)
    .execute(database.pool())
    .await
    .expect("failed to seed legacy project reasoning level");
    sqlx::query(
        r"
INSERT INTO setting (name, value)
VALUES ('ReasoningLevel', 'medium')
",
    )
    .execute(database.pool())
    .await
    .expect("failed to seed legacy global reasoning level");

    // Act
    rerun_embedded_migration(database.pool(), 70).await;
    let migrated_rows = load_project_setting_rows(&database, project_id).await;
    let legacy_global_reasoning_level = load_legacy_global_reasoning_level(&database).await;

    // Assert
    assert_eq!(
        migrated_rows,
        vec![
            (
                SettingName::DefaultFastReasoningLevel.as_str().to_string(),
                "xhigh".to_string()
            ),
            (
                SettingName::DefaultReviewReasoningLevel
                    .as_str()
                    .to_string(),
                "xhigh".to_string()
            ),
            (
                SettingName::DefaultSmartReasoningLevel.as_str().to_string(),
                "xhigh".to_string()
            ),
        ]
    );
    assert_eq!(legacy_global_reasoning_level, None);
}

#[tokio::test]
async fn test_split_default_reasoning_level_migrates_global_only_value() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/global-only-project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    sqlx::query(
        r"
INSERT INTO setting (name, value)
VALUES ('ReasoningLevel', 'medium')
",
    )
    .execute(database.pool())
    .await
    .expect("failed to seed legacy global reasoning level");

    // Act
    rerun_embedded_migration(database.pool(), 70).await;
    let migrated_rows = load_project_setting_rows(&database, project_id).await;
    let legacy_global_reasoning_level = load_legacy_global_reasoning_level(&database).await;

    // Assert
    assert_eq!(
        migrated_rows,
        vec![
            (
                SettingName::DefaultFastReasoningLevel.as_str().to_string(),
                "medium".to_string()
            ),
            (
                SettingName::DefaultReviewReasoningLevel
                    .as_str()
                    .to_string(),
                "medium".to_string()
            ),
            (
                SettingName::DefaultSmartReasoningLevel.as_str().to_string(),
                "medium".to_string()
            ),
        ]
    );
    assert_eq!(legacy_global_reasoning_level, None);
}

#[tokio::test]
async fn test_remove_session_wall_clock_triggers_drops_legacy_timestamp_policy() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    seed_legacy_wall_clock_schema(&database).await;

    // Act
    rerun_embedded_migration(database.pool(), 71).await;
    let trigger_count = sqlx::query_scalar::<_, i64>(
        r"
SELECT COUNT(*)
FROM sqlite_master
WHERE type = 'trigger'
  AND name IN ('update_session_insert_timestamps', 'update_session_updated_at')
",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to count legacy timestamp triggers");
    let usage_created_at = sqlx::query_scalar::<_, i64>(
        "SELECT created_at FROM session_usage WHERE session_id = 'session-a'",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load migrated usage row");
    let usage_created_at_default = sqlx::query_scalar::<_, Option<String>>(
        r"
SELECT dflt_value
FROM pragma_table_info('session_usage')
WHERE name = 'created_at'
",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed to load usage timestamp default");

    // Assert
    assert_eq!(trigger_count, 0);
    assert_eq!(usage_created_at, 123);
    assert_eq!(usage_created_at_default, None);
}

#[tokio::test]
async fn review_diff_baseline_migration_preserves_existing_review_hashes() {
    // Arrange
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("database should open");
    sqlx::raw_sql(
        "CREATE TABLE session (id TEXT, focused_review_diff_hash TEXT); INSERT INTO session \
         VALUES ('reviewed', '42'), ('empty', NULL);",
    )
    .execute(&pool)
    .await
    .expect("legacy rows should exist");

    // Act
    rerun_embedded_migration(&pool, 80).await;
    let rows = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, review_diff_hash FROM session ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("migrated baselines should load");

    // Assert
    assert_eq!(
        rows,
        vec![
            ("empty".to_string(), None),
            ("reviewed".to_string(), Some("42".to_string()))
        ]
    );
}

/// Inserts one raw session-message row for migration compatibility tests.
async fn insert_session_message_row(
    database: &Database,
    session_id: &str,
    position: i64,
    kind: &str,
    content: &str,
) {
    sqlx::query!(
        r"
INSERT INTO session_message (session_id, position, kind, content)
VALUES (?, ?, ?, ?)
",
        session_id,
        position,
        kind,
        content
    )
    .execute(database.pool())
    .await
    .expect("failed to insert raw session message row");
}

/// Reapplies one embedded migration under an isolated test tracking table.
async fn rerun_embedded_migration(pool: &SqlitePool, version: i64) {
    let migration = sqlx::migrate!("./migrations")
        .iter()
        .find(|migration| migration.version == version)
        .cloned()
        .expect("embedded migration should exist");
    let mut migrator = Migrator::with_migrations(vec![migration]);
    migrator.dangerous_set_table_name(format!("_sqlx_test_migrations_{version}"));

    migrator
        .run(pool)
        .await
        .expect("embedded migration should run");
}

/// Loads all project settings in deterministic name order for migration
/// assertions.
async fn load_project_setting_rows(database: &Database, project_id: i64) -> Vec<(String, String)> {
    sqlx::query_as::<_, (String, String)>(
        r"
SELECT name, value
FROM project_setting
WHERE project_id = ?
ORDER BY name
",
    )
    .bind(project_id)
    .fetch_all(database.pool())
    .await
    .expect("failed to load project settings")
}

/// Loads the legacy global reasoning value for migration cleanup assertions.
async fn load_legacy_global_reasoning_level(database: &Database) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        r"
SELECT value
FROM setting
WHERE name = 'ReasoningLevel'
",
    )
    .fetch_optional(database.pool())
    .await
    .expect("failed to load legacy global reasoning level")
}

async fn seed_legacy_wall_clock_schema(database: &Database) {
    let project_id = database
        .projects()
        .upsert_project("/tmp/clock-migration", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    sqlx::query("DROP TABLE session_usage")
        .execute(database.pool())
        .await
        .expect("failed to drop current usage table");
    sqlx::query(
        r"
CREATE TABLE session_usage (
    session_id TEXT REFERENCES session(id) ON DELETE SET NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    input_tokens INTEGER NOT NULL DEFAULT 0,
    invocation_count INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    UNIQUE(session_id, model)
)
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy usage table");
    sqlx::query(
        r"
CREATE INDEX session_usage_session_id_idx ON session_usage (session_id)
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy usage index");
    sqlx::query(
        r"
INSERT INTO session_usage (
    session_id, model, created_at, input_tokens, invocation_count, output_tokens
)
VALUES ('session-a', 'gpt-5.6-sol', 123, 3, 1, 5)
",
    )
    .execute(database.pool())
    .await
    .expect("failed to seed legacy usage row");
    sqlx::query(
        r"
CREATE TRIGGER update_session_insert_timestamps
AFTER INSERT ON session
BEGIN
    UPDATE session SET updated_at = unixepoch() WHERE rowid = NEW.rowid;
END
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy insert trigger");
    sqlx::query(
        r"
CREATE TRIGGER update_session_updated_at
AFTER UPDATE ON session
BEGIN
    SELECT unixepoch();
END
",
    )
    .execute(database.pool())
    .await
    .expect("failed to recreate legacy update trigger");
}
