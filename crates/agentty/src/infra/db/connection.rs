//! `SQLite` connection setup for the database layer.

use std::ops::Deref;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use super::{AppRepositories, DbError};

/// Subdirectory under the agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default database filename.
pub const DB_FILE: &str = "agentty.db";

/// Maximum number of pooled `SQLite` connections for the on-disk database.
///
/// `SQLite` still serializes writes in WAL mode, so the pool stays small and
/// biased toward a handful of concurrent readers instead of a large number of
/// queued writer contenders.
pub(crate) const DB_POOL_MAX_CONNECTIONS: u32 = 4;

/// Thin wrapper around a `SQLite` connection pool.
#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    repositories: AppRepositories,
}

impl Database {
    /// Opens the `SQLite` database and runs embedded migrations.
    ///
    /// Uses up to `DB_POOL_MAX_CONNECTIONS` pooled connections so UI reads can
    /// stay responsive without oversizing the `SQLite` pool beyond what WAL
    /// can use effectively. Applies a short busy timeout so bursty reducer
    /// writes wait briefly for the single `SQLite` writer instead of failing
    /// immediately with `SQLITE_BUSY`.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the database
    /// cannot be opened, or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(2))
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(DB_POOL_MAX_CONNECTIONS)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        let repositories = AppRepositories::from_pool(pool.clone());

        Ok(Self { pool, repositories })
    }

    /// Opens an in-memory `SQLite` database and runs migrations.
    ///
    /// This is primarily used by tests and any ephemeral workflows that need
    /// an isolated database instance while keeping the same durability and
    /// foreign-key settings as the on-disk database. Applies the same short
    /// busy timeout as the on-disk configuration so tests exercise the same
    /// writer wait policy.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        let pool = open_in_memory_pool(1).await?;

        let repositories = AppRepositories::from_pool(pool.clone());

        Ok(Self { pool, repositories })
    }

    /// Returns the shared `SQLite` connection pool for lower-level query
    /// access.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

impl Deref for Database {
    type Target = AppRepositories;

    fn deref(&self) -> &Self::Target {
        &self.repositories
    }
}

impl From<Database> for AppRepositories {
    fn from(database: Database) -> Self {
        database.repositories
    }
}

