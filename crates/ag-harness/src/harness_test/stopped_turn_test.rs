use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::Notify;

use super::support::{
    inspection_call, model, object_schema, read_call, read_harness, readable_file_system,
    response_without_metadata, write_call,
};
use crate::Harness;
use crate::file_system::MockFileSystem;
use crate::gated_store_test::{GatedStore, PauseAt};
use crate::lifecycle::TurnErrorType;
use crate::memory_store::MemoryStore;
use crate::model::{
    MockModel, Model, ModelCompletion, ModelError, ModelErrorType, ModelMessage, ModelRequest,
    ModelResponse,
};
use crate::session::{Database, SessionError};
use crate::store::{SessionStore, StoppedTurn};
use crate::tool::ToolCall;
use crate::turn::TurnError;

/// Calls one tool, then waits for cancellation, then answers later turns.
struct CancelledAfterToolModel {
    completions: AtomicUsize,
    entered: Arc<Notify>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl Model for CancelledAfterToolModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests.lock().expect("requests lock").push(request);
        match self.completions.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            ))),
            1 => {
                self.entered.notify_one();
                std::future::pending().await
            }
            _ => Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "done"}),
            ))),
        }
    }
}

fn assert_replayed_stopped_turn(replayed: &[ModelMessage], note: &ModelMessage) {
    assert_eq!(replayed.len(), 5);
    assert_eq!(replayed[0], ModelMessage::User("first".to_string()));
    assert!(matches!(
        &replayed[1],
        ModelMessage::AssistantToolCall(call) if call.id() == "call_read"
    ));
    assert!(matches!(
        &replayed[2],
        ModelMessage::ToolResult { call_id, .. } if call_id == "call_read"
    ));
    assert_eq!(&replayed[3], note);
    assert_eq!(replayed[4], ModelMessage::User("second".to_string()));
}

#[tokio::test]
async fn failed_turn_replays_finished_tool_exchanges_with_a_failure_note() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let recorded = Arc::clone(&requests);
    let completions = AtomicUsize::new(0);
    let mut model = model();
    model.expect_complete().times(3).returning(move |request| {
        recorded.lock().expect("requests lock").push(request);
        match completions.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            ))),
            1 => Err(ModelError::InvalidResponse),
            _ => Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "done"}),
            ))),
        }
    });
    let directory = tempdir().expect("temporary directory");
    let harness =
        read_harness(model, readable_file_system()).database(directory.path().join("history.db"));
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let failed = session.send("first").await;
    let next = session.send("second").await.expect("next turn");

    // Assert
    assert!(matches!(
        failed,
        Err(SessionError::Turn(TurnError::Model(
            ModelError::InvalidResponse
        )))
    ));
    assert_eq!(next.report().history().replayed_turns(), 1);
    let error_type = format!(
        "{:?}",
        TurnError::Model(ModelError::InvalidResponse).error_type()
    );
    let requests = requests.lock().expect("requests lock");
    assert_replayed_stopped_turn(
        requests[2].messages(),
        &StoppedTurn::Failed.note(&error_type),
    );
}

#[tokio::test]
async fn cancelled_turn_replays_finished_tool_exchanges_with_an_interruption_note() {
    // Arrange
    let entered = Arc::new(Notify::new());
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let directory = tempdir().expect("temporary directory");
    let harness = read_harness(
        CancelledAfterToolModel {
            completions: AtomicUsize::new(0),
            entered: Arc::clone(&entered),
            requests: Arc::clone(&requests),
        },
        readable_file_system(),
    )
    .database(directory.path().join("history.db"));
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let turn = session.turn("first").start();
    let control = turn.control();
    let (cancelled, ()) = tokio::join!(turn, async {
        entered.notified().await;
        control.cancel();
    });
    control.settled().await.expect("cancelled turn settles");
    session.send("second").await.expect("next turn");

    // Assert
    assert!(matches!(
        cancelled,
        Err(SessionError::Turn(TurnError::Cancelled))
    ));
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 3);
    assert_replayed_stopped_turn(
        requests[2].messages(),
        &StoppedTurn::Interrupted.note("cancelled"),
    );
}

