use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mockall::Sequence;
use serde_json::{Value, json};
use tempfile::tempdir;

use super::support::{model, object_schema, response_without_metadata, write_call};
use crate::file_system::MockFileSystem;
use crate::harness::{
    Harness, MAX_WRITE_DIAGNOSTIC_BYTES, WriteDiagnostic, retained_bytes, write_diagnostics,
};
use crate::model::{ModelError, ModelMessage, ModelResponse};
use crate::repository::Repository;
use crate::session::SessionError;
use crate::tool::{Tool, WriteArguments};
use crate::turn::TurnError;
use crate::write_journal::{WriteRecord, WriteRecordRow};

#[tokio::test]
async fn session_exposes_applied_writes_after_failure_and_reopen() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call(
                    "write-call",
                    "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                ),
            )))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(ModelError::InvalidResponse));
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|request| {
            assert_eq!(request.messages().len(), 2);
            assert!(
                matches!(&request.messages()[0], ModelMessage::System(context)
            if context.contains("write-call") && context.contains("applied"))
            );
            assert!(request.provider_session_id().is_none());

            Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "recovered"}),
            )))
        });
    let directory = tempdir().expect("repository");
    let harness = Harness::new(model)
        .database(directory.path().join("harness.db"))
        .repository(Repository::fixture(directory.path()))
        .allow(Tool::Write);
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let error = session
        .send("write and fail")
        .await
        .expect_err("model failure");
    let before = session.writes().await.expect("partial execution");
    drop(session);
    let mut reopened = harness.resume("session-a").await.expect("reopen");
    let after = reopened.writes().await.expect("durable execution");
    let retry = reopened.send("retry").await.expect("retry");
    let history = reopened
        .database
        .load_session("session-a")
        .await
        .expect("history");

    // Assert
    assert!(matches!(
        error,
        SessionError::Turn(TurnError::Model(ModelError::InvalidResponse))
    ));
    assert_eq!(before, after);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].status, crate::WriteStatus::Applied);
    assert_eq!(
        tokio::fs::read(directory.path().join("src/lib.rs"))
            .await
            .expect("file"),
        b"new\n"
    );
    assert_eq!(retry.output(), &json!({"summary": "recovered"}));
    assert_eq!(
        history.turns,
        vec![vec![
            ModelMessage::System(write_diagnostics(&after, after.len()).0),
            ModelMessage::User("retry".to_string()),
            ModelMessage::Assistant(r#"{"summary":"recovered"}"#.to_string()),
        ]]
    );
}

#[tokio::test]
async fn recovery_snapshot_obeys_the_whole_turn_history_budget() {
    // Arrange
    for budget in [64, 4096] {
        let mut model = model();
        model.expect_complete().once().returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "done"}),
            )))
        });
        let directory = tempdir().expect("database directory");
        let harness = Harness::new(model)
            .database(directory.path().join("harness.db"))
            .max_history_bytes(NonZeroUsize::new(budget).expect("positive budget"));
        let mut session = harness
            .session("budget", object_schema())
            .create()
            .await
            .expect("session");
        let mut acquired = session
            .database
            .begin_turn("budget", "write")
            .await
            .expect("turn");
        let journal = acquired.guard.write_journal();
        let id = journal
            .intent("write-call", directory.path(), "file.txt", None, b"new")
            .await
            .expect("intent");
        journal.finish(id, true).await.expect("outcome");
        session
            .database
            .fail_turn(
                "budget",
                acquired.turn_position,
                &TurnError::Model(ModelError::InvalidResponse),
            )
            .await
            .expect("failed turn");
        acquired.guard.disarm();

        // Act
        session.send("recover").await.expect("recovery");
        let in_memory = session.history.messages();
        let reopened = harness.resume("budget").await.expect("reopen");
        let persisted = reopened.history.messages();
        let (_, pending) = WriteRecordRow::load_pending_page(session.database.pool(), "budget")
            .await
            .expect("pending count");

        // Assert
        assert_eq!(in_memory, persisted);
        assert_eq!(pending, 0);
        assert!(retained_bytes(&persisted) <= budget);
        if budget == 64 {
            assert!(persisted.is_empty(), "snapshot must count toward eviction");
        } else {
            assert_eq!(persisted.len(), 3);
            assert!(matches!(&persisted[0], ModelMessage::System(context)
                if context.contains("write-call")));
            assert_eq!(persisted[1], ModelMessage::User("recover".to_string()));
        }
    }
}