/// Opens an in-memory `SQLite` pool with migrations applied.
///
/// The caller chooses the connection cap so tests and runtime code can share
/// the same setup logic while keeping their own concurrency requirements.
/// Applies the same 2-second busy timeout as the on-disk database so pooled
/// test connections exercise the same writer wait policy.
///
/// # Errors
/// Returns an error if the in-memory database connection or migrations fail.
pub(crate) async fn open_in_memory_pool(max_connections: u32) -> Result<SqlitePool, DbError> {
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .busy_timeout(Duration::from_secs(2))
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

/// Returns the current Unix timestamp in whole seconds.
pub(crate) fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::AgentModel;
    use crate::domain::agent::ReasoningLevel;
    use crate::domain::session::{
        DailyActivity, ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
        SessionStats,
    };
    use crate::domain::session_message::SessionMessageKind;
    use crate::domain::setting::SettingName;
    use crate::infra::db::{
        SessionFocusedReviewRow, SessionJoinRow, SessionOperationRow, SessionRow,
        SessionTurnMetadata,
    };
    /// Environment flag used to run the DST regression helper in an isolated
    /// subprocess with a fixed timezone.
    const DST_TEST_SUBPROCESS_ENV: &str = "AGENTTY_DST_TEST_SUBPROCESS";

    /// Builds one deterministic persisted review-request fixture for DB tests.
    fn review_request_fixture() -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: 456,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("2 approvals, checks passing".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        }
    }

    /// Asserts that one loaded session row carries the expected review-request
    /// linkage.
    fn assert_review_request_row(row: &SessionRow) {
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.display_id.as_str()),
            Some("#42")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.forge_kind.as_str()),
            Some("GitHub")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.last_refreshed_at),
            Some(456)
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.source_branch.as_str()),
            Some("feature/forge")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.state.as_str()),
            Some("Open")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .and_then(|review_request| review_request.status_summary.as_deref()),
            Some("2 approvals, checks passing")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.target_branch.as_str()),
            Some("main")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.title.as_str()),
            Some("Add forge review support")
        );
        assert_eq!(
            row.review_request
                .as_ref()
                .map(|review_request| review_request.web_url.as_str()),
            Some("https://github.com/agentty-xyz/agentty/pull/42")
        );
    }

    /// Inserts one session row with deterministic defaults for tests.
    async fn insert_session_fixture(
        database: &Database,
        session_id: &str,
        base_branch: &str,
        status: &str,
        project_id: i64,
    ) {
        database
            .sessions()
            .insert_session(session_id, "gpt-5.5", base_branch, status, project_id)
            .await
            .expect("failed to insert session fixture");
    }

    /// Loads one session row by identifier through `load_sessions()`.
    async fn load_session_row(database: &Database, session_id: &str) -> SessionRow {
        database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load all sessions")
            .into_iter()
            .find(|row| row.id == session_id)
            .expect("missing session row")
    }

    /// Loads one persisted session-operation row regardless of lifecycle
    /// status.
    async fn load_session_operation_row(
        database: &Database,
        operation_id: &str,
    ) -> SessionOperationRow {
        sqlx::query_as!(
            SessionOperationRow,
            r#"
SELECT id AS "id!", session_id AS "session_id!", kind AS "kind!", status AS "status!",
       queued_at, started_at, finished_at,
       heartbeat_at, last_error, cancel_requested AS "cancel_requested: _"
FROM session_operation
WHERE id = ?
"#,
            operation_id
        )
        .fetch_one(database.pool())
        .await
        .expect("failed to load session operation row")
    }

    /// Typed helper row used to verify nullable session references.
    struct SessionUsageSessionIdRow {
        session_id: Option<String>,
    }

    /// Builds one deterministic joined-session row fixture for conversion
    /// tests.
    fn session_join_row_fixture() -> SessionJoinRow {
        SessionJoinRow::fixture_for_test()
    }

    /// Verifies `open()` creates missing parent directories before opening the
    /// on-disk database.
    #[tokio::test]
    async fn test_open_creates_missing_parent_directory() {
        // Arrange
        let temp_dir = tempdir().expect("temp dir should be created");
        let db_path = temp_dir.path().join("nested").join("db").join(DB_FILE);

        // Act
        let database = Database::open(&db_path)
            .await
            .expect("database should open with missing parent directories");

        // Assert
        assert!(db_path.parent().is_some_and(std::path::Path::is_dir));
        assert!(!database.pool().is_closed());
    }

    /// Verifies `load_sessions()` maps persisted joined session fields.
    #[tokio::test]
    async fn test_load_sessions_maps_joined_session_fields() {
        // Arrange
        let (database, project_id) = database_with_joined_session_fields().await;

        // Act
        let session_row = load_session_row(&database, "session-a").await;

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.base_branch, "main");
        assert_eq!(session_row.created_at, 100);
        assert_eq!(session_row.updated_at, 200);
        assert_eq!(session_row.agent, "claude");
        assert_eq!(session_row.model, "claude-opus-4.1");
        assert_eq!(session_row.status, "Review");
        assert_eq!(session_row.in_progress_started_at, None);
        assert_eq!(session_row.in_progress_total_seconds, 120);
        assert_eq!(session_row.project_id, Some(project_id));
        assert_eq!(session_row.prompt, "Implement the feature");
        assert_eq!(session_row.added_lines, 14);
        assert_eq!(session_row.deleted_lines, 6);
        assert_eq!(session_row.input_tokens, 11);
        assert_eq!(session_row.output_tokens, 29);
        assert_eq!(session_row.parent_session_id, None);
        assert_eq!(session_row.size, "L");
        assert_eq!(
            session_row.summary.as_deref(),
            Some("Implemented the requested feature")
        );
        assert_eq!(session_row.questions.as_deref(), Some("[\"Need logs?\"]"));
        assert_eq!(session_row.title.as_deref(), Some("Feature work"));
        assert_eq!(
            session_row.published_upstream_ref.as_deref(),
            Some("origin/wt/session-a")
        );
        assert_review_request_row(&session_row);
    }

    /// Verifies message appends write ordered rows for the canonical
    /// transcript store without updating the legacy backup column.
    #[tokio::test]
    async fn test_append_session_message_writes_message_rows() {
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

        // Act
        database
            .sessions()
            .append_session_message("session-a", SessionMessageKind::UserPrompt, " hi ")
            .await
            .expect("failed to append prompt message");
        database
            .sessions()
            .append_session_message(
                "session-a",
                SessionMessageKind::AssistantAnswer,
                "\nHello\n",
            )
            .await
            .expect("failed to append assistant message");
        database
            .sessions()
            .append_session_message(
                "session-a",
                SessionMessageKind::WorkflowNotice,
                "\n[Sync Error] failed\n",
            )
            .await
            .expect("failed to append workflow notice");

        // Assert
        let messages = database
            .sessions()
            .load_session_messages("session-a")
            .await
            .expect("failed to load session messages");
        let detail = database
            .sessions()
            .load_session_detail("session-a")
            .await
            .expect("failed to load session detail")
            .expect("session detail should exist");
        assert!(detail.prompt.is_empty());
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].position, 0);
        assert_eq!(messages[0].kind, SessionMessageKind::UserPrompt.as_str());
        assert_eq!(messages[0].content, "hi");
        assert_eq!(messages[1].position, 1);
        assert_eq!(
            messages[1].kind,
            SessionMessageKind::AssistantAnswer.as_str()
        );
        assert_eq!(messages[1].content, "Hello");
        assert_eq!(messages[2].position, 2);
        assert_eq!(
            messages[2].kind,
            SessionMessageKind::WorkflowNotice.as_str()
        );
        assert_eq!(messages[2].content, "\n[Sync Error] failed\n");
    }

    /// Verifies canonical transcript appends refresh session list ordering
    /// metadata.
    #[tokio::test]
    async fn test_append_session_message_refreshes_session_updated_at() {
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
        database
            .sessions()
            .update_session_updated_at("session-a", 10)
            .await
            .expect("failed to backdate session updated_at");

        // Act
        database
            .sessions()
            .append_session_message(
                "session-a",
                SessionMessageKind::AssistantAnswer,
                "current answer",
            )
            .await
            .expect("failed to append assistant message");

        // Assert
        let (_, updated_at) = database
            .sessions()
            .load_session_timestamps("session-a")
            .await
            .expect("failed to load session timestamps")
            .expect("session timestamps should exist");
        assert!(
            updated_at > 10,
            "expected updated_at refresh, got {updated_at}"
        );
    }

    /// Verifies the legacy-output backfill appends to existing canonical
    /// transcript rows instead of replacing them.
    #[tokio::test]
    async fn test_session_message_backfill_preserves_existing_messages() {
        // Arrange
        let options = SqliteConnectOptions::new().filename(":memory:");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("failed to open in-memory pool");
        sqlx::query(
            r"
CREATE TABLE session (
    id TEXT PRIMARY KEY NOT NULL,
    output TEXT NOT NULL,
    created_at INTEGER NOT NULL
)
",
        )
        .execute(&pool)
        .await
        .expect("failed to create session table");
        sqlx::query(
            r"
CREATE TABLE session_message (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0
)
",
        )
        .execute(&pool)
        .await
        .expect("failed to create session_message table");
        sqlx::query(
            r"
INSERT INTO session (id, output, created_at)
VALUES ('session-a', 'legacy transcript with workflow notice', 100)
",
        )
        .execute(&pool)
        .await
        .expect("failed to insert session row");
        sqlx::query(
            r"
INSERT INTO session_message (session_id, position, kind, content, created_at)
VALUES
    ('session-a', 0, 'user_prompt', 'Prompt text', 101),
    ('session-a', 1, 'assistant_answer', 'Answer text', 102)
",
        )
        .execute(&pool)
        .await
        .expect("failed to insert existing messages");

        // Act
        sqlx::query(include_str!(
            "../../../migrations/050_backfill_session_message.sql"
        ))
        .execute(&pool)
        .await
        .expect("failed to run session_message backfill");

        // Assert
        let messages: Vec<(i64, String, String)> = sqlx::query_as(
            r"
SELECT position, kind, content
FROM session_message
WHERE session_id = 'session-a'
ORDER BY position
",
        )
        .fetch_all(&pool)
        .await
        .expect("failed to load backfilled messages");
        assert_eq!(
            messages,
            vec![
                (
                    0,
                    SessionMessageKind::UserPrompt.as_str().to_string(),
                    "Prompt text".to_string()
                ),
                (
                    1,
                    SessionMessageKind::AssistantAnswer.as_str().to_string(),
                    "Answer text".to_string()
                ),
                (
                    2,
                    SessionMessageKind::LegacyTranscript.as_str().to_string(),
                    "legacy transcript with workflow notice".to_string()
                ),
            ]
        );
    }

    /// Verifies session detail loads transcript metadata without reading
    /// legacy formatted output.
    #[tokio::test]
    async fn test_load_session_detail_omits_legacy_output() {
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
        database
            .sessions()
            .update_session_prompt("session-a", "Do something")
            .await
            .expect("failed to update prompt");
        database
            .sessions()
            .update_session_summary("session-a", "migrated summary")
            .await
            .expect("failed to update summary");

        // Act
        let detail = database
            .sessions()
            .load_session_detail("session-a")
            .await
            .expect("failed to load session detail")
            .expect("session detail should exist");

        // Assert
        assert_eq!(detail.prompt, "Do something");
        assert_eq!(detail.summary.as_deref(), Some("migrated summary"));
    }

    /// Builds an in-memory database with one session covering joined fields
    /// returned by `load_sessions()`.
    async fn database_with_joined_session_fields() -> (Database, i64) {
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        let review_request = review_request_fixture();

        insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
        persist_joined_session_metadata(&database, &review_request).await;
        persist_joined_session_state(&database).await;

        (database, project_id)
    }

    /// Persists metadata fields asserted by the joined-session mapping test.
    async fn persist_joined_session_metadata(database: &Database, review_request: &ReviewRequest) {
        database
            .sessions()
            .update_session_created_at("session-a", 100)
            .await
            .expect("failed to update session created_at");
        database
            .sessions()
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session updated_at");
        database
            .sessions()
            .update_session_diff_stats(14, 6, "session-a", "L")
            .await
            .expect("failed to update session diff stats");
        database
            .sessions()
            .update_session_questions("session-a", "[\"Need logs?\"]")
            .await
            .expect("failed to update session questions");
        database
            .sessions()
            .update_session_prompt("session-a", "Implement the feature")
            .await
            .expect("failed to update session prompt");
        database
            .sessions()
            .update_session_title("session-a", "Feature work")
            .await
            .expect("failed to update session title");
        database
            .sessions()
            .update_session_summary("session-a", "Implemented the requested feature")
            .await
            .expect("failed to update session summary");
        database
            .sessions()
            .update_session_stats(
                "session-a",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to update session stats");
        database
            .sessions()
            .update_session_model("session-a", "claude-opus-4.1")
            .await
            .expect("failed to update session model");
        database
            .sessions()
            .update_session_published_upstream_ref(
                "session-a",
                Some("origin/wt/session-a".to_string()),
            )
            .await
            .expect("failed to update published upstream ref");
        database
            .reviews()
            .update_session_review_request("session-a", Some(review_request.clone()))
            .await
            .expect("failed to update review request");
    }

    /// Persists timing fields asserted by the joined-session mapping test.
    async fn persist_joined_session_state(database: &Database) {
        database
            .sessions()
            .update_session_status_with_timing_at("session-a", "InProgress", 50)
            .await
            .expect("failed to open in-progress timing window");
        database
            .sessions()
            .update_session_status_with_timing_at("session-a", "Review", 170)
            .await
            .expect("failed to close in-progress timing window");
        database
            .sessions()
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session updated_at");
    }

    /// Verifies generated titles only overwrite the session title when the
    /// staged prompt has not changed since generation started.
    #[tokio::test]
    async fn test_update_session_title_for_prompt_requires_matching_prompt() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;
        database
            .sessions()
            .update_session_prompt("session-a", "First draft")
            .await
            .expect("failed to persist first staged prompt");
        database
            .sessions()
            .update_session_title("session-a", "First draft")
            .await
            .expect("failed to persist fallback title");

        // Act
        let stale_update_applied = database
            .sessions()
            .update_session_title_for_prompt(
                "session-a",
                "Second draft",
                "Refine draft workflow title",
            )
            .await
            .expect("failed to reject stale title update");
        let matching_update_applied = database
            .sessions()
            .update_session_title_for_prompt(
                "session-a",
                "First draft",
                "Refine draft workflow title",
            )
            .await
            .expect("failed to apply matching title update");

        // Assert
        let session_row = load_session_row(&database, "session-a").await;
        assert!(!stale_update_applied);
        assert!(matching_update_applied);
        assert_eq!(
            session_row.title.as_deref(),
            Some("Refine draft workflow title")
        );
    }

    /// Verifies timing-aware status transitions accumulate repeated
    /// `InProgress` intervals.
    #[tokio::test]
    async fn test_update_session_status_with_timing_at_accumulates_repeated_intervals() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;

        // Act
        database
            .sessions()
            .update_session_status_with_timing_at("session-a", "InProgress", 10)
            .await
            .expect("failed to enter in-progress the first time");
        database
            .sessions()
            .update_session_status_with_timing_at("session-a", "Review", 70)
            .await
            .expect("failed to leave in-progress the first time");
        database
            .sessions()
            .update_session_status_with_timing_at("session-a", "InProgress", 100)
            .await
            .expect("failed to enter in-progress the second time");
        database
            .sessions()
            .update_session_status_with_timing_at("session-a", "Question", 190)
            .await
            .expect("failed to leave in-progress the second time");
        let session_row = load_session_row(&database, "session-a").await;

        // Assert
        assert_eq!(session_row.status, "Question");
        assert_eq!(session_row.in_progress_started_at, None);
        assert_eq!(session_row.in_progress_total_seconds, 150);
    }

    /// Verifies `load_sessions_for_project()` filters rows by project id.
    #[tokio::test]
    async fn test_load_sessions_for_project_filters_to_project_rows() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .projects()
            .upsert_project("/tmp/project-a", Some("main".to_string()))
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .projects()
            .upsert_project("/tmp/project-b", Some("develop".to_string()))
            .await
            .expect("failed to insert second project");

        insert_session_fixture(&database, "session-a", "main", "Review", first_project_id).await;
        insert_session_fixture(&database, "session-b", "main", "Done", first_project_id).await;
        insert_session_fixture(&database, "session-c", "develop", "Done", second_project_id).await;
        database
            .sessions()
            .update_session_updated_at("session-a", 300)
            .await
            .expect("failed to update session-a updated_at");
        database
            .sessions()
            .update_session_updated_at("session-b", 200)
            .await
            .expect("failed to update session-b updated_at");
        database
            .sessions()
            .update_session_updated_at("session-c", 100)
            .await
            .expect("failed to update session-c updated_at");

        // Act
        let session_rows = database
            .sessions()
            .load_sessions_for_project(first_project_id)
            .await
            .expect("failed to load project sessions");

        // Assert
        assert_eq!(session_rows.len(), 2);
        assert_eq!(session_rows[0].id, "session-a");
        assert_eq!(session_rows[1].id, "session-b");
        assert!(
            session_rows
                .iter()
                .all(|row| row.project_id == Some(first_project_id))
        );
    }

    /// Verifies stacked draft inserts persist their parent session link.
    #[tokio::test]
    async fn test_insert_stacked_draft_session_persists_parent_session_id() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;

        // Act
        database
            .sessions()
            .insert_stacked_draft_session(
                "child-session",
                "gpt-5.5",
                "wt/parent-session",
                "Draft",
                "parent-session",
                project_id,
            )
            .await
            .expect("failed to insert stacked draft session");
        let child_session = load_session_row(&database, "child-session").await;

        // Assert
        assert_eq!(child_session.base_branch, "wt/parent-session");
        assert!(child_session.is_draft);
        assert_eq!(
            child_session.parent_session_id.as_deref(),
            Some("parent-session")
        );
    }

    /// Verifies restacking clears active child parent links after parent merge.
    #[tokio::test]
    async fn test_restack_child_sessions_after_parent_merge_clears_active_children() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
        database
            .sessions()
            .insert_stacked_draft_session(
                "child-session",
                "gpt-5.5",
                "wt/parent-session",
                "Draft",
                "parent-session",
                project_id,
            )
            .await
            .expect("failed to insert active stacked child");
        database
            .sessions()
            .insert_stacked_draft_session(
                "review-child",
                "gpt-5.5",
                "wt/parent-session",
                "Review",
                "parent-session",
                project_id,
            )
            .await
            .expect("failed to insert review stacked child");
        database
            .sessions()
            .insert_stacked_draft_session(
                "canceled-child",
                "gpt-5.5",
                "wt/parent-session",
                "Canceled",
                "parent-session",
                project_id,
            )
            .await
            .expect("failed to insert canceled stacked child");

        // Act
        let restacked_child_session_ids = database
            .sessions()
            .restack_child_sessions_after_parent_merge(
                "parent-session",
                "main",
                Some("parent-tip".to_string()),
            )
            .await
            .expect("failed to restack child sessions");
        let child_session = load_session_row(&database, "child-session").await;
        let review_child = load_session_row(&database, "review-child").await;
        let review_child_stack_base = database
            .sessions()
            .get_session_stack_base_commit_hash("review-child")
            .await
            .expect("failed to load review child stack base");
        let canceled_child = load_session_row(&database, "canceled-child").await;

        // Assert
        assert_eq!(
            restacked_child_session_ids,
            vec!["review-child".to_string()]
        );
        assert_eq!(child_session.parent_session_id, None);
        assert_eq!(child_session.base_branch, "main");
        assert_eq!(review_child.parent_session_id, None);
        assert_eq!(review_child.base_branch, "main");
        assert_eq!(review_child_stack_base.as_deref(), Some("parent-tip"));
        assert_eq!(
            canceled_child.parent_session_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(canceled_child.base_branch, "wt/parent-session");
    }

    /// Verifies deleting a parent retargets surviving children onto the
    /// parent's base branch instead of leaving them on the orphaned worktree
    /// branch.
    #[tokio::test]
    async fn test_delete_session_retargets_children_base_branch() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
        database
            .sessions()
            .insert_stacked_draft_session(
                "child-session",
                "gpt-5.5",
                "wt/parent-session",
                "Draft",
                "parent-session",
                project_id,
            )
            .await
            .expect("failed to insert active stacked child");
        database
            .sessions()
            .insert_stacked_draft_session(
                "canceled-child",
                "gpt-5.5",
                "wt/parent-session",
                "Canceled",
                "parent-session",
                project_id,
            )
            .await
            .expect("failed to insert canceled stacked child");

        // Act
        database
            .sessions()
            .delete_session("parent-session")
            .await
            .expect("failed to delete parent session");
        let child_session = load_session_row(&database, "child-session").await;
        let canceled_child = load_session_row(&database, "canceled-child").await;

        // Assert
        assert_eq!(child_session.parent_session_id, None);
        assert_eq!(child_session.base_branch, "main");
        assert_eq!(canceled_child.parent_session_id, None);
        assert_eq!(canceled_child.base_branch, "wt/parent-session");
    }

    #[tokio::test]
    async fn test_load_pending_stack_restack_session_ids_returns_only_review_ready_parentless_rows()
    {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        insert_session_fixture(&database, "ready-child", "main", "Review", project_id).await;
        insert_session_fixture(&database, "draft-child", "main", "Draft", project_id).await;
        insert_session_fixture(&database, "plain-review", "main", "Review", project_id).await;
        insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
        database
            .sessions()
            .insert_stacked_draft_session(
                "still-stacked",
                "gpt-5.5",
                "wt/parent-session",
                "Review",
                "parent-session",
                project_id,
            )
            .await
            .expect("failed to insert stacked child");
        for session_id in ["ready-child", "draft-child", "still-stacked"] {
            database
                .sessions()
                .update_session_stack_base_commit_hash(session_id, Some("parent-tip".to_string()))
                .await
                .expect("failed to set stack base hash");
        }

        // Act
        let pending_session_ids = database
            .sessions()
            .load_pending_stack_restack_session_ids(project_id)
            .await
            .expect("failed to load pending restacks");

        // Assert
        assert_eq!(pending_session_ids, vec!["ready-child".to_string()]);
    }

    /// Verifies `load_sessions_metadata()` returns session count and max
    /// `updated_at`.
    #[tokio::test]
    async fn test_load_sessions_metadata_returns_count_and_latest_timestamp() {
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
        insert_session_fixture(&database, "session-b", "main", "Done", project_id).await;
        database
            .sessions()
            .update_session_updated_at("session-a", 200)
            .await
            .expect("failed to update session-a updated_at");
        database
            .sessions()
            .update_session_updated_at("session-b", 300)
            .await
            .expect("failed to update session-b updated_at");

        // Act
        let session_metadata = database
            .sessions()
            .load_sessions_metadata()
            .await
            .expect("failed to load session metadata");

        // Assert
        assert_eq!(session_metadata, (2, 300));
    }

    /// Verifies `load_session_timestamps()` returns the persisted timestamps.
    #[tokio::test]
    async fn test_load_session_timestamps_returns_created_and_updated_values() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .sessions()
            .update_session_created_at("session-a", 111)
            .await
            .expect("failed to update session created_at");
        database
            .sessions()
            .update_session_updated_at("session-a", 222)
            .await
            .expect("failed to update session updated_at");

        // Act
        let session_timestamps = database
            .sessions()
            .load_session_timestamps("session-a")
            .await
            .expect("failed to load session timestamps");

        // Assert
        assert_eq!(session_timestamps, Some((111, 222)));
    }

    /// Verifies `get_session_base_branch()` returns the persisted branch name.
    #[tokio::test]
    async fn test_get_session_base_branch_returns_persisted_value() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "release", "Done", project_id).await;

        // Act
        let base_branch = database
            .sessions()
            .get_session_base_branch("session-a")
            .await
            .expect("failed to load session base branch");

        // Assert
        assert_eq!(base_branch.as_deref(), Some("release"));
    }

    /// Verifies `delete_session()` removes the session row and nulls
    /// `session_usage.session_id`.
    #[tokio::test]
    async fn test_delete_session_removes_row_and_nulls_usage_foreign_key() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .usage()
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to insert usage row");

        // Act
        database
            .sessions()
            .delete_session("session-a")
            .await
            .expect("failed to delete session");
        let deleted_session = database
            .sessions()
            .load_session_timestamps("session-a")
            .await
            .expect("failed to load deleted session timestamps");
        let retained_usage_row = sqlx::query_as!(
            SessionUsageSessionIdRow,
            r#"
SELECT session_id AS "session_id: _"
FROM session_usage
WHERE model = ?
"#,
            "claude-opus-4.1"
        )
        .fetch_one(database.pool())
        .await
        .expect("failed to load retained usage row");

        // Assert
        assert_eq!(deleted_session, None);
        assert_eq!(retained_usage_row.session_id, None,);
    }

    /// Verifies `load_unfinished_session_operations()` returns only queued and
    /// running rows.
    #[tokio::test]
    async fn test_load_unfinished_session_operations_returns_only_queued_and_running_rows() {
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
        database
            .operations()
            .insert_session_operation("operation-queued", "session-a", "merge")
            .await
            .expect("failed to insert queued operation");
        database
            .operations()
            .insert_session_operation("operation-running", "session-a", "sync")
            .await
            .expect("failed to insert running operation");
        database
            .operations()
            .insert_session_operation("operation-done", "session-a", "review")
            .await
            .expect("failed to insert done operation");
        database
            .operations()
            .mark_session_operation_running("operation-running")
            .await
            .expect("failed to mark running operation");
        database
            .operations()
            .mark_session_operation_running("operation-done")
            .await
            .expect("failed to mark done operation running");
        database
            .operations()
            .mark_session_operation_done("operation-done")
            .await
            .expect("failed to mark done operation");

        // Act
        let unfinished_rows = database
            .operations()
            .load_unfinished_session_operations()
            .await
            .expect("failed to load unfinished operations");

        // Assert
        assert_eq!(unfinished_rows.len(), 2);
        assert_eq!(unfinished_rows[0].id, "operation-queued");
        assert_eq!(unfinished_rows[0].status, "queued");
        assert_eq!(unfinished_rows[1].id, "operation-running");
        assert_eq!(unfinished_rows[1].status, "running");
    }

    /// Verifies `request_cancel_for_session_operations()` marks only
    /// unfinished rows.
    #[tokio::test]
    async fn test_request_cancel_for_session_operations_marks_only_unfinished_rows() {
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
        database
            .operations()
            .insert_session_operation("operation-queued", "session-a", "merge")
            .await
            .expect("failed to insert queued operation");
        database
            .operations()
            .insert_session_operation("operation-done", "session-a", "review")
            .await
            .expect("failed to insert done operation");
        database
            .operations()
            .mark_session_operation_running("operation-done")
            .await
            .expect("failed to mark done operation running");
        database
            .operations()
            .mark_session_operation_done("operation-done")
            .await
            .expect("failed to mark done operation");

        // Act
        database
            .operations()
            .request_cancel_for_session_operations("session-a")
            .await
            .expect("failed to request cancel");
        let queued_row = load_session_operation_row(&database, "operation-queued").await;
        let done_row = load_session_operation_row(&database, "operation-done").await;

        // Assert
        assert!(queued_row.cancel_requested);
        assert!(!done_row.cancel_requested);
    }

    /// Verifies `is_session_operation_unfinished()` returns `false` for a
    /// completed operation.
    #[tokio::test]
    async fn test_is_session_operation_unfinished_returns_false_for_done_operation() {
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
        database
            .operations()
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");
        database
            .operations()
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");
        database
            .operations()
            .mark_session_operation_done("operation-a")
            .await
            .expect("failed to mark operation done");

        // Act
        let is_unfinished = database
            .operations()
            .is_session_operation_unfinished("operation-a")
            .await
            .expect("failed to check unfinished operation state");

        // Assert
        assert!(!is_unfinished);
    }

    /// Verifies `is_cancel_requested_for_operation()` returns `true` for a
    /// cancelled operation and `false` for an unaffected one.
    #[tokio::test]
    async fn test_is_cancel_requested_for_operation_scoped_to_single_operation() {
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
        database
            .operations()
            .insert_session_operation("operation-cancelled", "session-a", "reply")
            .await
            .expect("failed to insert cancelled operation");
        database
            .operations()
            .insert_session_operation("operation-new", "session-a", "reply")
            .await
            .expect("failed to insert new operation");

        // Cancel only the first operation via session-level bulk update.
        database
            .operations()
            .request_cancel_for_session_operations("session-a")
            .await
            .expect("failed to request cancel");

        // Simulate a new operation created after the cancel request by
        // resetting its flag directly (mirrors real flow where new
        // operations are inserted with cancel_requested = 0 by default).
        sqlx::query("UPDATE session_operation SET cancel_requested = 0 WHERE id = 'operation-new'")
            .execute(&database.pool)
            .await
            .expect("failed to reset new operation flag");

        // Act
        let cancelled_flag = database
            .operations()
            .is_cancel_requested_for_operation("operation-cancelled")
            .await
            .expect("failed to check cancelled operation");
        let new_flag = database
            .operations()
            .is_cancel_requested_for_operation("operation-new")
            .await
            .expect("failed to check new operation");

        // Assert — only the cancelled operation is flagged; the new one
        // proceeds normally.
        assert!(cancelled_flag);
        assert!(!new_flag);
    }

    /// Verifies `mark_session_operation_running()` sets the running state and
    /// timestamps.
    #[tokio::test]
    async fn test_mark_session_operation_running_sets_started_at_and_heartbeat() {
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
        database
            .operations()
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");

        // Act
        database
            .operations()
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");
        let running_row = load_session_operation_row(&database, "operation-a").await;

        // Assert
        assert_eq!(running_row.status, "running");
        assert!(running_row.started_at.is_some());
        assert!(running_row.heartbeat_at.is_some());
        assert_eq!(running_row.last_error, None);
    }

    /// Verifies `mark_session_operation_done()` sets the terminal completion
    /// fields.
    #[tokio::test]
    async fn test_mark_session_operation_done_sets_finished_state() {
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
        database
            .operations()
            .insert_session_operation("operation-a", "session-a", "merge")
            .await
            .expect("failed to insert operation");
        database
            .operations()
            .mark_session_operation_running("operation-a")
            .await
            .expect("failed to mark operation running");

        // Act
        database
            .operations()
            .mark_session_operation_done("operation-a")
            .await
            .expect("failed to mark operation done");
        let done_row = load_session_operation_row(&database, "operation-a").await;

        // Assert
        assert_eq!(done_row.status, "done");
        assert!(done_row.finished_at.is_some());
        assert!(done_row.heartbeat_at.is_some());
        assert_eq!(done_row.last_error, None);
    }

    /// Verifies `SessionJoinRow::into_session_row()` drops partially
    /// populated review-request columns instead of surfacing an invalid row
    /// model.
    #[test]
    fn test_session_join_row_ignores_partial_review_request_columns() {
        // Arrange
        let mut session_join_row = session_join_row_fixture();
        session_join_row.review_request_last_refreshed_at = None;

        // Act
        let session_row = session_join_row.into_session_row();

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.project_id, Some(7));
        assert_eq!(
            session_row.parent_session_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(session_row.status, "Review");
        assert_eq!(session_row.added_lines, 14);
        assert_eq!(session_row.deleted_lines, 6);
        assert_eq!(session_row.review_request, None);
    }

    /// Verifies `SessionJoinRow::into_session_row()` maps a fully populated
    /// review-request into the public session row model.
    #[test]
    fn test_session_join_row_maps_review_request_columns() {
        // Arrange
        let session_join_row = session_join_row_fixture();

        // Act
        let session_row = session_join_row.into_session_row();

        // Assert
        assert_eq!(session_row.id, "session-a");
        assert_eq!(session_row.added_lines, 14);
        assert_eq!(session_row.deleted_lines, 6);
        assert_eq!(session_row.project_id, Some(7));
        assert_eq!(
            session_row.parent_session_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(
            session_row.published_upstream_ref.as_deref(),
            Some("origin/session-a")
        );
        assert_eq!(session_row.questions.as_deref(), Some("Question text"));
        assert_eq!(session_row.summary.as_deref(), Some("Summary text"));
        assert_eq!(session_row.title.as_deref(), Some("Review session"));
        assert_review_request_row(&session_row);
    }

    /// Verifies `upsert_session_usage()` accumulates per-model token totals and
    /// invocation counts.
    #[tokio::test]
    async fn test_upsert_session_usage_accumulates_counts_per_model() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");

        insert_session_fixture(&database, "session-a", "main", "Done", project_id).await;
        database
            .usage()
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 11,
                    output_tokens: 29,
                },
            )
            .await
            .expect("failed to insert first usage row");
        database
            .usage()
            .upsert_session_usage(
                "session-a",
                "claude-opus-4.1",
                &SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    input_tokens: 3,
                    output_tokens: 5,
                },
            )
            .await
            .expect("failed to update existing usage row");
        database
            .usage()
            .upsert_session_usage("session-a", "ignored-model", &SessionStats::default())
            .await
            .expect("failed to ignore zero-usage update");

        // Act
        let usage_rows = database
            .usage()
            .load_session_usage("session-a")
            .await
            .expect("failed to load session usage");

        // Assert
        assert_eq!(usage_rows.len(), 1);
        assert_eq!(usage_rows[0].model, "claude-opus-4.1");
        assert_eq!(usage_rows[0].input_tokens, 14);
        assert_eq!(usage_rows[0].invocation_count, 2);
        assert_eq!(usage_rows[0].output_tokens, 34);
        assert_eq!(usage_rows[0].session_id.as_deref(), Some("session-a"));
    }

    #[tokio::test]
    async fn test_setting_round_trip_supports_default_smart_fast_and_review_models() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        database
            .settings()
            .upsert_setting(
                SettingName::DefaultSmartModel,
                AgentModel::Gemini31ProPreview.as_str(),
            )
            .await
            .expect("failed to persist default smart model");
        database
            .settings()
            .upsert_setting(SettingName::DefaultFastModel, AgentModel::Gpt55.as_str())
            .await
            .expect("failed to persist default fast model");
        database
            .settings()
            .upsert_setting(
                SettingName::DefaultReviewModel,
                AgentModel::ClaudeOpus48.as_str(),
            )
            .await
            .expect("failed to persist default review model");

        // Act
        let default_smart_model = database
            .settings()
            .get_setting(SettingName::DefaultSmartModel)
            .await
            .expect("failed to load default smart model");
        let default_fast_model = database
            .settings()
            .get_setting(SettingName::DefaultFastModel)
            .await
            .expect("failed to load default fast model");
        let default_review_model = database
            .settings()
            .get_setting(SettingName::DefaultReviewModel)
            .await
            .expect("failed to load default review model");

        // Assert
        assert_eq!(
            default_smart_model,
            Some(AgentModel::Gemini31ProPreview.as_str().to_string())
        );
        assert_eq!(
            default_fast_model,
            Some(AgentModel::Gpt55.as_str().to_string())
        );
        assert_eq!(
            default_review_model,
            Some(AgentModel::ClaudeOpus48.as_str().to_string())
        );
    }

    #[tokio::test]
    async fn test_project_setting_round_trip_is_isolated_per_project() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let first_project_id = database
            .projects()
            .upsert_project("/tmp/project-a", Some("main".to_string()))
            .await
            .expect("failed to insert first project");
        let second_project_id = database
            .projects()
            .upsert_project("/tmp/project-b", Some("main".to_string()))
            .await
            .expect("failed to insert second project");

        database
            .settings()
            .upsert_project_setting(first_project_id, SettingName::OpenCommand, "npm run dev")
            .await
            .expect("failed to persist first project setting");
        database
            .settings()
            .upsert_project_setting(second_project_id, SettingName::OpenCommand, "cargo test")
            .await
            .expect("failed to persist second project setting");

        // Act
        let first_project_setting = database
            .settings()
            .get_project_setting(first_project_id, SettingName::OpenCommand)
            .await
            .expect("failed to load first project setting");
        let second_project_setting = database
            .settings()
            .get_project_setting(second_project_id, SettingName::OpenCommand)
            .await
            .expect("failed to load second project setting");

        // Assert
        assert_eq!(first_project_setting, Some("npm run dev".to_string()));
        assert_eq!(second_project_setting, Some("cargo test".to_string()));
    }

    #[tokio::test]
    async fn test_project_reasoning_level_round_trip_uses_typed_setting_helpers() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");

        // Act
        database
            .settings()
            .set_project_reasoning_level(project_id, ReasoningLevel::Low)
            .await
            .expect("failed to persist project reasoning level");
        let reasoning_level = database
            .settings()
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load project reasoning level");

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::Low);
    }

    #[tokio::test]
    async fn test_load_project_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");

        // Act
        let missing_setting_level = database
            .settings()
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load default project reasoning level");
        database
            .settings()
            .upsert_project_setting(project_id, SettingName::ReasoningLevel, "unsupported")
            .await
            .expect("failed to insert unsupported project reasoning level");
        let invalid_setting_level = database
            .settings()
            .load_project_reasoning_level(project_id)
            .await
            .expect("failed to load fallback project reasoning level");

        // Assert
        assert_eq!(missing_setting_level, ReasoningLevel::High);
        assert_eq!(invalid_setting_level, ReasoningLevel::High);
    }

    #[tokio::test]
    async fn test_reasoning_level_round_trip_uses_typed_setting_helpers() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        database
            .settings()
            .set_reasoning_level(ReasoningLevel::Low)
            .await
            .expect("failed to persist reasoning level");
        let reasoning_level = database
            .settings()
            .load_reasoning_level()
            .await
            .expect("failed to load reasoning level");

        // Assert
        assert_eq!(reasoning_level, ReasoningLevel::Low);
    }

    #[tokio::test]
    async fn test_load_reasoning_level_defaults_when_setting_is_missing_or_invalid() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let missing_setting_level = database
            .settings()
            .load_reasoning_level()
            .await
            .expect("failed to load default reasoning level");
        database
            .settings()
            .upsert_setting(SettingName::ReasoningLevel, "unsupported")
            .await
            .expect("failed to insert unsupported reasoning level");
        let invalid_setting_level = database
            .settings()
            .load_reasoning_level()
            .await
            .expect("failed to load fallback reasoning level");

        // Assert
        assert_eq!(missing_setting_level, ReasoningLevel::High);
        assert_eq!(invalid_setting_level, ReasoningLevel::High);
    }

    #[tokio::test]
    async fn test_session_provider_conversation_id_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .sessions()
            .update_session_provider_conversation_id("session-a", Some("thread-123".to_string()))
            .await
            .expect("failed to set provider conversation id");
        let stored_id = database
            .sessions()
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load provider conversation id");
        database
            .sessions()
            .update_session_provider_conversation_id("session-a", None)
            .await
            .expect("failed to clear provider conversation id");
        let cleared_id = database
            .sessions()
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load cleared provider conversation id");

        // Assert
        assert_eq!(stored_id, Some("thread-123".to_string()));
        assert_eq!(cleared_id, None);
    }

    #[tokio::test]
    async fn test_session_instruction_conversation_id_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session");
        let instruction_conversation_id = Some("thread-123");

        // Act
        database
            .sessions()
            .update_session_instruction_conversation_id(
                "session-a",
                instruction_conversation_id.map(str::to_string),
            )
            .await
            .expect("failed to set instruction conversation id");
        let stored_conversation_id = database
            .sessions()
            .get_session_instruction_conversation_id("session-a")
            .await
            .expect("failed to load instruction conversation id");
        database
            .sessions()
            .update_session_instruction_conversation_id("session-a", None)
            .await
            .expect("failed to clear instruction conversation id");
        let cleared_conversation_id = database
            .sessions()
            .get_session_instruction_conversation_id("session-a")
            .await
            .expect("failed to load cleared instruction conversation id");

        // Assert
        assert_eq!(stored_conversation_id, Some("thread-123".to_string()));
        assert_eq!(cleared_conversation_id, None);
    }

    #[tokio::test]
    async fn test_session_published_upstream_ref_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .sessions()
            .update_session_published_upstream_ref(
                "session-a",
                Some("origin/wt/session-a".to_string()),
            )
            .await
            .expect("failed to persist session published upstream ref");
        let persisted_row = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing persisted session row");
        database
            .sessions()
            .update_session_published_upstream_ref("session-a", None)
            .await
            .expect("failed to clear session published upstream ref");
        let cleared_row = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions after clearing")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing cleared session row");

        // Assert
        assert_eq!(
            persisted_row.published_upstream_ref.as_deref(),
            Some("origin/wt/session-a")
        );
        assert_eq!(cleared_row.published_upstream_ref, None);
    }

    #[tokio::test]
    async fn test_load_session_published_upstream_ref_returns_stored_value() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-load", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        database
            .sessions()
            .update_session_published_upstream_ref(
                "session-load",
                Some("origin/wt/session-load".to_string()),
            )
            .await
            .expect("failed to set published upstream ref");

        // Act
        let loaded_ref = database
            .sessions()
            .load_session_published_upstream_ref("session-load")
            .await
            .expect("failed to load published upstream ref");

        // Assert
        assert_eq!(loaded_ref.as_deref(), Some("origin/wt/session-load"));
    }

    #[tokio::test]
    async fn test_load_session_published_upstream_ref_returns_none_when_unset() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-unset", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let loaded_ref = database
            .sessions()
            .load_session_published_upstream_ref("session-unset")
            .await
            .expect("failed to load published upstream ref");

        // Assert
        assert_eq!(loaded_ref, None);
    }

    #[tokio::test]
    async fn test_load_session_published_upstream_ref_returns_none_for_missing_session() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let loaded_ref = database
            .sessions()
            .load_session_published_upstream_ref("nonexistent")
            .await
            .expect("failed to load published upstream ref");

        // Assert
        assert_eq!(loaded_ref, None);
    }

    #[tokio::test]
    async fn test_session_merged_commit_hash_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .sessions()
            .update_session_merged_commit_hash("session-a", Some("abc1234".to_string()))
            .await
            .expect("failed to store merged commit hash");
        let stored_hash = database
            .sessions()
            .load_session_merged_commit_hash("session-a")
            .await
            .expect("failed to load stored merged commit hash");
        database
            .sessions()
            .update_session_merged_commit_hash("session-a", None)
            .await
            .expect("failed to clear merged commit hash");
        let cleared_hash = database
            .sessions()
            .load_session_merged_commit_hash("session-a")
            .await
            .expect("failed to load cleared merged commit hash");

        // Assert
        assert_eq!(stored_hash.as_deref(), Some("abc1234"));
        assert_eq!(cleared_hash, None);
    }

    #[tokio::test]
    async fn test_session_review_request_round_trip_and_clear() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        let review_request = review_request_fixture();

        // Act
        database
            .reviews()
            .update_session_review_request("session-a", Some(review_request.clone()))
            .await
            .expect("failed to persist session review request");
        let persisted_row = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing persisted session row");
        database
            .reviews()
            .update_session_review_request("session-a", None)
            .await
            .expect("failed to clear session review request");
        let cleared_row = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to load sessions after clearing")
            .into_iter()
            .find(|row| row.id == "session-a")
            .expect("missing cleared session row");

        // Assert
        assert_review_request_row(&persisted_row);
        assert_eq!(cleared_row.review_request, None);
    }

    #[tokio::test]
    async fn test_insert_session_creation_activity_at_persists_timestamp() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .activity()
            .insert_session_creation_activity_at("session-a", 123)
            .await
            .expect("failed to persist activity event");
        let activity_timestamps = database
            .activity()
            .load_session_activity_timestamps()
            .await
            .expect("failed to load activity timestamps");

        // Assert
        assert_eq!(activity_timestamps, vec![123]);
    }

    #[tokio::test]
    async fn test_insert_session_creation_activity_at_ignores_duplicates_per_session() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        database
            .activity()
            .insert_session_creation_activity_at("session-a", 100)
            .await
            .expect("failed to persist first activity event");
        database
            .activity()
            .insert_session_creation_activity_at("session-a", 200)
            .await
            .expect("failed to persist duplicate activity event");
        let activity_timestamps = database
            .activity()
            .load_session_activity_timestamps()
            .await
            .expect("failed to load activity timestamps");

        // Assert
        assert_eq!(activity_timestamps, vec![100]);
    }

    #[tokio::test]
    async fn test_load_session_activity_timestamps_keeps_deleted_session_history() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .activity()
            .insert_session_creation_activity_at("session-a", 100)
            .await
            .expect("failed to persist first activity event");
        database
            .sessions()
            .insert_session("session-b", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .activity()
            .insert_session_creation_activity_at("session-b", 200)
            .await
            .expect("failed to persist second activity event");
        database
            .sessions()
            .delete_session("session-a")
            .await
            .expect("failed to delete first session");

        // Act
        let activity_timestamps = database
            .activity()
            .load_session_activity_timestamps()
            .await
            .expect("failed to load activity timestamps");

        // Assert
        assert_eq!(activity_timestamps, vec![100, 200]);
    }

    /// Verifies `load_session_activity()` groups immutable activity rows by
    /// local day.
    #[tokio::test]
    async fn test_load_session_activity_groups_counts_by_local_day() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .sessions()
            .insert_session("session-b", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .sessions()
            .insert_session("session-c", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert third session");

        let first_day_timestamp = 10 * 86_400 + 10;
        let second_timestamp_same_day = 10 * 86_400 + 600;
        let second_day_timestamp = 11 * 86_400 + 50;

        database
            .activity()
            .clear_session_activity()
            .await
            .expect("failed to clear session activity");
        database
            .activity()
            .insert_session_creation_activity_at("session-a", first_day_timestamp)
            .await
            .expect("failed to persist first activity event");
        database
            .activity()
            .insert_session_creation_activity_at("session-b", second_timestamp_same_day)
            .await
            .expect("failed to persist second activity event");
        database
            .activity()
            .insert_session_creation_activity_at("session-c", second_day_timestamp)
            .await
            .expect("failed to persist third activity event");

        let expected_activity = vec![
            DailyActivity {
                day_key: local_day_key(first_day_timestamp),
                session_count: 2,
            },
            DailyActivity {
                day_key: local_day_key(second_day_timestamp),
                session_count: 1,
            },
        ];

        // Act
        let activity = database
            .activity()
            .load_session_activity()
            .await
            .expect("failed to load aggregated session activity");

        // Assert
        assert_eq!(activity, expected_activity);
    }

    #[tokio::test]
    async fn test_load_projects_with_stats_returns_session_counts_tokens_and_last_update() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session-a");
        database
            .sessions()
            .persist_session_turn_metadata(
                "session-a",
                &SessionTurnMetadata {
                    instruction_conversation_id: None,
                    model: AgentModel::Gpt55.as_str().to_string(),
                    provider_conversation_id: None,
                    questions_json: "[]".to_string(),
                    summary: String::new(),
                    token_usage_delta: SessionStats {
                        added_lines: 0,
                        deleted_lines: 0,
                        input_tokens: 1_200,
                        output_tokens: 650,
                    },
                },
            )
            .await
            .expect("failed to persist session-a token metadata");
        database
            .sessions()
            .insert_session("session-b", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session-b");
        database
            .sessions()
            .persist_session_turn_metadata(
                "session-b",
                &SessionTurnMetadata {
                    instruction_conversation_id: None,
                    model: AgentModel::Gpt55.as_str().to_string(),
                    provider_conversation_id: None,
                    questions_json: "[]".to_string(),
                    summary: String::new(),
                    token_usage_delta: SessionStats {
                        added_lines: 0,
                        deleted_lines: 0,
                        input_tokens: 3,
                        output_tokens: 5,
                    },
                },
            )
            .await
            .expect("failed to persist session-b token metadata");

        // Act
        let projects = database
            .projects()
            .load_projects_with_stats()
            .await
            .expect("failed to load projects with stats");

        // Assert
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].session_count, 2);
        assert_eq!(projects[0].input_tokens, 1_203);
        assert_eq!(projects[0].output_tokens, 655);
        assert!(projects[0].last_session_updated_at.is_some());
    }

    /// Converts one Unix timestamp into the local day key used by heatmap
    /// activity rows.
    fn local_day_key(timestamp_seconds: i64) -> i64 {
        let utc_timestamp = time::OffsetDateTime::from_unix_timestamp(timestamp_seconds)
            .expect("timestamp should be valid for test fixture");
        let local_offset = time::UtcOffset::local_offset_at(utc_timestamp)
            .expect("local offset should resolve for test fixture");

        timestamp_seconds
            .saturating_add(i64::from(local_offset.whole_seconds()))
            .div_euclid(86_400)
    }

    /// Verifies the SQL activity aggregation matches Rust local-day grouping
    /// across a known daylight-saving transition in an isolated timezone-fixed
    /// subprocess.
    #[test]
    fn test_load_session_activity_matches_rust_grouping_across_dst_transition() {
        // Arrange
        if !cfg!(unix) {
            return;
        }

        let current_test_binary = env::current_exe().expect("failed to resolve current test bin");

        // Act
        let output = Command::new(current_test_binary)
            .env(DST_TEST_SUBPROCESS_ENV, "1")
            .env("TZ", "America/Los_Angeles")
            .arg(
                "test_load_session_activity_matches_rust_grouping_across_dst_transition_subprocess",
            )
            .arg("--exact")
            .arg("--test-threads=1")
            .output()
            .expect("failed to run DST subprocess test");

        // Assert
        assert!(
            output.status.success(),
            "DST subprocess test failed.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Verifies the SQL activity aggregation keeps timestamps on both sides of
    /// the 2024 spring-forward transition in the same local day when Rust's
    /// per-event local-offset calculation says they should.
    #[tokio::test]
    async fn test_load_session_activity_matches_rust_grouping_across_dst_transition_subprocess() {
        // Arrange
        if !cfg!(unix) || env::var_os(DST_TEST_SUBPROCESS_ENV).is_none() {
            return;
        }

        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", None)
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert first session");
        database
            .sessions()
            .insert_session("session-b", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert second session");
        database
            .sessions()
            .insert_session("session-c", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert third session");
        database
            .activity()
            .clear_session_activity()
            .await
            .expect("failed to clear activity history");

        // `2024-03-10T01:30:00-08:00`, still before the DST jump.
        let before_dst_jump = 1_710_063_000_i64;
        // `2024-03-10T03:30:00-07:00`, after the skipped hour.
        let after_dst_jump = 1_710_066_600_i64;
        // `2024-03-11T00:30:00-07:00`, the next local day.
        let next_local_day = 1_710_142_200_i64;

        database
            .activity()
            .insert_session_creation_activity_at("session-a", before_dst_jump)
            .await
            .expect("failed to persist pre-DST activity");
        database
            .activity()
            .insert_session_creation_activity_at("session-b", after_dst_jump)
            .await
            .expect("failed to persist post-DST activity");
        database
            .activity()
            .insert_session_creation_activity_at("session-c", next_local_day)
            .await
            .expect("failed to persist next-day activity");

        let first_day_key = local_day_key(before_dst_jump);
        let second_day_key = local_day_key(after_dst_jump);
        let third_day_key = local_day_key(next_local_day);

        // Act
        let activity = database
            .activity()
            .load_session_activity()
            .await
            .expect("failed to load grouped session activity");

        // Assert
        assert_eq!(first_day_key, second_day_key);
        assert_ne!(second_day_key, third_day_key);
        assert_eq!(
            activity,
            vec![
                DailyActivity {
                    day_key: first_day_key,
                    session_count: 2,
                },
                DailyActivity {
                    day_key: third_day_key,
                    session_count: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_set_and_load_active_project_id_round_trip() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");

        // Act
        database
            .settings()
            .set_active_project_id(project_id)
            .await
            .expect("failed to persist active project id");
        let active_project_id = database
            .settings()
            .load_active_project_id()
            .await
            .expect("failed to load active project id");

        // Assert
        assert_eq!(active_project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_load_session_project_id_returns_associated_project() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session");

        // Act
        let loaded_project_id = database
            .sessions()
            .load_session_project_id("session-a")
            .await
            .expect("failed to load session project id");

        // Assert
        assert_eq!(loaded_project_id, Some(project_id));
    }

    #[tokio::test]
    async fn test_load_session_summary_returns_persisted_summary() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Done", project_id)
            .await
            .expect("failed to insert session");
        database
            .sessions()
            .update_session_summary("session-a", "persisted summary")
            .await
            .expect("failed to update session summary");

        // Act
        let loaded_summary = database
            .sessions()
            .load_session_summary("session-a")
            .await
            .expect("failed to load session summary");

        // Assert
        assert_eq!(loaded_summary.as_deref(), Some("persisted summary"));
    }

    #[tokio::test]
    async fn test_load_session_focused_reviews_for_project_returns_persisted_review() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        database
            .sessions()
            .update_session_focused_review(
                "session-a",
                Some("42".to_string()),
                Some("## Review\nPersisted".to_string()),
            )
            .await
            .expect("failed to update focused review");

        // Act
        let focused_reviews = database
            .sessions()
            .load_session_focused_reviews_for_project(project_id)
            .await
            .expect("failed to load focused reviews");

        // Assert
        assert_eq!(
            focused_reviews,
            vec![SessionFocusedReviewRow {
                diff_hash: "42".to_string(),
                session_id: "session-a".to_string(),
                text: "## Review\nPersisted".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn test_update_session_focused_review_clears_persisted_review() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        database
            .sessions()
            .update_session_focused_review(
                "session-a",
                Some("42".to_string()),
                Some("## Review\nPersisted".to_string()),
            )
            .await
            .expect("failed to update focused review");

        // Act
        database
            .sessions()
            .update_session_focused_review("session-a", None, None)
            .await
            .expect("failed to clear focused review");
        let focused_reviews = database
            .sessions()
            .load_session_focused_reviews_for_project(project_id)
            .await
            .expect("failed to load focused reviews");

        // Assert
        assert!(focused_reviews.is_empty());
    }

    #[tokio::test]
    /// Verifies transactional turn-metadata persistence rolls back partial
    /// writes when any statement in the transaction fails.
    async fn test_persist_session_turn_metadata_rolls_back_on_failure() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        database
            .sessions()
            .insert_session("session-a", "gpt-5.5", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        database
            .sessions()
            .update_session_summary("session-a", "persisted summary")
            .await
            .expect("failed to seed summary");
        sqlx::query("DROP TABLE session_usage")
            .execute(database.pool())
            .await
            .expect("failed to drop session-usage table");

        // Act
        let result = database
            .sessions()
            .persist_session_turn_metadata(
                "session-a",
                &SessionTurnMetadata {
                    instruction_conversation_id: Some("instruction-thread".to_string()),
                    model: AgentModel::Gpt55.as_str().to_string(),
                    provider_conversation_id: Some("thread-123".to_string()),
                    questions_json: r#"[{"text":"Need tests?"}]"#.to_string(),
                    summary: r#"{"turn":"Updated the worker.","session":"Session state changed."}"#
                        .to_string(),
                    token_usage_delta: SessionStats {
                        added_lines: 0,
                        deleted_lines: 0,
                        input_tokens: 3,
                        output_tokens: 5,
                    },
                },
            )
            .await;
        let session = database
            .sessions()
            .load_sessions()
            .await
            .expect("failed to reload sessions")
            .into_iter()
            .find(|session| session.id == "session-a")
            .expect("expected seeded session");
        let provider_conversation_id = database
            .sessions()
            .get_session_provider_conversation_id("session-a")
            .await
            .expect("failed to load provider conversation id");

        // Assert
        assert!(matches!(result, Err(DbError::Query(_))));
        assert_eq!(session.summary.as_deref(), Some("persisted summary"));
        assert_eq!(session.questions.as_deref(), None);
        assert_eq!(session.input_tokens, 0);
        assert_eq!(session.output_tokens, 0);
        assert_eq!(provider_conversation_id.as_deref(), None);
    }

    #[tokio::test]
    async fn test_set_project_favorite_updates_project_state() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");

        // Act
        database
            .projects()
            .set_project_favorite(project_id, true)
            .await
            .expect("failed to set project favorite");
        let project = database
            .projects()
            .get_project(project_id)
            .await
            .expect("failed to load project")
            .expect("expected existing project");

        // Assert
        assert!(project.is_favorite);
    }

    #[tokio::test]
    async fn query_on_dropped_table_returns_db_error_query() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        sqlx::query("DROP TABLE session")
            .execute(database.pool())
            .await
            .expect("failed to drop table");

        // Act
        let result = database.sessions().load_sessions_metadata().await;

        // Assert
        assert!(
            matches!(result, Err(DbError::Query(_))),
            "expected DbError::Query variant"
        );
    }

    #[tokio::test]
    async fn db_error_display_includes_underlying_message() {
        // Arrange
        let database = Database::open_in_memory()
            .await
            .expect("failed to open database");
        sqlx::query("DROP TABLE session")
            .execute(database.pool())
            .await
            .expect("failed to drop table");

        // Act
        let result = database.sessions().load_sessions_metadata().await;

        // Assert
        let error = result.expect_err("expected query on dropped table to fail");
        let display_text = error.to_string();
        assert!(
            !display_text.is_empty(),
            "DbError Display should produce a non-empty message"
        );
    }

    #[tokio::test]
    async fn open_with_unwritable_parent_returns_db_error_io() {
        // Arrange — place the database path under a regular file so
        // `create_dir_all` fails with an I/O error.
        let temp = tempdir().expect("failed to create temp directory");
        let blocking_file = temp.path().join("not_a_dir");
        std::fs::write(&blocking_file, b"").expect("failed to create blocking file");
        let db_path = blocking_file.join("nested").join("db.sqlite");

        // Act
        let result = Database::open(&db_path).await;

        // Assert
        assert!(
            matches!(result, Err(DbError::Io(_))),
            "expected DbError::Io variant"
        );
    }

    #[tokio::test]
    async fn open_configures_small_wal_pool_normal_synchronous_mode_and_busy_timeout() {
        // Arrange
        let temp = tempdir().expect("failed to create temp directory");
        let db_path = temp.path().join("agentty.db");

        // Act
        let database = Database::open(&db_path)
            .await
            .expect("failed to open database");
        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load journal mode pragma");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load synchronous pragma");
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load busy-timeout pragma");

        // Assert
        // `SqliteConnectOptions` are reused for every pooled connection, so
        // checking one pooled connection here is sufficient to prove the
        // configured busy timeout propagates across the on-disk pool.
        assert_eq!(
            database.pool().options().get_max_connections(),
            DB_POOL_MAX_CONNECTIONS
        );
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
        assert_eq!(busy_timeout, 2_000, "expected PRAGMA busy_timeout = 2000");
    }

    #[tokio::test]
    async fn open_in_memory_uses_single_connection_normal_synchronous_mode_and_busy_timeout() {
        // Arrange, Act
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory database");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load synchronous pragma");
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout;")
            .fetch_one(database.pool())
            .await
            .expect("failed to load busy-timeout pragma");

        // Assert
        assert_eq!(database.pool().options().get_max_connections(), 1);
        assert_eq!(synchronous, 1, "expected PRAGMA synchronous = NORMAL");
        assert_eq!(busy_timeout, 2_000, "expected PRAGMA busy_timeout = 2000");
    }

    // NOTE: `DbError::Migration` is not directly tested because
    // `Database::open` and `Database::open_in_memory` run migrations
    // atomically after connecting — there is no injection point to
    // pre-corrupt the schema before migrations execute. The `#[from]`
    // derive mapping from `sqlx::migrate::MigrateError` is validated
    // at compile time by `thiserror`.
}