#[tokio::test]
async fn batch_stopped_by_a_later_call_replays_the_calls_it_finished() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let recorded = Arc::clone(&requests);
    let completions = AtomicUsize::new(0);
    let mut model = model();
    model.expect_complete().times(2).returning(move |request| {
        recorded.lock().expect("requests lock").push(request);
        match completions.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                read_call("call_read"),
                write_call("call_write", "patch"),
            ]))),
            _ => Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "done"}),
            ))),
        }
    });
    let directory = tempdir().expect("temporary directory");
    let harness =
        read_harness(model, readable_file_system()).database(directory.path().join("history.db"));
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let failed = session.send("first").await;
    session.send("second").await.expect("next turn");

    // Assert
    assert!(matches!(
        failed,
        Err(SessionError::Turn(TurnError::ToolDenied { .. }))
    ));
    let requests = requests.lock().expect("requests lock");
    assert_replayed_stopped_turn(
        requests[1].messages(),
        &StoppedTurn::Failed.note(&format!("{:?}", TurnErrorType::ToolDenied)),
    );
}

#[tokio::test]
async fn completed_turn_replaces_recorded_progress_without_duplication() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let recorded = Arc::clone(&requests);
    let mut model = model();
    model.expect_complete().times(3).returning(move |request| {
        let response = if request.messages().last() == Some(&ModelMessage::User("first".into())) {
            ModelResponse::ToolCall(read_call("call_read"))
        } else {
            ModelResponse::Output(json!({"summary": "done"}))
        };
        recorded.lock().expect("requests lock").push(request);

        Ok(response_without_metadata(response))
    });
    let directory = tempdir().expect("temporary directory");
    let harness =
        read_harness(model, readable_file_system()).database(directory.path().join("history.db"));
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    session.send("first").await.expect("tool turn");
    session.send("second").await.expect("next turn");

    // Assert
    let requests = requests.lock().expect("requests lock");
    let replayed = requests[2].messages();
    assert_eq!(replayed.len(), 5);
    assert_eq!(replayed[0], ModelMessage::User("first".to_string()));
    assert!(matches!(&replayed[1], ModelMessage::AssistantToolCall(_)));
    assert!(matches!(&replayed[2], ModelMessage::ToolResult { .. }));
    assert_eq!(
        replayed[3],
        ModelMessage::Assistant(json!({"summary": "done"}).to_string())
    );
    assert_eq!(replayed[4], ModelMessage::User("second".to_string()));
}

#[tokio::test]
async fn progress_persistence_failure_fails_the_turn_and_replays_its_input() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    let store = Arc::new(GatedStore::new(database, PauseAt::Renewal));
    store.fail_progress.store(true, Ordering::SeqCst);
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let recorded = Arc::clone(&requests);
    let mut model = model();
    model.expect_complete().times(2).returning(move |request| {
        let response = if request.messages().last() == Some(&ModelMessage::User("first".into())) {
            ModelResponse::ToolCall(read_call("call_read"))
        } else {
            ModelResponse::Output(json!({"summary": "done"}))
        };
        recorded.lock().expect("requests lock").push(request);

        Ok(response_without_metadata(response))
    });
    let harness = read_harness(model, readable_file_system()).store(store);
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let failed = session.send("first").await;
    session.send("second").await.expect("next turn");

    // Assert
    assert!(matches!(
        &failed,
        Err(SessionError::Turn(error @ TurnError::Progress { .. }))
            if error.error_type() == TurnErrorType::Session
    ));
    let requests = requests.lock().expect("requests lock");
    assert_eq!(
        requests[1].messages(),
        [
            ModelMessage::User("first".to_string()),
            StoppedTurn::Failed.note("Session"),
            ModelMessage::User("second".to_string()),
        ]
    );
}