#[test]
fn write_diagnostics_bounds_context_and_keeps_latest_records_in_order() {
    // Arrange
    let record = diagnostic_record("file.txt");
    let mut records = vec![record; 100];
    for (position, record) in records.iter_mut().enumerate() {
        record.turn_position = i64::try_from(position).expect("position");
        record.id = record.turn_position;
    }

    // Act
    let (context, acknowledged_writes) = write_diagnostics(&records, records.len());

    // Assert
    assert!(context.len() < MAX_WRITE_DIAGNOSTIC_BYTES + 512);
    assert!(!context.contains("repository_root"));
    assert!(!context.contains("host-user"));
    assert!(!context.contains("secret-project"));
    assert!(!acknowledged_writes.contains(&0));
    assert!(acknowledged_writes.contains(&99));
    let (_, encoded) = context.split_once(": [").expect("records");
    let encoded: Vec<Value> =
        serde_json::from_str(&format!("[{encoded}")).expect("diagnostic JSON");
    assert_eq!(acknowledged_writes.len(), encoded.len());
    assert!(!context.contains(r#""turn_position":0"#));
    let penultimate = context
        .find(r#""turn_position":98"#)
        .expect("penultimate write");
    let last = context.find(r#""turn_position":99"#).expect("latest write");
    assert!(penultimate < last);
}

#[test]
fn write_diagnostics_bounds_escaped_paths_without_splitting_unicode() {
    for (path, truncated) in [
        ("\u{1}".repeat(4096), true),
        (
            format!("{}é{}", "\u{1}".repeat(2047), "\u{1}".repeat(2047)),
            true,
        ),
        ("a".repeat(4096), false),
    ] {
        // Arrange
        let arguments: WriteArguments =
            serde_json::from_value(json!({"path": path, "patch": "patch"}))
                .expect("path must be valid tool input");
        let mut record = diagnostic_record(arguments.path());
        record.call_id = "\u{1}".repeat(1024);
        record.expected_hash = Some("1".repeat(64));

        // Act
        let diagnostic =
            serde_json::to_value(WriteDiagnostic::from(&record)).expect("provider diagnostic");

        // Assert
        assert_eq!(diagnostic["path_truncated"], truncated);
        assert_eq!(diagnostic["call_id"], record.call_id);
        assert_eq!(
            diagnostic["expected_hash"],
            record.expected_hash.expect("expected fingerprint")
        );
        assert_eq!(diagnostic["resulting_hash"], record.resulting_hash);
        assert!(diagnostic.to_string().len() < MAX_WRITE_DIAGNOSTIC_BYTES);
        let prefix = diagnostic["path"].as_str().expect("path prefix");
        assert!(path.starts_with(prefix));
        assert_ne!(prefix, "");
        if truncated {
            assert!(
                serde_json::to_string(&path).expect("escaped path").len()
                    > MAX_WRITE_DIAGNOSTIC_BYTES
            );
            assert!(prefix.len() < path.len());
        } else {
            assert_eq!(prefix, path);
        }
    }
}

#[tokio::test]
async fn oversized_write_path_is_acknowledged_and_native_continuation_resumes() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .once()
        .in_sequence(&mut sequence)
        .returning(|request| {
            assert!(request.provider_session_id().is_none());
            let context = match &request.messages()[0] {
                ModelMessage::System(context) => Some(context),
                _ => None,
            }
            .expect("recovery diagnostics must be presented");
            let (_, records) = context.split_once(": [").expect("diagnostic records");
            let records: Vec<Value> = serde_json::from_str(&format!("[{records}")).expect("JSON");
            assert_eq!(records.len(), 2);
            assert_eq!(records[0]["call_id"], "older");
            assert_eq!(records[1]["call_id"], "newest");
            assert_eq!(records[1]["path_truncated"], true);
            assert!(context.contains("Truncated paths are prefixes"));

            Ok(
                response_without_metadata(ModelResponse::Output(json!({"summary": "recovered"})))
                    .with_provider_session_id("recovered-session"),
            )
        });
    model
        .expect_complete()
        .once()
        .in_sequence(&mut sequence)
        .returning(|request| {
            assert_eq!(request.provider_session_id(), Some("recovered-session"));
            assert!(
                matches!(&request.messages()[0], ModelMessage::System(context)
                    if context.contains("newest") && context.contains("path_truncated"))
            );

            Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "continued"}),
            )))
        });
    let directory = tempdir().expect("database directory");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session");
    let mut acquired = session
        .database
        .begin_turn("session-a", "write")
        .await
        .expect("turn");
    let journal = acquired.guard.write_journal();
    let path = "\u{1}".repeat(4096);
    for (call_id, path) in [("older", "file.txt"), ("newest", path.as_str())] {
        let id = journal
            .intent(call_id, directory.path(), path, None, b"new")
            .await
            .expect("intent");
        journal.finish(id, true).await.expect("applied outcome");
    }
    session
        .database
        .fail_turn(
            "session-a",
            acquired.turn_position,
            &TurnError::Model(ModelError::InvalidResponse),
        )
        .await
        .expect("failed turn");
    acquired.guard.disarm();

    // Act
    session.send("recover").await.expect("recovery turn");
    let pending = WriteRecordRow::load(session.database.pool(), "session-a", true, None)
        .await
        .expect("pending diagnostics");
    let writes = session.writes().await.expect("complete journal");
    drop(session);
    let mut reopened = harness.resume("session-a").await.expect("reopen");
    let outcome = reopened
        .send("continue")
        .await
        .expect("native continuation");

    // Assert
    assert_eq!(pending, Vec::<WriteRecord>::new());
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[1].path, path);
    assert_eq!(outcome.output(), &json!({"summary": "continued"}));
}

