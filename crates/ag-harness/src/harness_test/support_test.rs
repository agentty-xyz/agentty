//! Shared harness test fixtures: mock models and filesystems, harness and
//! tool call builders, stored turn state helpers, and lifecycle assertions.

use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
#[cfg(feature = "sqlite")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "sqlite")]
use std::time::Instant;

use async_trait::async_trait;
use mockall::Sequence;
use serde_json::{Value, json};
use tokio::io::AsyncRead;
use tokio::sync::Notify;

use crate::context_budget_fixture::unbounded_context_budget;
use crate::file_system::{FileSystem, MockFileSystem};
use crate::harness::Harness;
use crate::lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, ModelResponseType,
};
use crate::model::{
    CompletionMetadata, CompletionUsage, MockModel, Model, ModelCompletion, ModelResponse,
};
#[cfg(feature = "sqlite")]
use crate::model::{ModelError, ModelMessage, ModelRequest};
use crate::repository::Repository;
use crate::schema_contract::OutputSchema;
#[cfg(feature = "sqlite")]
use crate::session::SessionError;
#[cfg(feature = "sqlite")]
use crate::sqlite_store::Database;
use crate::tool::{ReadArguments, ToolCall, ToolSet, WriteArguments};
#[cfg(feature = "sqlite")]
use crate::turn::TurnOutcome;

pub(super) fn model() -> MockModel {
    let mut model = MockModel::new();
    model.expect_metadata().return_const(None);
    model.expect_validate_input().returning(|_| Ok(()));

    model
}

pub(super) fn object_schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("schema should be valid")
}

pub(super) fn read_harness(model: impl Model + 'static, file_system: MockFileSystem) -> Harness {
    Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .tools(ToolSet::empty().with_read())
        .file_system(file_system)
}

pub(super) fn write_harness(model: impl Model + 'static, file_system: MockFileSystem) -> Harness {
    Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .tools(ToolSet::empty().with_write())
        .file_system(file_system)
}

pub(super) fn read_call(id: &str) -> ToolCall {
    read_call_with_path(id, "Cargo.toml")
}

pub(super) fn read_call_with_path(id: &str, path: &str) -> ToolCall {
    let arguments = serde_json::from_value::<ReadArguments>(json!({
        "action": "file",
        "path": path,
        "limit": 1
    }))
    .expect("read arguments should be valid");

    ToolCall::read(id.to_string(), arguments, None)
}

pub(super) fn inspection_call(id: &str, arguments: Value) -> ToolCall {
    let arguments = serde_json::from_value::<ReadArguments>(arguments)
        .expect("inspection arguments should be valid");

    ToolCall::read(id.to_string(), arguments, None)
}

pub(super) fn response_without_metadata(response: ModelResponse) -> ModelCompletion {
    ModelCompletion::from_response(response)
}

pub(super) fn response_with_metadata(response: ModelResponse) -> ModelCompletion {
    ModelCompletion::new(
        CompletionMetadata::new(
            "stop\nforged".to_string(),
            Some("response\u{1b}-1".to_string()),
            Some("reported\nmodel".to_string()),
            Some("finger\tprint".to_string()),
            Some(CompletionUsage::new(
                None,
                None,
                Some(12),
                Some(4),
                None,
                Some(16),
            )),
        ),
        response,
    )
}

#[cfg(feature = "sqlite")]
pub(super) struct SlowModel;

#[cfg(feature = "sqlite")]
#[async_trait]
impl Model for SlowModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    }
}

#[cfg(feature = "sqlite")]
pub(super) struct InterruptionModel {
    pub(super) call_count: AtomicUsize,
    pub(super) dropped: Arc<Notify>,
    pub(super) started: Arc<Notify>,
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl Model for InterruptionModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        match self.call_count.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "first"
            })))),
            1 => {
                let _drop_notifier = RequestDropNotifier {
                    dropped: Arc::clone(&self.dropped),
                };
                self.started.notify_one();
                std::future::pending().await
            }
            _ => {
                // The interrupted turn replays its input and stop note.
                let messages = request.messages();
                assert_eq!(messages.len(), 5);
                assert_eq!(
                    messages[..2],
                    [
                        ModelMessage::User("first".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"first"}"#.to_string()),
                    ]
                );
                assert!(matches!(
                    &messages[3],
                    ModelMessage::User(note)
                        if note.starts_with("[harness] The turn above was interrupted")
                ));
                assert_eq!(messages[4], ModelMessage::User("retry".to_string()));

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            }
        }
    }
}