/// Scripts `first` to receive `responses` before its next model request
/// fails with `error`, then answers every later request.
fn failing_model(
    responses: Vec<ModelResponse>,
    error: fn() -> ModelError,
    requests: &Arc<Mutex<Vec<ModelRequest>>>,
) -> MockModel {
    let recorded = Arc::clone(requests);
    let completions = AtomicUsize::new(0);
    let mut model = model();
    model
        .expect_complete()
        .times(responses.len() + 2)
        .returning(move |request| {
            recorded.lock().expect("requests lock").push(request);
            let index = completions.fetch_add(1, Ordering::SeqCst);
            match responses.get(index) {
                Some(response) => Ok(response_without_metadata(response.clone())),
                None if index == responses.len() => Err(error()),
                None => Ok(response_without_metadata(ModelResponse::Output(
                    json!({"summary": "done"}),
                ))),
            }
        });

    model
}

fn rejected_request(status: u16) -> ModelError {
    ModelError::classified_request(
        ModelErrorType::Provider,
        Some(status),
        io::Error::other("provider rejected the request").into(),
    )
}

/// Sends a failing `first` turn and a `second` turn that replays it.
async fn send_after_failure(harness: Harness) -> Result<(), SessionError> {
    let mut session = harness
        .session("session", object_schema())
        .create()
        .await
        .expect("session");
    let failed = session.send("first").await.map(|_| ());
    session.send("second").await.expect("next turn");

    failed
}

async fn stores() -> [Arc<dyn SessionStore>; 2] {
    [
        Arc::new(Database::open_in_memory().await.expect("database")),
        Arc::new(MemoryStore::new()),
    ]
}

fn is_omitted_result(message: &ModelMessage, expected_call_id: &str) -> bool {
    matches!(
        message,
        ModelMessage::ToolResult { call_id, content, .. }
            if call_id == expected_call_id && content.starts_with("[Result omitted:")
    )
}

#[tokio::test]
async fn rejected_tool_result_is_omitted_from_replay_with_its_http_status() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
        let model = failing_model(
            vec![ModelResponse::ToolCall(read_call("call_read"))],
            || rejected_request(400),
            &requests,
        );
        let harness = read_harness(model, readable_file_system()).store(store);

        // Act
        let failed = send_after_failure(harness).await;

        // Assert
        assert!(matches!(
            &failed,
            Err(SessionError::Turn(TurnError::Model(error))) if error.http_status() == Some(400)
        ));
        let requests = requests.lock().expect("requests lock");
        assert!(matches!(
            &requests[1].messages()[2],
            ModelMessage::ToolResult { call_id, content, .. }
                if call_id == "call_read" && !content.starts_with("[Result omitted:")
        ));
        let replayed = requests[2].messages();
        assert_eq!(replayed.len(), 5);
        assert_eq!(replayed[0], ModelMessage::User("first".to_string()));
        assert!(matches!(
            &replayed[1],
            ModelMessage::AssistantToolCall(call) if call.id() == "call_read"
        ));
        assert!(is_omitted_result(&replayed[2], "call_read"));
        assert_eq!(
            replayed[3],
            StoppedTurn::Failed.note("Model(Provider), HTTP 400")
        );
        assert_eq!(replayed[4], ModelMessage::User("second".to_string()));
    }
}

#[tokio::test]
async fn rejected_first_request_omits_the_turn_input() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
        let model = failing_model(Vec::new(), || rejected_request(413), &requests);
        let harness = read_harness(model, MockFileSystem::new()).store(store);

        // Act
        let failed = send_after_failure(harness).await;

        // Assert
        assert!(failed.is_err());
        let requests = requests.lock().expect("requests lock");
        assert_eq!(
            requests[0].messages().last(),
            Some(&ModelMessage::User("first".to_string()))
        );
        let replayed = requests[1].messages();
        assert_eq!(replayed.len(), 3);
        assert!(matches!(
            &replayed[0],
            ModelMessage::User(text) if text.starts_with("[Input omitted:")
        ));
        assert_eq!(
            replayed[1],
            StoppedTurn::Failed.note("Model(Provider), HTTP 413")
        );
        assert_eq!(replayed[2], ModelMessage::User("second".to_string()));
    }
}

