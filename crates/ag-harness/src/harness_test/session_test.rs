use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mockall::Sequence;
use serde_json::json;
use tempfile::tempdir;

use super::support::{
    model, object_schema, read_call, response_without_metadata, resume_fallback_model,
    send_with_resumed_session, turn_started_id, wait_for_stored_turn_state,
};
use crate::harness::{Harness, SessionHistory, retained_bytes};
use crate::lifecycle::{LifecycleEventKind, ModelResponseType, TurnErrorType};
use crate::model::{ModelError, ModelErrorType, ModelMessage, ModelResponse};
use crate::session::{Database, SessionError};
use crate::turn::TurnError;

#[tokio::test]
async fn session_retains_successful_conversation_history() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            assert_eq!(
                request.messages(),
                &[ModelMessage::User("first question".to_string())]
            );

            return Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first answer"
            }))));
        }
        assert_eq!(
            request.messages(),
            &[
                ModelMessage::User("first question".to_string()),
                ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
                ModelMessage::User("second question".to_string()),
            ]
        );

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "second answer"
        }))))
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    let first = session
        .send("first question")
        .await
        .expect("first chat turn should succeed");
    let second = session
        .send("second question")
        .await
        .expect("second chat turn should succeed");

    // Assert
    assert_eq!(first.output(), &json!({"summary": "first answer"}));
    assert_eq!(second.output(), &json!({"summary": "second answer"}));
    assert_eq!(second.report().model_requests().len(), 1);
    assert!(second.report().duration() >= second.report().model_requests()[0].duration());
}

#[tokio::test]
async fn stale_session_handles_acquire_current_canonical_state() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            assert_eq!(
                request.messages(),
                &[ModelMessage::User("first question".to_string())]
            );
            assert_eq!(request.provider_session_id(), None);

            return Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first answer"
            })))
            .with_provider_session_id("native-session"));
        }
        assert_eq!(
            request.messages(),
            &[
                ModelMessage::User("first question".to_string()),
                ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
                ModelMessage::User("second question".to_string()),
            ]
        );
        assert_eq!(request.provider_session_id(), Some("native-session"));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "second answer"
        }))))
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut first = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    let mut stale = harness
        .resume("session-a")
        .await
        .expect("second handle should resume before the first turn");

    // Act
    first
        .send("first question")
        .await
        .expect("first handle should complete its turn");
    let outcome = stale
        .send("second question")
        .await
        .expect("stale handle should refresh before its turn");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "second answer" }));
}

#[tokio::test]
async fn session_sends_the_system_prompt_on_every_turn() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let expected = if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ModelMessage::System("read-only instructions".to_string()),
                ModelMessage::User("first".to_string()),
            ]
        } else {
            vec![
                ModelMessage::System("read-only instructions".to_string()),
                ModelMessage::User("first".to_string()),
                ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                ModelMessage::User("second".to_string()),
            ]
        };
        assert_eq!(request.messages(), expected);

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": if expected.len() == 2 { "one" } else { "two" }
        }))))
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .system_prompt("read-only instructions")
        .create()
        .await
        .expect("session should be created");

    // Act
    session
        .send("first")
        .await
        .expect("first chat turn should succeed");
    let second = session
        .send("second")
        .await
        .expect("second chat turn should succeed");

    // Assert
    assert_eq!(second.output(), &json!({"summary": "two"}));
}

