use std::io::Cursor;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::tempdir;

use super::support::{model, object_schema, read_call, response_without_metadata};
use crate::comparison::support::ComparisonRepository;
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, TurnErrorType};
use crate::model::{ModelMessage, ModelRequest, ModelResponse, ReasoningEffort};
use crate::repository::Repository;
use crate::session::{Database, SessionError};
use crate::{ComparisonBase, OutputSchema, Tool, ToolPolicy, TurnError, TurnLimits, TurnOptions};

fn options(schema: OutputSchema, policy: ToolPolicy, limit: usize) -> TurnOptions {
    TurnOptions::new(
        schema,
        policy,
        TurnLimits::new(NonZeroUsize::new(limit).expect("nonzero budget")),
    )
}

fn readable_file_system() -> MockFileSystem {
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .returning(|path| Ok(path.to_path_buf()));
    file_system
        .expect_open_beneath()
        .returning(|_, _| Ok(Box::new(Cursor::new(b"[workspace]"))));

    file_system
}

#[tokio::test]
async fn equivalent_ephemeral_and_durable_turns_send_the_same_requests() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let recorded = Arc::clone(&requests);
    let mut model = model();
    model.expect_complete().times(4).returning(move |request| {
        let response = if matches!(request.messages().last(), Some(ModelMessage::User(_))) {
            ModelResponse::ToolCall(read_call("read"))
        } else {
            ModelResponse::Output(json!({"summary": "done"}))
        };
        recorded.lock().expect("requests lock").push(request);
        Ok(response_without_metadata(response))
    });
    let directory = tempdir().expect("temporary directory");
    let harness = Harness::new(model)
        .database(directory.path().join("history.db"))
        .repository(Repository::fixture("repo"))
        .file_system(readable_file_system())
        .model_reasoning_effort(ReasoningEffort::Low);
    let options = options(object_schema(), ToolPolicy::default().allow(Tool::Read), 2);

    // Act
    harness
        .run_once_with_options("read", options.clone())
        .await
        .expect("one-shot turn");
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    session
        .send_with_options("read", options)
        .await
        .expect("durable turn");

    // Assert
    let requests = requests.lock().expect("requests lock");
    for (once, durable) in requests[..2].iter().zip(&requests[2..]) {
        assert_eq!(once.messages(), durable.messages());
        assert_eq!(once.schema(), durable.schema());
        assert_eq!(once.tools(), durable.tools());
        assert_eq!(
            once.model_reasoning_effort(),
            durable.model_reasoning_effort()
        );
        assert_eq!(once.provider_session_id(), durable.provider_session_id());
    }
}

#[tokio::test]
async fn changing_options_uses_canonical_state_without_changing_session_defaults() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let recorded = Arc::clone(&requests);
    let mut model = model();
    model.expect_complete().times(6).returning(move |request| {
        let output = if request.schema().value()["type"] == "integer" {
            json!(42)
        } else {
            json!({"summary": "done"})
        };
        recorded.lock().expect("requests lock").push(request);
        Ok(response_without_metadata(ModelResponse::Output(output))
            .with_provider_session_id("native"))
    });
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("history.db");
    let harness = Harness::new(model)
        .database(&database_path)
        .repository(Repository::fixture("repo"));
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    let mut stale = harness.resume("session").await.expect("stale handle");
    let read = options(object_schema(), ToolPolicy::default().allow(Tool::Read), 1);
    let integer = OutputSchema::new(json!({"type":"integer"})).expect("integer schema");

    // Act
    session.send("default").await.expect("default turn");
    stale
        .send_with_options("policy change", read)
        .await
        .expect("policy change");
    session
        .send_with_options(
            "schema change",
            options(integer.clone(), ToolPolicy::default().allow(Tool::Read), 1),
        )
        .await
        .expect("schema change");
    stale
        .send_with_options(
            "budget change",
            options(integer, ToolPolicy::default().allow(Tool::Read), 2),
        )
        .await
        .expect("budget change");
    let mut reopened = harness.resume("session").await.expect("reopened session");
    reopened
        .send("defaults again")
        .await
        .expect("default turn after reopen");
    reopened
        .send("same defaults")
        .await
        .expect("compatible turn");
    let database = Database::open(&database_path).await.expect("database");
    let snapshots: Vec<String> =
        sqlx::query_scalar("SELECT turn_options FROM session_turn ORDER BY turn_position")
            .fetch_all(database.pool())
            .await
            .expect("snapshots");

    // Assert
    let requests = requests.lock().expect("requests lock");
    let continuations: Vec<_> = requests
        .iter()
        .map(ModelRequest::provider_session_id)
        .collect();
    assert_eq!(
        continuations,
        [None, None, None, Some("native"), None, Some("native")]
    );
    assert_eq!(requests[0].tools().len(), 0);
    assert_eq!(requests[1].tools().len(), 1);
    assert_eq!(requests[2].schema().value(), &json!({"type":"integer"}));
    assert_eq!(requests[4].schema(), &object_schema());
    assert_eq!(requests[4].tools().len(), 0);
    assert_eq!(requests[4].messages().len(), 9);
    let snapshots: Vec<Value> = snapshots
        .iter()
        .map(|snapshot| serde_json::from_str(snapshot).expect("snapshot JSON"))
        .collect();
    assert_eq!(
        snapshots[1]["tool_policy"],
        json!({"read":true, "write":false})
    );
    assert_eq!(snapshots[2]["max_tool_calls"], 1);
    assert_eq!(snapshots[3]["max_tool_calls"], 2);
    assert_eq!(snapshots[0], snapshots[4]);
}

