//! Harness failures without a SQLite backend: foreign comparison scopes,
//! store faults while settling a turn, and resumed sessions whose stored
//! model identity does not match.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::Notify;

use super::support::{
    PendingToolFileSystem, model, object_schema, read_call, response_without_metadata,
};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::fault_store_test::{Fault, FaultStore};
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, TurnErrorType};
use crate::model::{MockModel, ModelError, ModelMetadata, ModelResponse};
use crate::recovery::{ExecutionIdentity, HostTurnStatus};
use crate::repository::Repository;
use crate::store::MemoryStore;
use crate::{
    ComparisonBase, LimitKind, RunLimits, RunRequest, SessionError, ToolSet, TurnError, TurnOptions,
};

/// Returns a model reporting `metadata` that accepts every input.
fn identified_model(metadata: Option<ModelMetadata>) -> MockModel {
    let mut model = MockModel::new();
    model.expect_metadata().return_const(metadata);
    model.expect_validate_input().returning(|_| Ok(()));

    model
}

fn metadata(provider: &'static str, model: &str) -> ModelMetadata {
    ModelMetadata::new(provider, model).expect("metadata should be valid")
}

#[tokio::test]
async fn runs_with_a_foreign_comparison_base_fail_before_calling_the_model() {
    // Arrange
    let harness =
        Harness::new(model(), unbounded_context_budget()).repository(Repository::fixture("repo"));
    let request = RunRequest::new("compare")
        .output(object_schema())
        .comparison_base(ComparisonBase::fixture("other"));

    // Act
    let outcome = harness.run(request).await;

    // Assert
    let error = outcome.result.expect_err("scope mismatch");
    let turn_error = match error.get_ref().downcast_ref::<SessionError>() {
        Some(SessionError::Turn(turn_error)) => turn_error,
        _ => error
            .get_ref()
            .downcast_ref::<TurnError>()
            .expect("turn failure"),
    };
    assert!(matches!(
        turn_error,
        TurnError::ComparisonRepositoryMismatch
    ));
    assert_eq!(
        turn_error.error_type(),
        TurnErrorType::ComparisonRepositoryMismatch
    );
}

#[tokio::test]
async fn completion_persistence_failure_reports_a_session_failure() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(2).returning(|_| {
        Ok(response_without_metadata(ModelResponse::Output(
            json!({"summary": "done"}),
        )))
    });
    let store = Arc::new(FaultStore::new());
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let harness = Harness::new(model, unbounded_context_budget())
        .store(store.clone())
        .with_lifecycle_observer(move |event: LifecycleEvent| {
            recorded.lock().expect("events lock").push(event);
        });
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    store.inject(Fault::CompleteTurn);

    // Act
    let controlled = session.turn("answer").start();
    let control = controlled.control();
    let controlled_result = controlled.await;
    let sent_result = session.send("answer again").await;

    // Assert
    assert!(matches!(controlled_result, Err(SessionError::Store { .. })));
    assert!(matches!(sent_result, Err(SessionError::Store { .. })));
    assert_eq!(control.command_outcomes(), Vec::new());
    let failures = events
        .lock()
        .expect("events lock")
        .iter()
        .filter_map(|event| match event.kind() {
            LifecycleEventKind::TurnFailed { error_type, .. } => Some(*error_type),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(failures, [TurnErrorType::Session; 2]);
}

#[tokio::test(start_paused = true)]
async fn wall_time_limit_during_a_tool_call_interrupts_the_host_turn() {
    // Arrange
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    let mut model = model();
    model.expect_complete().returning(move |_| {
        let response = if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            ModelResponse::ToolCall(read_call("pending-read"))
        } else {
            ModelResponse::Output(json!({"summary": "done"}))
        };

        Ok(response_without_metadata(response))
    });
    let harness = Harness::new(model, unbounded_context_budget())
        .store(Arc::new(MemoryStore::new()))
        .execution_identity(ExecutionIdentity::new("injected", "v1").expect("identity"))
        .repository(Repository::fixture("repo"))
        .tools(ToolSet::empty().with_read())
        .file_system(PendingToolFileSystem {
            started: Arc::new(Notify::new()),
        });
    let mut session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session should be created");
    let options = TurnOptions::from_output(object_schema())
        .with_tools(ToolSet::empty().with_read())
        .with_limits(RunLimits::default().with_max_wall_time(Duration::from_secs(1)));

    // Act
    let controlled = session
        .turn("read the pending file")
        .options(options)
        .host_id("limited")
        .start();
    let control = controlled.control();
    let result = controlled.await;
    control.settled().await.expect("dropped tool call settles");
    let record = session
        .recover("limited")
        .await
        .expect("record lookup")
        .expect("host turn record");
    let next = session.send("answer").await;

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::Turn(TurnError::LimitReached {
            limit: LimitKind::WallTime
        }))
    ));
    assert_eq!(
        record.status,
        HostTurnStatus::Interrupted {
            error_type: "run_limit_wall_time".to_string()
        }
    );
    assert_eq!(
        next.expect("the session runs its next turn").output(),
        &json!({"summary": "done"})
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failure_persistence_failure_keeps_the_turn_failure() {
    // Arrange
    let mut model = model();
    model
        .expect_complete()
        .times(1)
        .returning(|_| Err(ModelError::InvalidResponse));
    let store = Arc::new(FaultStore::new());
    let harness = Harness::new(model, unbounded_context_budget()).store(store.clone());
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    store.inject(Fault::FailTurn);

    // Act
    let result = session.send("answer").await;

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::TurnPersistence {
            turn: TurnError::Model(ModelError::InvalidResponse),
            ..
        })
    ));
}

#[tokio::test]
async fn resume_rejects_another_or_unavailable_model_and_incomplete_identity() {
    // Arrange
    let store = Arc::new(FaultStore::new());
    let original = Harness::new(
        identified_model(Some(metadata("provider-a", "model-a"))),
        unbounded_context_budget(),
    )
    .store(store.clone());
    original
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    let other = Harness::new(
        identified_model(Some(metadata("provider-b", "model-b"))),
        unbounded_context_budget(),
    )
    .store(store.clone());
    let anonymous =
        Harness::new(identified_model(None), unbounded_context_budget()).store(store.clone());

    // Act
    let switched = other.resume("session").await.err();
    let unavailable = anonymous.resume("session").await.err();
    store.inject(Fault::IncompleteModel);
    let incomplete = original.resume("session").await.err();

    // Assert
    assert!(matches!(
        switched,
        Some(SessionError::ModelMismatch { actual_model, stored_model, .. })
            if actual_model == "model-b" && stored_model == "model-a"
    ));
    assert!(matches!(
        unavailable,
        Some(SessionError::ModelMismatch { actual_provider, .. })
            if actual_provider == "unavailable"
    ));
    assert!(matches!(incomplete, Some(SessionError::InvalidData { .. })));
}