pub(super) struct PendingToolFileSystem {
    pub(super) started: Arc<Notify>,
}

#[async_trait]
impl FileSystem for PendingToolFileSystem {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        if path == Path::new("repo") {
            Ok(PathBuf::from("/repo"))
        } else {
            Ok(PathBuf::from("/repo/Cargo.toml"))
        }
    }

    async fn open_beneath(
        &self,
        _root: &Path,
        _path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        self.started.notify_one();
        std::future::pending().await
    }

    async fn replace_beneath(
        &self,
        _root: &Path,
        _path: &Path,
        _expected: Option<Vec<u8>>,
        _content: Vec<u8>,
    ) -> io::Result<()> {
        Err(io::Error::other(
            "pending read fixture must not replace files",
        ))
    }
}

#[cfg(feature = "sqlite")]
pub(super) struct LeaseExpiryModel {
    pub(super) call_count: AtomicUsize,
    pub(super) release_first: Arc<Notify>,
    pub(super) started_first: Arc<Notify>,
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl Model for LeaseExpiryModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            self.started_first.notify_one();
            self.release_first.notified().await;
        }

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    }
}

#[cfg(feature = "sqlite")]
pub(super) struct RequestDropNotifier {
    pub(super) dropped: Arc<Notify>,
}

#[cfg(feature = "sqlite")]
impl Drop for RequestDropNotifier {
    fn drop(&mut self) {
        self.dropped.notify_one();
    }
}

#[cfg(feature = "sqlite")]
pub(super) struct LeaseOwnershipModel {
    pub(super) call_count: AtomicUsize,
    pub(super) dropped_first: Arc<Notify>,
    pub(super) started_first: Arc<Notify>,
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl Model for LeaseOwnershipModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            let _drop_notifier = RequestDropNotifier {
                dropped: Arc::clone(&self.dropped_first),
            };
            self.started_first.notify_one();
            std::future::pending::<()>().await;
        }

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    }
}

#[cfg(feature = "sqlite")]
pub(super) async fn send_with_resumed_session(
    harness: Arc<Harness>,
    prompt: &'static str,
) -> Result<TurnOutcome, SessionError> {
    let mut session = harness
        .resume("session-a")
        .await
        .expect("session should resume");

    session.send(prompt).await
}

pub(super) async fn wait_for_fixture(notify: &Notify, description: &str) {
    let result = tokio::time::timeout(Duration::from_secs(5), notify.notified()).await;

    assert!(result.is_ok(), "timed out waiting for {description}");
}

#[cfg(feature = "sqlite")]
pub(super) async fn stored_turn_state(database: &Database) -> (String, Option<String>) {
    sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, error_type FROM session_turn ORDER BY turn_position DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored turn state should load")
}

#[cfg(feature = "sqlite")]
pub(super) async fn wait_for_stored_turn_state(
    database: &Database,
    expected: &(String, Option<String>),
) -> (String, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        let state = stored_turn_state(database).await;
        if &state == expected || Instant::now() >= deadline {
            return state;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(feature = "sqlite")]
pub(super) async fn stored_lease_expiry(database: &Database) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT lease_expires_at FROM session_turn WHERE session_id = ? AND status = 'running'",
    )
    .bind("session-a")
    .fetch_one(database.pool())
    .await
    .expect("active lease should load")
}

#[cfg(feature = "sqlite")]
pub(super) fn elapsed_timestamp(origin: i64, started_at: tokio::time::Instant) -> i64 {
    let elapsed_seconds = i64::try_from(started_at.elapsed().as_secs()).unwrap_or(i64::MAX);

    origin.saturating_add(elapsed_seconds)
}

#[cfg(feature = "sqlite")]
pub(super) async fn wait_for_lease_extension(
    database: &Database,
    previous_expiry: i64,
) -> Option<i64> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let lease_expiry = stored_lease_expiry(database).await;
            if lease_expiry > previous_expiry {
                return lease_expiry;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .ok()
}