#[test]
fn chat_history_evicts_complete_tool_turns() {
    // Arrange
    let tool_turn = vec![
        ModelMessage::User("inspect".to_string()),
        ModelMessage::AssistantToolCall(read_call("call_read")),
        ModelMessage::ToolResult {
            call_id: "call_read".to_string(),
            content: "file contents".to_string(),
            name: "read".to_string(),
        },
        ModelMessage::Assistant(r#"{"summary":"old"}"#.to_string()),
    ];
    let latest_turn = vec![
        ModelMessage::User("latest".to_string()),
        ModelMessage::Assistant(r#"{"summary":"new"}"#.to_string()),
    ];
    let max_bytes = retained_bytes(&tool_turn).max(retained_bytes(&latest_turn));
    let mut history = SessionHistory::new(max_bytes);

    // Act
    history.push(tool_turn);
    history.push(latest_turn.clone());

    // Assert
    assert_eq!(history.messages(), latest_turn);
    assert!(history.bytes <= max_bytes);
}

#[tokio::test]
async fn session_applies_the_configured_history_budget() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(3).returning(move |request| {
        match call_count.fetch_add(1, Ordering::SeqCst) {
            0 => {
                assert_eq!(
                    request.messages(),
                    &[ModelMessage::User("first".to_string())]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "xxxxxxxxxxxxxxxxxxxx"
                }))))
            }
            1 => {
                assert_eq!(request.messages().len(), 3);

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "two"
                }))))
            }
            _ => {
                assert_eq!(
                    request.messages(),
                    &[
                        ModelMessage::User("second".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"two"}"#.to_string()),
                        ModelMessage::User("third".to_string()),
                    ]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "three"
                }))))
            }
        }
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model)
        .database(directory.path().join("harness.db"))
        .max_history_bytes(NonZeroUsize::new(50).expect("history budget should be nonzero"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    session
        .send("first")
        .await
        .expect("first chat turn should succeed");
    session
        .send("second")
        .await
        .expect("second chat turn should succeed");
    let third = session
        .send("third")
        .await
        .expect("third chat turn should succeed");

    // Assert
    assert_eq!(third.output(), &json!({"summary": "three"}));
}

#[tokio::test]
async fn sequential_resumed_handles_reload_completed_canonical_history() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        let expected = if call_index == 0 {
            vec![ModelMessage::User("first".to_string())]
        } else {
            vec![
                ModelMessage::User("first".to_string()),
                ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                ModelMessage::User("second".to_string()),
            ]
        };
        assert_eq!(request.messages(), expected);

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": if call_index == 0 { "one" } else { "two" }
        }))))
    });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut first_handle = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    let mut stale_handle = harness
        .resume("session-a")
        .await
        .expect("second handle should resume before the first turn");

    // Act
    let first = first_handle
        .send("first")
        .await
        .expect("first handle should complete its turn");
    let second = stale_handle
        .send("second")
        .await
        .expect("stale handle should reload the completed turn");

    // Assert
    assert_eq!(first.output(), &json!({"summary": "one"}));
    assert_eq!(second.output(), &json!({"summary": "two"}));
}

#[tokio::test]
async fn session_does_not_replay_a_failed_turn() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
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
            assert_eq!(
                request.messages(),
                &[ModelMessage::User("retry".to_string())]
            );

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    let error = session
        .send("failed question")
        .await
        .expect_err("the first turn should fail");
    let recovered = session
        .send("retry")
        .await
        .expect("the next turn should start from clean history");

    // Assert
    assert!(matches!(
        error,
        SessionError::Turn(TurnError::Model(ModelError::InvalidResponse))
    ));
    assert_eq!(recovered.output(), &json!({"summary": "recovered"}));
}

#[tokio::test]
async fn session_invalidates_provider_resume_state_after_a_failed_turn() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id().is_none())
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first"
            })))
            .with_provider_session_id("native-session"))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id() == Some("native-session"))
        .returning(|_| {
            Err(ModelError::SchemaViolation {
                path: "/summary".to_string(),
                reason: "required property is missing".to_string(),
            })
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| {
            request.provider_session_id().is_none()
                && request.messages()
                    == [
                        ModelMessage::User("first".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"first"}"#.to_string()),
                        ModelMessage::User("retry".to_string()),
                    ]
        })
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    session
        .send("first")
        .await
        .expect("first turn should succeed");

    // Act
    let error = session
        .send("failed")
        .await
        .expect_err("second turn should fail");
    drop(session);
    let mut resumed = harness
        .resume("session-a")
        .await
        .expect("session should reopen");
    let recovered = resumed
        .send("retry")
        .await
        .expect("replayed turn should succeed");

    // Assert
    assert!(matches!(
        &error,
        SessionError::Turn(TurnError::Model(ModelError::SchemaViolation { path, reason }))
            if path == "/summary" && reason == "required property is missing"
    ));
    assert_eq!(recovered.output(), &json!({"summary": "recovered"}));
}

