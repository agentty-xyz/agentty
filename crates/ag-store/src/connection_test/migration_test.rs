use ag_session::{SessionMessageKind, SettingName};
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;

use super::support::insert_session_fixture;
use crate::connection::Database;

#[tokio::test]
async fn task_context_migration_preserves_unassigned_task_history() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    sqlx::query("DROP TABLE task_agent_context")
        .execute(database.pool())
        .await
        .expect("previous schema");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    insert_session_fixture(&database, "legacy-context", "main", "Review", project).await;
    let request = ag_session::SubmitTaskRequest {
        attempt_id: "legacy-attempt".to_string(),
        message: "ordinary task input".to_string(),
        objective: "existing outcome".to_string(),
        task_id: "legacy-task".to_string(),
        visibility: ag_session::DurableMessageVisibility::Visible,
    };
    // Insert the pre-context records directly: current admission also reads
    // the context table, which does not exist in the previous schema.
    sqlx::query("INSERT INTO task (id, session_id, objective, created_at) VALUES (?, ?, ?, 1)")
        .bind(&request.task_id)
        .bind("legacy-context")
        .bind(&request.objective)
        .execute(database.pool())
        .await
        .expect("legacy task");
    database
        .operations()
        .insert_session_operation(&request.operation_id(), "legacy-context", "reply")
        .await
        .expect("operation");
    sqlx::query(
        "INSERT INTO task_attempt (id, task_id, session_operation_id, message, is_visible) VALUES \
         (?, ?, ?, ?, 1)",
    )
    .bind(&request.attempt_id)
    .bind(&request.task_id)
    .bind(request.operation_id())
    .bind(&request.message)
    .execute(database.pool())
    .await
    .expect("legacy attempt");
    let before = database
        .tasks()
        .load_task(&request.task_id)
        .await
        .expect("history");

    // Act
    rerun_embedded_migration(database.pool(), 93).await;
    let after = database
        .tasks()
        .load_task(&request.task_id)
        .await
        .expect("history");
    let duplicate = database
        .tasks()
        .contains_attempt("legacy-context", &request)
        .await
        .expect("legacy delivery");

    // Assert
    assert_eq!(after, before);
    assert!(duplicate);
    assert_eq!(
        database
            .tasks()
            .load_agent_context(&request.task_id)
            .await
            .expect("no assignment"),
        None
    );
}

#[tokio::test]
async fn task_migration_preserves_existing_sessions_and_operation_retry_semantics() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    sqlx::raw_sql("DROP TRIGGER task_attempt_no_reopen; DROP TABLE task_attempt; DROP TABLE task;")
        .execute(database.pool())
        .await
        .expect("prior schema");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    insert_session_fixture(&database, "legacy", "main", "Review", project).await;
    database
        .operations()
        .insert_session_operation("legacy-operation", "legacy", "reply")
        .await
        .expect("operation");
    database
        .operations()
        .mark_session_operation_failed("legacy-operation", "retryable")
        .await
        .expect("failure");
    let before = database
        .sessions()
        .load_session("legacy")
        .await
        .expect("session")
        .expect("present");

    // Act
    rerun_embedded_migration(database.pool(), 92).await;
    let after = database
        .sessions()
        .load_session("legacy")
        .await
        .expect("session")
        .expect("present");
    let claimed = database
        .operations()
        .claim_session_operation("legacy-operation", "legacy", "reply")
        .await
        .expect("legacy retry");

    // Assert
    assert_eq!(after.id, before.id);
    assert_eq!(after.status, before.status);
    assert_eq!(after.project_id, before.project_id);
    assert_eq!(after.model, before.model);
    assert!(claimed);
    assert_eq!(
        database
            .tasks()
            .load_task("legacy")
            .await
            .expect("no backfill"),
        None
    );
}

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

