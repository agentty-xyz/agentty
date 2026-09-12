use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::tempdir;
use tokio::io::AsyncRead;

use crate::file_system::{FileSystem, LocalFileSystem, MockFileSystem};
use crate::session::{Database, NewSession, SessionError, TurnGuard};
use crate::tool::WriteArguments;
use crate::write::WriteTool;
use crate::write_journal::{WriteRecord, WriteRecordRow, WriteStatus, content_hash};
use crate::{ModelError, OutputSchema, ToolPolicy, TurnError, TurnLimits, TurnOptions, WriteError};

async fn fixture() -> (Database, TurnGuard) {
    let database = Database::open_in_memory().await.expect("database");
    let schema = OutputSchema::new(json!({"type": "object"})).expect("schema");
    let options = TurnOptions::new(schema.clone(), ToolPolicy::default(), TurnLimits::default());
    database
        .create_session(&NewSession::new("session", schema), None, 4096)
        .await
        .expect("session");
    let acquired = database
        .begin_turn("session", "write", &options)
        .await
        .expect("turn");

    (database, acquired.guard)
}

async fn fail(database: &Database, guard: &mut TurnGuard) {
    database
        .fail_turn("session", 0, &TurnError::Model(ModelError::InvalidResponse))
        .await
        .expect("fail turn");
    guard.disarm();
}

fn arguments() -> WriteArguments {
    serde_json::from_value(json!({"path": "file.txt", "patch": "--- /dev/null\n+++ b/file.txt\n@@ -0,0 +1 @@\n+new\n"}))
        .expect("write arguments")
}

#[tokio::test]
async fn applied_and_failed_outcomes_survive_failed_turns() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let directory = tempdir().expect("repository");
    let root = directory.path().canonicalize().expect("root");
    let mut tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
    tool.journal = Some(guard.write_journal());

    // Act
    tool.execute(&arguments(), "create").await.expect("write");
    let mut file_system = MockFileSystem::new();
    let canonical_root = root.clone();
    file_system
        .expect_canonicalize()
        .returning(move |_| Ok(canonical_root.clone()));
    file_system
        .expect_open_beneath()
        .returning(|_, _| Err(io::ErrorKind::NotFound.into()));
    file_system
        .expect_replace_beneath()
        .returning(|_, _, _, _| Err(io::Error::other("replacement failed")));
    let mut failing_tool = WriteTool::new(Arc::new(file_system), root);
    failing_tool.journal = Some(guard.write_journal());
    let error = failing_tool
        .execute(&arguments(), "failed")
        .await
        .expect_err("filesystem failure");
    fail(&database, &mut guard).await;
    let records = WriteRecordRow::load(database.pool(), "session")
        .await
        .expect("records");

    // Assert
    assert!(matches!(error, WriteError::WriteTarget { .. }));
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(records[1].status, WriteStatus::Failed);
    assert_eq!(
        content_hash(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[tokio::test]
async fn intent_failure_prevents_mutation_and_outcome_failure_keeps_pending_intent() {
    for phase in ["INSERT", "UPDATE"] {
        // Arrange
        let (database, mut guard) = fixture().await;
        let directory = tempdir().expect("repository");
        let root = directory.path().canonicalize().expect("root");
        let mut tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
        tool.journal = Some(guard.write_journal());
        let trigger = if phase == "INSERT" {
            "CREATE TRIGGER fail_journal BEFORE INSERT ON session_write BEGIN SELECT RAISE(FAIL, \
             'journal unavailable'); END"
        } else {
            "CREATE TRIGGER fail_journal BEFORE UPDATE ON session_write BEGIN SELECT RAISE(FAIL, \
             'journal unavailable'); END"
        };
        sqlx::query(trigger)
            .execute(database.pool())
            .await
            .expect("trigger");

        // Act
        let error = tool
            .execute(&arguments(), "create")
            .await
            .expect_err("journal failure");
        fail(&database, &mut guard).await;
        let records = WriteRecordRow::load(database.pool(), "session")
            .await
            .expect("records");

        // Assert
        assert!(!error.is_model_correctable());
        let persistence = match &error {
            WriteError::WriteTarget { path, source } => {
                assert_eq!(path, "file.txt");
                assert_eq!(source.kind(), io::ErrorKind::Other);
                source
                    .get_ref()
                    .and_then(|source| source.downcast_ref::<SessionError>())
            }
            _ => None,
        }
        .expect("journal failure must retain its typed persistence source");
        assert!(matches!(persistence, SessionError::QueryContext { .. }));
        if phase == "INSERT" {
            assert!(!root.join("file.txt").exists());
            assert_eq!(records, Vec::<WriteRecord>::new());
        } else {
            assert_eq!(
                tokio::fs::read(root.join("file.txt"))
                    .await
                    .expect("applied file"),
                b"new\n"
            );
            assert_eq!(records[0].status, WriteStatus::Pending);
        }
    }
}

#[tokio::test]
async fn rejected_intent_commit_prevents_file_mutation() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let directory = tempdir().expect("repository");
    let root = directory.path().canonicalize().expect("root");
    let mut tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
    tool.journal = Some(guard.write_journal());
    let commits = Arc::new(AtomicUsize::new(0));
    {
        let commits = Arc::clone(&commits);
        let mut connection = database.pool().acquire().await.expect("connection");
        connection
            .lock_handle()
            .await
            .expect("SQLite handle")
            .set_commit_hook(move || {
                commits.fetch_add(1, Ordering::SeqCst);

                false
            });
    }

    // Act
    let result = tool.execute(&arguments(), "create").await;
    let records = WriteRecordRow::load(database.pool(), "session")
        .await
        .expect("journal after rejected commit");
    guard.disarm();

    // Assert
    assert!(!root.join("file.txt").exists());
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(records, Vec::<WriteRecord>::new());
    let error = result.expect_err("commit rejection must fail the write");
    assert!(!error.is_model_correctable());
    let persistence = match &error {
        WriteError::WriteTarget { source, .. } => source
            .get_ref()
            .and_then(|source| source.downcast_ref::<SessionError>()),
        _ => None,
    };
    assert!(matches!(
        persistence,
        Some(SessionError::QueryContext {
            operation: "commit write intent",
            ..
        })
    ));
}

#[tokio::test]
async fn unavailable_intent_transaction_prevents_file_mutation() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let directory = tempdir().expect("repository");
    let root = directory.path().canonicalize().expect("root");
    let mut tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
    tool.journal = Some(guard.write_journal());
    guard.disarm();
    database.pool().close().await;

    // Act
    let error = tool
        .execute(&arguments(), "create")
        .await
        .expect_err("closed pool");

    // Assert
    assert!(!root.join("file.txt").exists());
    let persistence = match &error {
        WriteError::WriteTarget { source, .. } => source
            .get_ref()
            .and_then(|source| source.downcast_ref::<SessionError>()),
        _ => None,
    };
    assert!(matches!(
        persistence,
        Some(SessionError::QueryContext {
            operation: "begin write intent",
            source: sqlx::Error::PoolClosed,
        })
    ));
}