pub(super) fn write_call(id: &str, patch: &str) -> ToolCall {
    let arguments = serde_json::from_value::<WriteArguments>(json!({
        "path": "src/lib.rs",
        "patch": patch
    }))
    .expect("write arguments should be valid");

    ToolCall::write(id.to_string(), arguments, None)
}

pub(super) fn readable_file_system() -> MockFileSystem {
    readable_file_system_with(b"[workspace]\nmember = true\n".to_vec())
}

pub(super) fn readable_file_system_with(content: Vec<u8>) -> MockFileSystem {
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo/Cargo.toml")));
    file_system
        .expect_open_beneath()
        .times(1)
        .return_once(move |_, _| Ok(Box::new(Cursor::new(content))));

    file_system
}

/// Records every lifecycle event.
#[derive(Clone, Default)]
pub(super) struct LifecycleRecorder {
    pub(super) events: Arc<Mutex<Vec<LifecycleEvent>>>,
}

impl LifecycleObserver for LifecycleRecorder {
    fn observe(&self, event: LifecycleEvent) {
        self.events.lock().expect("events lock").push(event);
    }
}

pub(super) fn turn_started_id(event: &LifecycleEvent) -> Option<LifecycleId> {
    match event.kind() {
        LifecycleEventKind::TurnStarted { turn_id } => Some(*turn_id),
        _ => None,
    }
}

pub(super) fn model_started_id(event: &LifecycleEvent) -> Option<LifecycleId> {
    match event.kind() {
        LifecycleEventKind::ModelRequestStarted { model_call_id, .. } => Some(*model_call_id),
        _ => None,
    }
}

pub(super) fn tool_requested_id(event: &LifecycleEvent) -> Option<LifecycleId> {
    match event.kind() {
        LifecycleEventKind::ToolRequested { tool_call_id, .. } => Some(*tool_call_id),
        _ => None,
    }
}

pub(super) fn assert_read_tool_lifecycle(events: &[LifecycleEvent]) {
    let turn_id = turn_started_id(&events[0]).expect("first event should start the turn");
    let first_model_call_id =
        model_started_id(&events[1]).expect("second event should start the model request");
    assert!(matches!(
        events[1].kind(),
        LifecycleEventKind::ModelRequestStarted {
            model: None,
            request_index: 0,
            turn_id: Some(event_turn_id),
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[2].kind(),
        LifecycleEventKind::ModelRequestCompleted {
            completion: None,
            model_call_id,
            response_type: ModelResponseType::ToolCall,
            turn_id: Some(event_turn_id),
            ..
        } if *model_call_id == first_model_call_id && *event_turn_id == turn_id
    ));
    let tool_call_id = tool_requested_id(&events[3]).expect("fourth event should request the tool");
    assert!(matches!(
        events[3].kind(),
        LifecycleEventKind::ToolRequested {
            provider_call_id,
            tool_name,
            turn_id: event_turn_id,
            ..
        } if provider_call_id == "provider-call-id"
            && tool_name == "read"
            && *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[4].kind(),
        LifecycleEventKind::ToolStarted {
            tool_call_id: event_tool_call_id,
            turn_id: event_turn_id,
        } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[5].kind(),
        LifecycleEventKind::ToolCompleted {
            tool_call_id: event_tool_call_id,
            turn_id: event_turn_id,
            ..
        } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[6].kind(),
        LifecycleEventKind::ModelRequestStarted {
            request_index: 1,
            turn_id: Some(event_turn_id),
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[7].kind(),
        LifecycleEventKind::ModelRequestCompleted {
            completion: None,
            response_type: ModelResponseType::Output,
            turn_id: Some(event_turn_id),
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(matches!(
        events[8].kind(),
        LifecycleEventKind::TurnCompleted {
            turn_id: event_turn_id,
            ..
        } if *event_turn_id == turn_id
    ));
    assert!(turn_started_id(&events[1]).is_none());
    assert!(model_started_id(&events[0]).is_none());
    assert!(tool_requested_id(&events[0]).is_none());
}