/// Verifies Sonnet retirement updates saved selections without changing other
/// settings.
#[tokio::test]
async fn test_migrate_sonnet_5_to_55_updates_sessions_and_model_settings() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/sonnet-migration", None)
        .await
        .expect("project should exist");
    for (session_id, model) in [
        ("legacy", "claude-sonnet-5"),
        ("current", "claude-sonnet-5-5"),
        ("other", "claude-opus-5-5"),
    ] {
        database
            .sessions()
            .insert_session(session_id, model, "main", "Review", project_id)
            .await
            .expect("session should exist");
    }
    let setting_names = [
        SettingName::DefaultSmartModel,
        SettingName::DefaultFastModel,
        SettingName::DefaultReviewModel,
        SettingName::Theme,
    ];
    for name in setting_names {
        database
            .settings()
            .upsert_setting(name, "claude-sonnet-5")
            .await
            .expect("global setting should exist");
        database
            .settings()
            .upsert_project_setting(project_id, name, "claude-sonnet-5")
            .await
            .expect("project setting should exist");
    }

    // Act
    rerun_embedded_migration(database.pool(), 88).await;
    let sessions =
        sqlx::query_as::<_, (String, String)>("SELECT id, model FROM session ORDER BY id")
            .fetch_all(database.pool())
            .await
            .expect("sessions should load");
    let global_settings =
        sqlx::query_as::<_, (String, String)>("SELECT name, value FROM setting ORDER BY name")
            .fetch_all(database.pool())
            .await
            .expect("global settings should load");
    let project_settings = load_project_setting_rows(&database, project_id).await;

    // Assert
    assert_eq!(
        sessions,
        vec![
            ("current".to_string(), "claude-sonnet-5-5".to_string()),
            ("legacy".to_string(), "claude-sonnet-5-5".to_string()),
            ("other".to_string(), "claude-opus-5-5".to_string())
        ]
    );
    let expected_settings = vec![
        (
            "DefaultFastModel".to_string(),
            "claude-sonnet-5-5".to_string(),
        ),
        (
            "DefaultReviewModel".to_string(),
            "claude-sonnet-5-5".to_string(),
        ),
        (
            "DefaultSmartModel".to_string(),
            "claude-sonnet-5-5".to_string(),
        ),
        ("Theme".to_string(), "claude-sonnet-5".to_string()),
    ];
    assert_eq!(global_settings, expected_settings);
    assert_eq!(project_settings, expected_settings);
}