#[tokio::test]
async fn root_migration_preserves_existing_records_and_sequence() {
    // Arrange
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("legacy database");
    for migration in [
        include_str!("../migrations/001_create_session.sql"),
        include_str!("../migrations/002_add_session_turn_lifecycle.sql"),
        include_str!("../migrations/003_add_session_turn_owner_token.sql"),
        include_str!("../migrations/004_add_session_write.sql"),
    ] {
        sqlx::raw_sql(migration)
            .execute(&pool)
            .await
            .expect("legacy migration");
    }
    sqlx::raw_sql(r#"
INSERT INTO session (id, output_schema, max_history_bytes, created_at, updated_at)
VALUES ('session', '{"type":"object"}', 4096, 0, 0);
INSERT INTO session_turn (session_id, turn_position, status, created_at, updated_at)
VALUES ('session', 0, 'failed', 0, 0);
INSERT INTO session_write (id, session_id, turn_position, call_id, repository_root, path, resulting_hash, status)
VALUES (7, 'session', 0, 'call', '/repo/λ', 'file.txt', 'hash', 'applied'),
   (9, 'session', 0, 'deleted', '/repo/λ', 'file.txt', 'hash', 'pending');
DELETE FROM session_write WHERE id = 9;
"#).execute(&pool).await.expect("legacy data");

    // Act
    sqlx::raw_sql(include_str!(
        "../migrations/005_update_session_write_diagnostics.sql"
    ))
    .execute(&pool)
    .await
    .expect("upgrade journal");
    let records = WriteRecordRow::load(&pool, "session")
        .await
        .expect("migrated journal");
    let sequence = sqlx::query_scalar::<_, i64>(
        "SELECT seq FROM sqlite_sequence WHERE name = 'session_write'",
    )
    .fetch_one(&pool)
    .await
    .expect("write sequence");

    // Assert
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, 7);
    assert_eq!(records[0].call_id, "call");
    assert_eq!(records[0].repository_root, PathBuf::from("/repo/λ"));
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(sequence, 9);
}

