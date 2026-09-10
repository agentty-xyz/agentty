use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mockall::Sequence;
use serde_json::{Value, json};
use tokio::io::AsyncRead;
use tokio::sync::Notify;

use crate::file_system::{FileSystem, MockFileSystem};
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleId, ModelResponseType};
use crate::model::{
    CompletionMetadata, CompletionUsage, MockModel, Model, ModelCompletion, ModelError,
    ModelMessage, ModelRequest, ModelResponse,
};
use crate::repository::Repository;
use crate::schema_contract::OutputSchema;
use crate::session::{Database, SessionError};
use crate::tool::{ReadArguments, Tool, ToolCall, WriteArguments};
use crate::turn::TurnOutcome;

pub(super) fn model() -> MockModel {
    let mut model = MockModel::new();
    model.expect_metadata().return_const(None);

    model
}

pub(super) fn resume_fallback_model() -> MockModel {
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
        .returning(|_| Err(ModelError::ResumeUnavailable));
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
                        ModelMessage::User("second".to_string()),
                    ]
        })
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "second"
            })))
            .with_provider_session_id("replacement-session"))
        });
    model
        .expect_complete()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|request| request.provider_session_id() == Some("replacement-session"))
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "third"
            }))))
        });

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
    Harness::new(model)
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(file_system)
}

pub(super) fn write_harness(model: impl Model + 'static, file_system: MockFileSystem) -> Harness {
    Harness::new(model)
        .repository(Repository::fixture("repo"))
        .allow(Tool::Write)
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

pub(super) struct SlowModel;

#[async_trait]
impl Model for SlowModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "done"
        }))))
    }
}

pub(super) struct ContinuationInterruptionModel {
    pub(super) call_count: AtomicUsize,
    pub(super) dropped: Arc<Notify>,
    pub(super) started: Arc<Notify>,
}

#[async_trait]
impl Model for ContinuationInterruptionModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        match self.call_count.fetch_add(1, Ordering::SeqCst) {
            0 => {
                assert!(request.provider_session_id().is_none());

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "first"
                })))
                .with_provider_session_id("native-session"))
            }
            1 => {
                assert_eq!(request.provider_session_id(), Some("native-session"));
                let _drop_notifier = RequestDropNotifier {
                    dropped: Arc::clone(&self.dropped),
                };
                self.started.notify_one();
                std::future::pending().await
            }
            _ => {
                assert!(request.provider_session_id().is_none());
                assert_eq!(
                    request.messages(),
                    [
                        ModelMessage::User("first".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"first"}"#.to_string()),
                        ModelMessage::User("retry".to_string()),
                    ]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                })))
                .with_provider_session_id("replacement-session"))
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

pub(super) struct LeaseExpiryModel {
    pub(super) call_count: AtomicUsize,
    pub(super) release_first: Arc<Notify>,
    pub(super) started_first: Arc<Notify>,
}

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

pub(super) struct RequestDropNotifier {
    pub(super) dropped: Arc<Notify>,
}

impl Drop for RequestDropNotifier {
    fn drop(&mut self) {
        self.dropped.notify_one();
    }
}

pub(super) struct LeaseOwnershipModel {
    pub(super) call_count: AtomicUsize,
    pub(super) dropped_first: Arc<Notify>,
    pub(super) started_first: Arc<Notify>,
}

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

pub(super) async fn stored_turn_state(database: &Database) -> (String, Option<String>) {
    sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT status, error_type FROM session_turn ORDER BY turn_position DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored turn state should load")
}

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

pub(super) async fn stored_lease_expiry(database: &Database) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT lease_expires_at FROM session_turn WHERE session_id = ? AND status = 'running'",
    )
    .bind("session-a")
    .fetch_one(database.pool())
    .await
    .expect("active lease should load")
}

pub(super) fn elapsed_timestamp(origin: i64, started_at: tokio::time::Instant) -> i64 {
    let elapsed_seconds = i64::try_from(started_at.elapsed().as_secs()).unwrap_or(i64::MAX);

    origin.saturating_add(elapsed_seconds)
}

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