/// Verifies merged Sonnet usage keeps all counters and the earliest timestamp.
#[tokio::test]
async fn test_migrate_sonnet_5_to_55_consolidates_usage_without_losing_history() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/sonnet-usage", None)
        .await
        .expect("project should exist");
    for session_id in ["legacy", "mixed", "current"] {
        insert_session_fixture(&database, session_id, "main", "Review", project_id).await;
    }
    sqlx::raw_sql(
        "INSERT INTO session_usage (session_id, model, created_at, input_tokens, \
         invocation_count, output_tokens) VALUES
        ('legacy', 'claude-sonnet-5', 30, 10, 1, 20),
        ('mixed', 'claude-sonnet-5', 20, 100, 2, 200),
        ('mixed', 'claude-sonnet-5-5', 10, 300, 3, 400),
        ('mixed', 'claude-opus-5-5', 40, 50, 4, 60),
        ('current', 'claude-sonnet-5-5', 50, 70, 5, 80),
        (NULL, 'claude-sonnet-5', 60, 90, 6, 100),
        (NULL, 'claude-sonnet-5-5', 70, 110, 7, 120);",
    )
    .execute(database.pool())
    .await
    .expect("usage rows should exist");

    // Act
    rerun_embedded_migration(database.pool(), 88).await;
    let usage = sqlx::query_as::<_, (Option<String>, String, i64, i64, i64, i64)>(
        "SELECT session_id, model, created_at, input_tokens, invocation_count, output_tokens FROM \
         session_usage ORDER BY session_id, model, created_at",
    )
    .fetch_all(database.pool())
    .await
    .expect("usage should load");

    // Assert
    assert_eq!(
        usage,
        vec![
            (None, "claude-sonnet-5-5".to_string(), 60, 90, 6, 100),
            (None, "claude-sonnet-5-5".to_string(), 70, 110, 7, 120),
            (
                Some("current".to_string()),
                "claude-sonnet-5-5".to_string(),
                50,
                70,
                5,
                80
            ),
            (
                Some("legacy".to_string()),
                "claude-sonnet-5-5".to_string(),
                30,
                10,
                1,
                20
            ),
            (
                Some("mixed".to_string()),
                "claude-opus-5-5".to_string(),
                40,
                50,
                4,
                60
            ),
            (
                Some("mixed".to_string()),
                "claude-sonnet-5-5".to_string(),
                10,
                400,
                5,
                600
            ),
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

/// Retires campaigns without losing session history or granting researchers
/// write access, and prevents pending coordinator work from being replayed.
#[tokio::test]
async fn retirement_preserves_sessions_and_archives_campaign_results() {
    // Arrange
    let pool = legacy_campaign_database().await;

    // Act
    rerun_embedded_migration(&pool, 90).await;
    let sessions = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT id, status, permission_mode, questions FROM session ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("retained sessions");
    let messages = sqlx::query_as::<_, (String, String)>(
        "SELECT kind, content FROM session_message WHERE session_id = 'controller' ORDER BY \
         position",
    )
    .fetch_all(&pool)
    .await
    .expect("retained history");
    let operations = sqlx::query_as::<_, (String, String)>(
        "SELECT id, status FROM session_operation ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("operation states");
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .expect("foreign keys");
    let columns = sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('session')")
        .fetch_all(&pool)
        .await
        .expect("session columns");
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE name LIKE 'session_orchestration%'",
    )
    .fetch_all(&pool)
    .await
    .expect("retired tables");
    let settings = sqlx::query_scalar::<_, String>("SELECT name FROM setting")
        .fetch_all(&pool)
        .await
        .expect("settings");

    // Assert
    assert_eq!(sessions.len(), 4);
    assert_eq!(
        sessions[0],
        (
            "controller".into(),
            "Review".into(),
            "read_only".into(),
            None
        )
    );
    assert_eq!(
        sessions[1],
        (
            "ordinary".into(),
            "Question".into(),
            "auto_edit".into(),
            Some("[\"Question?\"]".into())
        )
    );
    assert_eq!(sessions[2].1, "Canceled");
    assert_eq!(sessions[2].2, "read_only");
    assert_eq!(sessions[3].1, "Review");
    assert_eq!(sessions[3].2, "auto_edit");
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0],
        ("user_prompt".into(), "Original objective".into())
    );
    assert_eq!(messages[1].0, "workflow_notice");
    assert!(messages[1].1.contains("Retained findings"));
    assert!(messages[1].1.contains("Inspect code"));
    assert_eq!(
        operations,
        [
            ("old-run".into(), "failed".into()),
            ("user-run".into(), "queued".into())
        ]
    );
    assert!(violations.is_empty());
    assert_eq!(tables, [] as [String; 0]);
    assert!(
        !columns
            .iter()
            .any(|column| matches!(column.as_str(), "role" | "orchestration_task_id"))
    );
    assert_eq!(settings, ["Theme"]);
}