#[tokio::test]
async fn rejected_batch_omits_every_result_of_its_last_response() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
        let model = failing_model(
            vec![ModelResponse::ToolCalls(vec![
                inspection_call("call_file", json!({})),
                inspection_call("call_search", json!({ "action": "search" })),
            ])],
            || rejected_request(422),
            &requests,
        );
        let harness = read_harness(model, MockFileSystem::new()).store(store);

        // Act
        let failed = send_after_failure(harness).await;

        // Assert
        assert!(failed.is_err());
        let requests = requests.lock().expect("requests lock");
        let replayed = requests[2].messages();
        assert_eq!(replayed.len(), 7);
        assert_eq!(replayed[0], ModelMessage::User("first".to_string()));
        assert!(matches!(&replayed[1], ModelMessage::AssistantToolCall(_)));
        assert!(is_omitted_result(&replayed[2], "call_file"));
        assert!(matches!(&replayed[3], ModelMessage::AssistantToolCall(_)));
        assert!(is_omitted_result(&replayed[4], "call_search"));
        assert_eq!(
            replayed[5],
            StoppedTurn::Failed.note("Model(Provider), HTTP 422")
        );
    }
}

#[tokio::test]
async fn stopped_batch_replays_its_reasoning_on_every_split_call() {
    for store in stores().await {
        // Arrange
        let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
        let reasoned_call = ToolCall::from_json(
            "call_file".to_string(),
            "read",
            "{}",
            Some("plan".to_string()),
        )
        .expect("reasoned call");
        let model = failing_model(
            vec![ModelResponse::ToolCalls(vec![
                reasoned_call,
                inspection_call("call_search", json!({ "action": "search" })),
            ])],
            || ModelError::InvalidResponse,
            &requests,
        );
        let harness = read_harness(model, MockFileSystem::new()).store(store);

        // Act
        let failed = send_after_failure(harness).await;

        // Assert
        assert!(failed.is_err());
        let requests = requests.lock().expect("requests lock");
        let replayed = requests[2].messages();
        assert_eq!(replayed.len(), 7);
        for (position, call_id) in [(1, "call_file"), (3, "call_search")] {
            assert!(matches!(
                &replayed[position],
                ModelMessage::AssistantToolCall(call)
                    if call.id() == call_id && call.reasoning_content() == Some("plan")
            ));
        }
    }
}

#[tokio::test]
async fn unrejected_provider_failure_replays_tool_results_with_its_http_status() {
    // Arrange
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let model = failing_model(
        vec![ModelResponse::ToolCall(read_call("call_read"))],
        || rejected_request(503),
        &requests,
    );
    let directory = tempdir().expect("temporary directory");
    let harness =
        read_harness(model, readable_file_system()).database(directory.path().join("history.db"));

    // Act
    let failed = send_after_failure(harness).await;

    // Assert
    assert!(failed.is_err());
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests[2].messages()[2], requests[1].messages()[2]);
    assert_replayed_stopped_turn(
        requests[2].messages(),
        &StoppedTurn::Failed.note("Model(Provider), HTTP 503"),
    );
}

#[tokio::test]
async fn failed_omission_fails_the_turn_and_replays_its_input() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    let store = Arc::new(GatedStore::new(database, PauseAt::Renewal));
    store.fail_progress.store(true, Ordering::SeqCst);
    let requests = Arc::new(Mutex::new(Vec::<ModelRequest>::new()));
    let model = failing_model(Vec::new(), || rejected_request(400), &requests);
    let harness = read_harness(model, MockFileSystem::new()).store(store);

    // Act
    let failed = send_after_failure(harness).await;

    // Assert
    assert!(matches!(
        failed,
        Err(SessionError::Turn(TurnError::Progress { .. }))
    ));
    let requests = requests.lock().expect("requests lock");
    assert_eq!(
        requests[1].messages(),
        [
            ModelMessage::User("first".to_string()),
            StoppedTurn::Failed.note("Session"),
            ModelMessage::User("second".to_string()),
        ]
    );
}