#[tokio::test]
async fn diagnostic_backlog_reconciles_only_the_payload_and_advances_after_success() {
    // Arrange
    let model = diagnostic_backlog_model();
    let paths = Arc::new(Mutex::new(Vec::new()));
    let observed_paths = Arc::clone(&paths);
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(3)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_open_beneath()
        .times(3)
        .returning(move |_, path| {
            observed_paths
                .lock()
                .expect("read paths")
                .push(path.to_path_buf());

            Ok(Box::new(Cursor::new(b"new")))
        });
    let directory = tempdir().expect("database directory");
    let harness = Harness::new(model)
        .database(directory.path().join("harness.db"))
        .repository(Repository::fixture("repo"))
        .file_system(file_system);
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session");
    let mut acquired = session
        .database
        .begin_turn("session-a", "write")
        .await
        .expect("turn");
    let journal = acquired.guard.write_journal();
    let mut ids = Vec::new();
    for index in 0..70 {
        let id = journal
            .intent(
                &format!("{}-{index:03}", "\u{1}".repeat(1020)),
                Path::new("/repo"),
                &format!("{}-{index:03}", "\u{1}".repeat(4092)),
                None,
                b"new",
            )
            .await
            .expect("intent");
        if index % 2 == 0 {
            journal.finish(id, false).await.expect("failed write");
        }
        ids.push(id);
    }
    session
        .database
        .fail_turn(
            "session-a",
            acquired.turn_position,
            &TurnError::Model(ModelError::InvalidResponse),
        )
        .await
        .expect("failed turn");
    acquired.guard.disarm();

    // Act
    let failed = session.send("retry").await.expect_err("model failure");
    let (_, after_failure) =
        WriteRecordRow::load_pending_page(session.database.pool(), "session-a")
            .await
            .expect("pending after failure");
    session.send("recover").await.expect("recovery");
    let (pending, after_success) =
        WriteRecordRow::load_pending_page(session.database.pool(), "session-a")
            .await
            .expect("pending after success");
    session.send("continue recovery").await.expect("next batch");
    let (_, remaining) = WriteRecordRow::load_pending_page(session.database.pool(), "session-a")
        .await
        .expect("remaining writes");

    // Assert
    assert!(matches!(
        failed,
        SessionError::Turn(TurnError::Model(ModelError::InvalidResponse))
    ));
    assert_eq!(after_failure, 70);
    assert_eq!(after_success, 69);
    assert_eq!(pending.last().expect("newest pending").id, ids[68]);
    assert_eq!(remaining, 68);
    let paths = paths.lock().expect("read paths");
    assert_eq!(paths.len(), 3);
    assert_eq!(paths[0], paths[1]);
    assert_ne!(paths[1], paths[2]);
}

fn diagnostic_backlog_model() -> crate::model::MockModel {
    let mut model = model();
    let calls = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(3).returning(move |request| {
        let index = calls.fetch_add(1, Ordering::SeqCst);
        assert!(request.provider_session_id().is_none());
        let context = request
            .messages()
            .iter()
            .rev()
            .find_map(|message| match message {
                ModelMessage::System(context) => Some(context),
                _ => None,
            })
            .expect("write diagnostics");
        let (_, encoded) = context.split_once(": [").expect("records");
        let records: Vec<Value> = serde_json::from_str(&format!("[{encoded}")).expect("JSON");
        assert_eq!(records.len(), 1);
        let expected = if index < 2 { "-069" } else { "-068" };
        assert!(
            records[0]["call_id"]
                .as_str()
                .expect("call id")
                .ends_with(expected)
        );
        assert_eq!(records[0]["recovery"], "result_matches");
        assert!(context.contains(if index < 2 {
            "69 earlier records omitted"
        } else {
            "68 earlier records omitted"
        }));
        if index == 0 {
            return Err(ModelError::InvalidResponse);
        }

        Ok(
            response_without_metadata(ModelResponse::Output(json!({"summary": "recovered"})))
                .with_provider_session_id("native-session"),
        )
    });

    model
}

fn diagnostic_record(path: &str) -> WriteRecord {
    WriteRecord {
        call_id: "write-call".to_string(),
        expected_hash: None,
        id: 0,
        path: path.to_string(),
        recovery: None,
        repository_root: PathBuf::from("/private/host-user/secret-project"),
        resulting_hash: "0".repeat(64),
        status: crate::WriteStatus::Applied,
        turn_position: 0,
    }
}