/// Builds a pre-retirement database containing a linked campaign and user work.
async fn legacy_campaign_database() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("legacy database");
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .expect("foreign keys");
    let migrations = sqlx::migrate!("./migrations");
    for migration in migrations.iter().filter(|migration| migration.version < 90) {
        sqlx::raw_sql(migration.sql.clone())
            .execute(&pool)
            .await
            .expect("legacy migration");
    }
    sqlx::raw_sql(
        "INSERT INTO project (id, path) VALUES (1, 'project');
         INSERT INTO session (id, model, base_branch, status, project_id, role, is_draft, \
         permission_mode)
         VALUES ('controller', 'gpt-5.6-sol', 'main', 'Question', 1, 'Orchestrator', 0, \
         'auto_edit'),
                ('worker', 'gpt-5.6-sol', 'main', 'InProgress', 1, 'OrchestrationWorker', 0, \
         'auto_edit'),
                ('researcher', 'gpt-5.6-sol', 'main', 'Canceled', 1, 'OrchestrationResearcher', 0, \
         'auto_edit'),
                ('ordinary', 'gpt-5.6-sol', 'main', 'Question', 1, NULL, 0, 'auto_edit');
         UPDATE session SET questions = '[\"Question?\"]';
         INSERT INTO session_message (session_id, position, kind, content)
         VALUES ('controller', 0, 'user_prompt', 'Original objective');
         INSERT INTO session_orchestration (id, controller_session_id, status, max_parallelism, \
         goal_statement)
         VALUES (1, 'controller', 'Running', 3, 'Original objective');
         INSERT INTO session_orchestration_task
         (id, session_orchestration_id, task_key, child_session_id, title, prompt, status, \
         research_report)
         VALUES (1, 1, 'research', 'researcher', 'Research', 'Inspect code', 'Reported', 'Retained \
         findings');
         UPDATE session SET orchestration_task_id = 1 WHERE id = 'researcher';
         UPDATE session_orchestration SET relayed_question_task_id = 1;
         INSERT INTO session_operation (id, session_id, kind, status, queued_at)
         VALUES ('old-run', 'worker', 'reply', 'running', 1),
                ('user-run', 'ordinary', 'reply', 'queued', 1);
         INSERT INTO setting (name, value) VALUES ('OrchestrationParallelism', '3'), ('Theme', \
         'green');",
    )
    .execute(&pool)
    .await
    .expect("legacy campaign fixtures");

    pool
}

/// Saved campaign prompts become history while ordinary preparation stays
/// resumable.
#[tokio::test]
async fn retirement_stops_saved_campaign_prompts_without_discarding_them() {
    // Arrange
    let pool = legacy_campaign_database().await;
    sqlx::raw_sql(
        "INSERT INTO session_preparation (session_id, state, start_ref, prompt)
        VALUES ('controller', 'preparing', 'main', 'Legacy planning prompt'),
               ('worker', 'ready', 'main', 'Legacy implementation prompt'),
               ('ordinary', 'ready', 'main', 'User prompt');",
    )
    .execute(&pool)
    .await
    .expect("saved prompts");

    // Act
    rerun_embedded_migration(&pool, 90).await;
    let preparations = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT session_id, state, prompt FROM session_preparation ORDER BY session_id",
    )
    .fetch_all(&pool)
    .await
    .expect("preparations");
    let archived_prompts = sqlx::query_scalar::<_, String>(
        "SELECT content FROM session_message WHERE content LIKE '%Unstarted prompt preserved:%' \
         ORDER BY session_id",
    )
    .fetch_all(&pool)
    .await
    .expect("archived prompts");

    // Assert
    assert_eq!(
        preparations,
        [
            ("controller".into(), "failed".into(), None),
            (
                "ordinary".into(),
                "ready".into(),
                Some("User prompt".into())
            ),
            ("worker".into(), "ready".into(), None),
        ]
    );
    assert_eq!(archived_prompts.len(), 2);
    assert!(archived_prompts[0].ends_with("Legacy planning prompt"));
    assert!(archived_prompts[1].ends_with("Legacy implementation prompt"));
}