#[tokio::test]
async fn journal_preserves_native_roots_fingerprints_and_duplicate_call_ids() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let journal = guard.write_journal();
    let native_root = PathBuf::from(OsString::from_vec(vec![b'/', 0xff]));

    // Act
    let first = journal
        .intent("call", &native_root, "file.txt", Some(b"old"), b"new")
        .await
        .expect("update intent");
    let second = journal
        .intent("call", &native_root, "other.txt", None, b"new")
        .await
        .expect("same identifier in a later model response");
    fail(&database, &mut guard).await;
    let records = WriteRecordRow::load(database.pool(), "session")
        .await
        .expect("native records");

    // Assert
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id, first);
    assert_eq!(records[1].id, second);
    assert!(second > first);
    assert_eq!(records[0].call_id, records[1].call_id);
    assert_eq!(records[0].expected_hash, Some(content_hash(b"old")));
    assert_eq!(records[1].expected_hash, None);
    assert_eq!(records[0].resulting_hash, content_hash(b"new"));
    assert_eq!(records[0].status, WriteStatus::Pending);
    assert_eq!(records[0].turn_position, 0);
    assert_eq!(records[0].repository_root, native_root);
    assert_eq!(
        serde_json::to_value(&records[0]).expect("lossless serialization")["repository_root"],
        json!(native_root.as_os_str().as_bytes())
    );
}

#[tokio::test]
async fn journal_rejects_lost_ownership_before_file_mutation() {
    for invalidate in [
        "UPDATE session_turn SET owner_token = X'00'",
        "UPDATE session_turn SET lease_expires_at = 0",
        "UPDATE session_turn SET status = 'failed', lease_expires_at = NULL",
    ] {
        // Arrange
        let (database, mut guard) = fixture().await;
        let directory = tempdir().expect("repository");
        let root = directory.path().canonicalize().expect("root");
        let mut tool = WriteTool::new(Arc::new(LocalFileSystem), root.clone());
        tool.journal = Some(guard.write_journal());
        sqlx::raw_sql(invalidate)
            .execute(database.pool())
            .await
            .expect("invalidate owner");

        // Act
        let error = tool
            .execute(&arguments(), "call")
            .await
            .expect_err("lost owner");
        let records = WriteRecordRow::load(database.pool(), "session")
            .await
            .expect("journal");
        guard.disarm();

        // Assert
        let source = match error {
            WriteError::WriteTarget { source, .. } => Some(source),
            _ => None,
        }
        .expect("expected persistence error");
        assert!(matches!(
            source
                .get_ref()
                .and_then(|error| error.downcast_ref::<SessionError>()),
            Some(SessionError::OwnershipLost {
                turn_position: 0,
                ..
            })
        ));
        assert!(!root.join("file.txt").exists());
        assert_eq!(records, Vec::<WriteRecord>::new());
    }
}

#[tokio::test]
async fn journal_reports_corrupt_status_and_unavailable_storage() {
    // Arrange
    let (database, mut guard) = fixture().await;
    guard
        .write_journal()
        .intent("call", Path::new("repo"), "file.txt", None, b"new")
        .await
        .expect("intent");
    fail(&database, &mut guard).await;
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(database.pool())
        .await
        .expect("disable check for corruption test");
    sqlx::query("UPDATE session_write SET status = 'invalid'")
        .execute(database.pool())
        .await
        .expect("corrupt status");

    // Act
    let invalid = WriteRecordRow::load(database.pool(), "session").await;
    sqlx::query("DROP TABLE session_write")
        .execute(database.pool())
        .await
        .expect("remove storage");
    let missing = WriteRecordRow::load(database.pool(), "session").await;

    // Assert
    assert!(matches!(invalid, Err(SessionError::InvalidData { .. })));
    assert!(matches!(
        missing,
        Err(SessionError::QueryContext {
            operation: "load persistent writes",
            ..
        })
    ));
}

struct InspectingFileSystem {
    pool: SqlitePool,
}

#[async_trait]
impl FileSystem for InspectingFileSystem {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        LocalFileSystem.canonicalize(path).await
    }

    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        LocalFileSystem.open_beneath(root, path).await
    }

    async fn replace_beneath(
        &self,
        root: &Path,
        path: &Path,
        expected: Option<Vec<u8>>,
        content: Vec<u8>,
    ) -> io::Result<()> {
        let records = WriteRecordRow::load(&self.pool, "session")
            .await
            .expect("committed intent");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, WriteStatus::Pending);
        assert_eq!(records[0].call_id, "create");
        assert_eq!(records[0].expected_hash, None);
        assert_eq!(records[0].resulting_hash, content_hash(&content));
        assert!(!root.join(path).exists());

        LocalFileSystem
            .replace_beneath(root, path, expected, content)
            .await
    }
}

#[tokio::test]
async fn intent_is_committed_before_replacement_and_outcome_before_return() {
    // Arrange
    let (database, mut guard) = fixture().await;
    let directory = tempdir().expect("repository");
    let mut tool = WriteTool::new(
        Arc::new(InspectingFileSystem {
            pool: database.pool().clone(),
        }),
        directory.path().to_path_buf(),
    );
    tool.journal = Some(guard.write_journal());

    // Act
    tool.execute(&arguments(), "create").await.expect("write");
    let records = WriteRecordRow::load(database.pool(), "session")
        .await
        .expect("outcome");
    guard.disarm();

    // Assert
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(
        tokio::fs::read(directory.path().join("file.txt"))
            .await
            .expect("file"),
        b"new\n"
    );
}