#[tokio::test]
async fn session_accounts_for_resume_fallback_and_persists_its_continuation() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Harness::new(resume_fallback_model())
        .database(directory.path().join("harness.db"))
        .with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    session
        .send("first")
        .await
        .expect("first turn should succeed");
    drop(session);
    let mut session = harness
        .resume("session-a")
        .await
        .expect("session should resume");

    // Act
    let outcome = session
        .send("second")
        .await
        .expect("history replay should succeed");
    drop(session);
    let mut session = harness
        .resume("session-a")
        .await
        .expect("session should resume with replacement continuation");
    let third = session
        .send("third")
        .await
        .expect("replacement continuation should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({"summary": "second"}));
    assert_eq!(outcome.report().model_requests().len(), 2);
    assert_eq!(
        outcome.report().model_requests()[0].response_type(),
        ModelResponseType::ResumeUnavailable
    );
    assert_eq!(
        outcome.report().model_requests()[1].response_type(),
        ModelResponseType::Output
    );
    assert_eq!(third.output(), &json!({"summary": "third"}));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert!(matches!(
        events[5].kind(),
        LifecycleEventKind::ModelRequestStarted {
            request_index: 0,
            ..
        }
    ));
    assert!(matches!(
        events[6].kind(),
        LifecycleEventKind::ModelRequestFailed {
            error_type: ModelErrorType::Provider,
            ..
        }
    ));
    assert!(matches!(
        events[7].kind(),
        LifecycleEventKind::ModelRequestStarted {
            request_index: 1,
            ..
        }
    ));
    assert!(matches!(
        events[8].kind(),
        LifecycleEventKind::ModelRequestCompleted {
            response_type: ModelResponseType::Output,
            ..
        }
    ));
}

#[tokio::test]
async fn session_preserves_structured_failure_after_native_resume_fallback() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first"
            })))
            .with_provider_session_id("native-session"))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id() == Some("native-session"))
        .returning(|_| Err(ModelError::ResumeUnavailable));
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id().is_none())
        .returning(|_| Err(ModelError::ResponseBodyTooLarge));
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    session
        .send("first")
        .await
        .expect("first turn should succeed");

    // Act
    let error = session
        .send("second")
        .await
        .expect_err("history replay should fail");

    // Assert
    assert!(matches!(
        &error,
        SessionError::Turn(TurnError::Model(ModelError::ResponseBodyTooLarge))
    ));
}

#[tokio::test]
async fn concurrent_session_creation_and_resume_share_one_database_pool() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let harness = Harness::new(model()).database(directory.path().join("harness.db"));
    assert!(harness.database.get().is_none());

    // Act
    let (first, second) = tokio::join!(
        harness.session("first", object_schema()).create(),
        harness.session("second", object_schema()).create()
    );
    let first = first.expect("first session should be created");
    let second = second.expect("second session should be created");
    let resumed = harness
        .resume("first")
        .await
        .expect("session should resume");
    first.database.pool().close().await;

    // Assert
    assert!(second.database.pool().is_closed());
    assert!(resumed.database.pool().is_closed());
    assert!(
        harness
            .database
            .get()
            .expect("database should be initialized")
            .pool()
            .is_closed()
    );
}