/// Undispatched follow-ups remain inert history even when remediation was
/// claimed.
#[tokio::test]
async fn retirement_archives_pending_continuation_and_remediation_prompts() {
    // Arrange
    let pool = legacy_campaign_database().await;
    sqlx::raw_sql(
        "INSERT INTO session_orchestration_task
         (session_orchestration_id, task_key, child_session_id, title, prompt, status, \
         continuation_prompt)
         VALUES (1, 'follow-up', 'worker', 'Follow up', 'Original implementation',
                 'ContinuationPending', 'Undispatched follow-up instruction'),
                (1, 'remediation', NULL, 'Remediation', 'Original review',
                 'ReviewApplying', 'Claimed remediation instruction');",
    )
    .execute(&pool)
    .await
    .expect("pending instructions");

    // Act
    rerun_embedded_migration(&pool, 90).await;
    let history = sqlx::query_scalar::<_, String>(
        "SELECT content FROM session_message WHERE session_id = 'controller' AND kind = \
         'workflow_notice'",
    )
    .fetch_one(&pool)
    .await
    .expect("campaign archive");
    let archive = history
        .split_once("Campaign history: ")
        .expect("archive prefix")
        .1;
    let tasks = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT json_extract(value, '$.key'), json_extract(value, '$.status'),
                json_extract(value, '$.prompt'), json_extract(value, '$.continuation_prompt')
         FROM json_each(?, '$[0].tasks')
         WHERE json_extract(value, '$.continuation_prompt') IS NOT NULL ORDER BY 1",
    )
    .bind(archive)
    .fetch_all(&pool)
    .await
    .expect("archived instructions");

    // Assert
    assert_eq!(
        tasks,
        [
            (
                "follow-up".into(),
                "ContinuationPending".into(),
                "Original implementation".into(),
                "Undispatched follow-up instruction".into()
            ),
            (
                "remediation".into(),
                "ReviewApplying".into(),
                "Original review".into(),
                "Claimed remediation instruction".into()
            ),
        ]
    );
    let child_messages = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM session_message WHERE session_id = 'worker'",
    )
    .fetch_one(&pool)
    .await
    .expect("child history");
    assert_eq!(child_messages, 0);
}

/// Retirement closes open timers, including stale intervals, without charging
/// negative time or altering ordinary sessions.
#[tokio::test]
async fn retirement_closes_active_work_timers() {
    // Arrange
    let pool = legacy_campaign_database().await;
    let before: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&pool)
        .await
        .expect("retirement clock");
    sqlx::query(
        "UPDATE session SET in_progress_total_seconds = 17, in_progress_started_at = ? WHERE id \
         IN ('worker', 'ordinary')",
    )
    .bind(before - 30)
    .execute(&pool)
    .await
    .expect("open timers");
    sqlx::query(
        "UPDATE session SET in_progress_total_seconds = 7, in_progress_started_at = ? WHERE id = \
         'researcher'",
    )
    .bind(before + 86400)
    .execute(&pool)
    .await
    .expect("future timer");

    // Act
    rerun_embedded_migration(&pool, 90).await;
    let after: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&pool)
        .await
        .expect("retirement clock");
    let sessions = sqlx::query_as::<_, (String, String, i64, Option<i64>)>(
        "SELECT id, status, in_progress_total_seconds, in_progress_started_at FROM session ORDER \
         BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("retired timers");
    let operation = sqlx::query_as::<_, (String, i64)>(
        "SELECT status, finished_at FROM session_operation WHERE id = 'old-run'",
    )
    .fetch_one(&pool)
    .await
    .expect("retired operation");

    // Assert
    assert_eq!(sessions[0], ("controller".into(), "Review".into(), 0, None));
    assert_eq!(
        sessions[1],
        ("ordinary".into(), "Question".into(), 17, Some(before - 30))
    );
    assert_eq!(
        sessions[2],
        ("researcher".into(), "Canceled".into(), 7, None)
    );
    assert_eq!(sessions[3].1, "Review");
    assert!((47..=47 + after - before).contains(&sessions[3].2));
    assert_eq!(sessions[3].3, None);
    assert_eq!(operation.0, "failed");
    assert!((before..=after).contains(&operation.1));
}