#[tokio::test]
async fn empty_explicit_policy_denies_calls_despite_allowed_harness_defaults() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(2).returning(|request| {
        assert_eq!(request.tools().len(), 0);
        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("denied"),
        )))
    });
    let directory = tempdir().expect("temporary directory");
    let harness = Harness::new(model)
        .allow(Tool::Read)
        .allow(Tool::Write)
        .database(directory.path().join("history.db"));
    let denied = options(object_schema(), ToolPolicy::default(), 1);

    // Act
    let once = harness
        .run_once_with_options("denied", denied.clone())
        .await;
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    let durable = session.send_with_options("denied", denied).await;

    // Assert
    assert!(matches!(once, Err(TurnError::ToolDenied { .. })));
    assert!(matches!(
        durable,
        Err(SessionError::Turn(TurnError::ToolDenied { .. }))
    ));
}

#[tokio::test]
async fn explicit_budget_replaces_default_and_resets_for_the_next_turn() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(3).returning(|request| {
        let response = if matches!(request.messages().last(), Some(ModelMessage::User(_))) {
            ModelResponse::ToolCalls(vec![read_call("first"), read_call("second")])
        } else {
            ModelResponse::Output(json!({"summary":"done"}))
        };
        Ok(response_without_metadata(response))
    });
    let directory = tempdir().expect("temporary directory");
    let harness = Harness::new(model)
        .allow(Tool::Read)
        .repository(Repository::fixture("repo"))
        .file_system(readable_file_system())
        .max_tool_calls(NonZeroUsize::new(1).expect("nonzero budget"))
        .database(directory.path().join("history.db"));
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let allowed = session
        .send_with_options(
            "two calls",
            options(object_schema(), ToolPolicy::default().allow(Tool::Read), 2),
        )
        .await;
    let limited = session.send("two calls again").await;

    // Assert
    assert_eq!(
        allowed
            .expect("explicit budget")
            .report()
            .tool_calls()
            .len(),
        2
    );
    assert!(matches!(
        limited,
        Err(SessionError::Turn(TurnError::ToolCallLimit { limit: 1 }))
    ));
}