#[tokio::test]
async fn database_initialization_retries_after_failure_and_resets_on_reconfiguration() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let parent = directory.path().join("blocked");
    tokio::fs::write(&parent, "not a directory")
        .await
        .expect("blocking file should exist");
    let harness = Harness::new(model()).database(parent.join("harness.db"));

    // Act
    let error = harness
        .session("first", object_schema())
        .create()
        .await
        .err();
    assert!(harness.database.get().is_none());
    tokio::fs::remove_file(&parent)
        .await
        .expect("blocking file should be removed");
    let session = harness
        .session("first", object_schema())
        .create()
        .await
        .expect("initialization should retry");
    let original = session.database.clone();
    drop(session);
    let harness = harness.database(directory.path().join("other.db"));
    let missing = harness.resume("first").await.err();
    let session = harness
        .session("first", object_schema())
        .create()
        .await
        .expect("new database should allow the same id");
    original.pool().close().await;

    // Assert
    assert!(matches!(error, Some(SessionError::Io(_))));
    assert!(matches!(missing, Some(SessionError::NotFound { .. })));
    assert!(!session.database.pool().is_closed());
}

#[tokio::test]
async fn completion_persistence_failure_interrupts_the_session_turn() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Arc::new(
        Harness::new(model)
            .database(&database_path)
            .with_lifecycle_observer(move |event| {
                observed_events
                    .lock()
                    .expect("events should lock")
                    .push(event);
            }),
    );
    let session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    drop(session);
    let database = harness.open_database().await.expect("database should open");
    sqlx::query(
        r"
CREATE TRIGGER reject_turn_completion
BEFORE UPDATE OF status ON session_turn
WHEN NEW.status = 'completed'
BEGIN
    SELECT RAISE(ABORT, 'injected completion persistence failure');
END
",
    )
    .execute(database.pool())
    .await
    .expect("completion failure trigger should be created");

    // Act
    let error = send_with_resumed_session(Arc::clone(&harness), "complete").await;
    let database = Database::open(&database_path)
        .await
        .expect("database should reopen");
    let expected_state = ("interrupted".to_string(), Some("interrupted".to_string()));
    let state = wait_for_stored_turn_state(&database, &expected_state).await;

    // Assert
    assert!(
        error
            .expect_err("completion persistence should fail")
            .to_string()
            .contains("injected completion persistence failure")
    );
    assert_eq!(state, expected_state);
    let events = events.lock().expect("events should lock");
    assert_eq!(events.len(), 4);
    let turn_id = turn_started_id(&events[0]).expect("turn should start");
    assert!(matches!(
        events[3].kind(),
        LifecycleEventKind::TurnFailed {
            error_type: TurnErrorType::Session,
            turn_id: event_turn_id,
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind(), LifecycleEventKind::TurnCompleted { .. }))
    );
}

#[tokio::test]
async fn session_error_preserves_model_and_failure_persistence_errors() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let mut model = model();
    model
        .expect_complete()
        .times(1)
        .returning(|_| Err(ModelError::InvalidResponse));
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Harness::new(model)
        .database(&database_path)
        .with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("events should lock")
                .push(event);
        });
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    sqlx::query(
        r"
CREATE TRIGGER reject_turn_failure
BEFORE UPDATE OF status ON session_turn
WHEN NEW.status = 'failed'
BEGIN
    SELECT RAISE(ABORT, 'injected persistence failure');
END
",
    )
    .execute(session.database.pool())
    .await
    .expect("failure trigger should be created");

    // Act
    let error = session
        .send("fail twice")
        .await
        .expect_err("model and persistence failures should be returned");

    // Assert
    assert!(matches!(&error, SessionError::TurnPersistence { .. }));
    let events = events.lock().expect("events should lock");
    assert_eq!(events.len(), 4);
    assert!(matches!(
        events[3].kind(),
        LifecycleEventKind::TurnFailed {
            error_type: TurnErrorType::Model(ModelErrorType::InvalidResponse),
            ..
        }
    ));
    if let SessionError::TurnPersistence { turn, persistence } = error {
        assert!(matches!(
            turn,
            TurnError::Model(ModelError::InvalidResponse)
        ));
        assert!(
            persistence
                .to_string()
                .contains("injected persistence failure")
        );
    }
}