#[tokio::test]
async fn options_persistence_failure_prevents_execution_and_reports_session_failure() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("history.db");
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let harness = Harness::new(model())
        .database(&database_path)
        .with_lifecycle_observer(move |event: LifecycleEvent| {
            recorded.lock().expect("events lock").push(event);
        });
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    let database = Database::open(&database_path).await.expect("database");
    sqlx::query(
        "CREATE TRIGGER reject_options BEFORE INSERT ON session_turn WHEN NEW.turn_options IS NOT \
         NULL BEGIN SELECT RAISE(ABORT, 'options unavailable'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure trigger");

    // Act
    let result = session
        .send_with_options(
            "never executed",
            options(object_schema(), ToolPolicy::default(), 1),
        )
        .await;
    let messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_message")
        .fetch_one(database.pool())
        .await
        .expect("message count");

    // Assert
    assert!(matches!(result, Err(SessionError::QueryContext { .. })));
    assert_eq!(messages, 0);
    let events = events.lock().expect("events lock");
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        LifecycleEventKind::TurnFailed {
            error_type: TurnErrorType::Session,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind(), LifecycleEventKind::TurnCompleted { .. }))
    );
}

#[tokio::test]
async fn failed_schema_override_does_not_leak_into_the_next_turn() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(2).returning(|_| {
        Ok(response_without_metadata(ModelResponse::Output(
            json!({"summary":"done"}),
        )))
    });
    let directory = tempdir().expect("temporary directory");
    let harness = Harness::new(model).database(directory.path().join("history.db"));
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    let integer = OutputSchema::new(json!({"type":"integer"})).expect("integer schema");

    // Act
    let invalid = session
        .send_with_options("integer", options(integer, ToolPolicy::default(), 1))
        .await;
    let valid = session.send("default schema").await;

    // Assert
    assert!(matches!(
        invalid,
        Err(SessionError::Turn(TurnError::Model(_)))
    ));
    assert_eq!(
        valid.expect("default schema remains valid").output(),
        &json!({"summary":"done"})
    );
}

#[tokio::test]
async fn comparison_changes_clear_native_continuation_using_persisted_options() {
    // Arrange
    let fixture = ComparisonRepository::new().await;
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let recorded = Arc::clone(&requests);
    let mut model = model();
    model.expect_complete().times(4).returning(move |request| {
        recorded.lock().expect("requests").push(request);
        Ok(
            response_without_metadata(ModelResponse::Output(json!({"summary":"done"})))
                .with_provider_session_id("native"),
        )
    });
    let harness = Harness::new(model)
        .repository(fixture.repository.clone())
        .database(fixture.directory.path().join("session.db"));
    let without = options(object_schema(), ToolPolicy::default().allow(Tool::Read), 2);
    let first = without.clone().with_comparison_base(
        ComparisonBase::validate(&fixture.repository, &fixture.base)
            .await
            .expect("base"),
    );
    let second = without.clone().with_comparison_base(
        ComparisonBase::validate(&fixture.repository, &fixture.next)
            .await
            .expect("next"),
    );
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    let mut stale = harness.resume("session").await.expect("stale handle");

    // Act
    session
        .send_with_options("base", first.clone())
        .await
        .expect("first");
    stale
        .send_with_options("same", first)
        .await
        .expect("same base");
    session
        .send_with_options("next", second)
        .await
        .expect("new base");
    let mut reopened = harness.resume("session").await.expect("reopen");
    reopened
        .send_with_options("no comparisons", without)
        .await
        .expect("removed base");

    // Assert
    let requests = requests.lock().expect("requests");
    assert_eq!(
        requests
            .iter()
            .map(ModelRequest::provider_session_id)
            .collect::<Vec<_>>(),
        [None, Some("native"), None, None]
    );
    assert!(requests[0].tools()[0].description().contains(&fixture.base));
    assert!(requests[2].tools()[0].description().contains(&fixture.next));
    assert_eq!(requests[3].messages().len(), 7);
}

#[tokio::test]
async fn comparison_scope_mismatch_fails_before_calling_the_model() {
    // Arrange
    let base = ComparisonBase::fixture("other");
    let selected = options(object_schema(), ToolPolicy::default(), 2).with_comparison_base(base);
    let harness = Harness::new(model()).repository(Repository::fixture("repo"));

    // Act
    let result = harness
        .run_once_with_options("wrong repository", selected)
        .await;

    // Assert
    let error = result.expect_err("scope mismatch");
    assert!(matches!(error, TurnError::ComparisonRepositoryMismatch));
    assert_eq!(error.error_type(), TurnErrorType::RepositoryRequired);
}
