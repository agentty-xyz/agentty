//! Host request conformance exercised externally and in source coverage.

use std::future::pending;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::{
    ExecutionIdentity, Harness, HostRequest, HostTurnAcquisition, HostTurnStatus, LifecycleEvent,
    LifecycleEventKind, LifecycleObserver, Model, ModelCompletion, ModelError, ModelRequest,
    ModelResponse, NewSession, SessionError, SessionStore, SqliteStore, Tool, ToolPolicy,
    TurnError, TurnInput, TurnLimits, TurnOptions, WriteStatus,
};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::timeout;

use crate::store_conformance_test::{options, schema, stores};

#[derive(Clone, Default)]
struct EventRecorder(Arc<Mutex<Vec<LifecycleEvent>>>);

impl EventRecorder {
    fn events(&self) -> Vec<LifecycleEvent> {
        self.0.lock().expect("events").clone()
    }
}

impl LifecycleObserver for EventRecorder {
    fn observe(&self, event: LifecycleEvent) {
        self.0.lock().expect("events").push(event);
    }
}

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    entered: Notify,
}

struct TestModel(Arc<Probe>);

#[async_trait]
impl Model for TestModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        self.0.entered.notify_one();
        match request.prompt() {
            "write" if !matches!(request.messages().last(), Some(ag_harness::ModelMessage::ToolResult { .. })) => {
                Ok(ModelCompletion::from_response(ModelResponse::ToolCall(ag_harness::ToolCall::from_json(
                    "write-call".into(), "write", &json!({"path":"file.txt","patch":"--- /dev/null\n+++ b/file.txt\n@@ -0,0 +1 @@\n+new\n"}).to_string(), None,
                )?)))
            }
            "wait" => pending().await,
            "fail" => Err(ModelError::InvalidResponse),
            _ => Ok(ModelCompletion::from_response(ModelResponse::Output(
                json!({"answer": request.prompt()}),
            ))),
        }
    }
}

fn harness(store: Arc<dyn SessionStore>, probe: &Arc<Probe>) -> Harness {
    Harness::new(TestModel(Arc::clone(probe)))
        .execution_identity(
            ExecutionIdentity::new("test-model-and-filesystem", "1").expect("identity"),
        )
        .store(store)
}

#[tokio::test]
async fn completed_and_failed_retries_never_execute_again() {
    // Arrange
    for store in stores().await {
        let probe = Arc::new(Probe::default());
        let events = EventRecorder::default();
        let harness = harness(store, &probe).with_lifecycle_observer(events.clone());
        let mut session = harness
            .session("session", schema())
            .create()
            .await
            .expect("session");

        // Act
        let original = session
            .submit("first", "hello", options())
            .await
            .expect("first");
        let completed_events = events.events();
        let duplicate = session
            .submit("first", "hello", options())
            .await
            .expect("duplicate");
        let controlled_duplicate = session
            .submit_controlled("first", "hello", options())
            .expect("controlled retry")
            .await
            .expect("recorded completion");
        let conflict = session.submit("first", "different", options()).await;
        assert_eq!(events.events(), completed_events);
        let failed = session.submit("failed", "fail", options()).await;
        let failed_events = events.events();
        let failed_retry = session.submit("failed", "fail", options()).await;

        // Assert
        assert_eq!(original, duplicate);
        assert_eq!(original, controlled_duplicate);
        assert_eq!(completed_events.len(), 4);
        assert!(matches!(
            completed_events[3].kind(),
            LifecycleEventKind::TurnCompleted { .. }
        ));
        assert_eq!(failed_events.len(), 8);
        assert!(matches!(
            failed_events[7].kind(),
            LifecycleEventKind::TurnFailed { .. }
        ));
        assert_eq!(events.events(), failed_events);
        assert!(matches!(conflict, Err(SessionError::HostTurnConflict)));
        assert!(matches!(failed, Err(SessionError::Turn(_))));
        let Err(SessionError::HostTurnStopped(record)) = failed_retry else {
            std::panic::resume_unwind(Box::new("stopped request"))
        };
        assert!(matches!(record.status, HostTurnStatus::Failed { .. }));
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
        assert!(session.recover("missing").await.expect("lookup").is_none());
        assert!(session.recover("").await.is_err());
        assert!(session.submit("", "hello", options()).await.is_err());
        assert_eq!(events.events(), failed_events);
        session
            .submit("new-attempt", "hello", options())
            .await
            .expect("new ID");
        assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
        assert_eq!(events.events().len(), 12);
    }
}

#[tokio::test]
async fn active_retries_classify_before_local_busy_and_cancelled_retries_stay_stopped() {
    // Arrange
    for store in stores().await {
        let probe = Arc::new(Probe::default());
        let events = EventRecorder::default();
        let harness = harness(store, &probe).with_lifecycle_observer(events.clone());
        let mut first = harness
            .session("session", schema())
            .create()
            .await
            .expect("session");
        let mut retry = harness.resume("session").await.expect("resume");
        let controlled = first
            .submit_controlled("id", "wait", options())
            .expect("controlled");
        let control = controlled.control();

        // Act
        let wait = async {
            probe.entered.notified().await;
            let active_events = events.events();
            assert_eq!(active_events.len(), 2);
            assert!(matches!(
                retry.submit("id", "wait", options()).await,
                Err(SessionError::HostTurnInProgress(_))
            ));
            assert!(matches!(
                retry.submit("id", "changed", options()).await,
                Err(SessionError::HostTurnConflict)
            ));
            assert!(matches!(
                retry.submit("other", "hello", options()).await,
                Err(SessionError::Busy { .. })
            ));
            assert_eq!(events.events(), active_events);
            control.cancel();
        };
        let (result, ()) = timeout(Duration::from_secs(5), async {
            tokio::join!(controlled, wait)
        })
        .await
        .expect("bounded cancellation");
        control.settled().await.expect("settled");

        // Assert
        assert!(matches!(
            result,
            Err(SessionError::Turn(TurnError::Cancelled))
        ));
        let cancelled_events = events.events();
        assert_eq!(cancelled_events.len(), 4);
        assert!(matches!(
            cancelled_events[3].kind(),
            LifecycleEventKind::TurnFailed { .. }
        ));
        let Err(SessionError::HostTurnStopped(record)) =
            retry.submit("id", "wait", options()).await
        else {
            std::panic::resume_unwind(Box::new("interrupted"))
        };
        assert!(matches!(record.status, HostTurnStatus::Interrupted { .. }));
        assert_eq!(events.events(), cancelled_events);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        retry
            .submit("new", "hello", options())
            .await
            .expect("successor");
        control.cancel();
        assert!(matches!(
            retry
                .recover("new")
                .await
                .expect("lookup")
                .expect("record")
                .status,
            HostTurnStatus::Completed(_)
        ));
    }
}

#[tokio::test]
async fn backend_duplicate_acquisition_is_atomic_and_recovers_pending_effects() {
    // Arrange
    for store in stores().await {
        let probe = Arc::new(Probe::default());
        let harness = harness(Arc::clone(&store), &probe);
        let mut seed = harness
            .session("seed", schema())
            .create()
            .await
            .expect("seed");
        seed.submit("id", "hello", options())
            .await
            .expect("seed result");
        let request = seed
            .recover("id")
            .await
            .expect("lookup")
            .expect("record")
            .request;
        store
            .create_session(&NewSession::new("race", schema()), None, 1000)
            .await
            .expect("session");
        let options = options();

        // Act
        let input = TurnInput::from("hello");
        let (left, right) = tokio::join!(
            store.begin_request(Arc::clone(&store), "race", &input, &options, &request, 0),
            store.begin_request(Arc::clone(&store), "race", &input, &options, &request, 0)
        );
        let ((HostTurnAcquisition::Acquired(acquired), HostTurnAcquisition::Recorded(duplicate))
        | (HostTurnAcquisition::Recorded(duplicate), HostTurnAcquisition::Acquired(acquired))) =
            (left.expect("left"), right.expect("right"))
        else {
            std::panic::resume_unwind(Box::new("exactly one reservation"));
        };
        let owner = acquired.owner();
        let write_id = store
            .write_intent(owner, "call", Path::new("workspace"), "file", None, b"new")
            .await
            .expect("intent");
        store.interrupt(owner).await.expect("interrupt");
        let recovered = store
            .load_request("race", "id")
            .await
            .expect("lookup")
            .expect("record");
        store
            .finish_write(owner, write_id, true)
            .await
            .expect("late outcome");
        let settled = store
            .load_request("race", "id")
            .await
            .expect("lookup")
            .expect("record");

        // Assert
        assert!(matches!(duplicate.status, HostTurnStatus::InProgress));
        assert!(matches!(
            recovered.status,
            HostTurnStatus::Interrupted { .. }
        ));
        assert_eq!(recovered.writes[0].status, WriteStatus::Pending);
        assert_eq!(settled.writes[0].status, WriteStatus::Applied);
        let mut changed = serde_json::to_value(&request).expect("request encoding");
        changed["fingerprint"] = json!("changed");
        let changed: HostRequest = serde_json::from_value(changed).expect("request");
        assert!(matches!(
            store
                .begin_request(
                    Arc::clone(&store),
                    "race",
                    &TurnInput::from("changed"),
                    &options,
                    &changed,
                    0
                )
                .await,
            Err(SessionError::HostTurnConflict)
        ));
        assert!(matches!(
            store.load_request("missing", "id").await,
            Err(SessionError::NotFound { .. })
        ));
    }
}

#[tokio::test]
async fn sqlite_reopen_recovers_original_outcomes_after_history_eviction() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("session.sqlite");
    let probe = Arc::new(Probe::default());
    let store = Arc::new(SqliteStore::open(&path).await.expect("sqlite"));
    let harness = harness(store, &probe).max_history_bytes(std::num::NonZeroUsize::MIN);
    let mut session = harness
        .session("session", schema())
        .create()
        .await
        .expect("session");
    let original = session
        .submit("id", "hello", options())
        .await
        .expect("original");
    session
        .submit("failure", "fail", options())
        .await
        .expect_err("failure");
    drop(session);
    drop(harness);

    // Act
    let store = Arc::new(SqliteStore::open(&path).await.expect("reopen"));
    let reopened = self::harness(store, &probe);
    let mut session = reopened.resume("session").await.expect("resume");
    let duplicate = session
        .submit("id", "hello", options())
        .await
        .expect("recovered");

    // Assert
    assert_eq!(original, duplicate);
    assert!(matches!(
        session.submit("failure", "fail", options()).await,
        Err(SessionError::HostTurnStopped(_))
    ));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn effective_configuration_changes_conflict_and_identity_is_required() {
    // Arrange
    for store in stores().await {
        let probe = Arc::new(Probe::default());
        let harness = harness(Arc::clone(&store), &probe);
        let mut session = harness
            .session("session", schema())
            .system_prompt("policy")
            .create()
            .await
            .expect("session");
        session
            .submit("id", "hello", options())
            .await
            .expect("original");
        let unidentified = Harness::new(TestModel(Arc::clone(&probe))).store(Arc::clone(&store));
        let revised = self::harness(Arc::clone(&store), &probe).execution_identity(
            ExecutionIdentity::new("test-model-and-filesystem", "2").expect("identity"),
        );
        let reasoning =
            self::harness(store, &probe).model_reasoning_effort(ag_harness::ReasoningEffort::High);

        // Act / Assert
        let changed = TurnOptions::new(
            schema(),
            ToolPolicy::default().allow(Tool::Read),
            TurnLimits::default(),
        );
        assert!(matches!(
            session.submit("id", "hello", changed).await,
            Err(SessionError::HostTurnConflict)
        ));
        assert!(matches!(
            revised
                .resume("session")
                .await
                .expect("resume")
                .submit("id", "hello", options())
                .await,
            Err(SessionError::HostTurnConflict)
        ));
        assert!(matches!(
            reasoning
                .resume("session")
                .await
                .expect("resume")
                .submit("id", "hello", options())
                .await,
            Err(SessionError::HostTurnConflict)
        ));
        assert!(matches!(
            unidentified
                .resume("session")
                .await
                .expect("resume")
                .submit("id", "hello", options())
                .await,
            Err(SessionError::ExecutionIdentityRequired)
        ));
        assert!(ExecutionIdentity::new("", "1").is_err());
        assert!(ExecutionIdentity::new("key", " ").is_err());
        assert!(ExecutionIdentity::new("x".repeat(257), "1").is_err());
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn retry_of_completed_write_returns_original_report_without_touching_files() {
    // Arrange
    for store in stores().await {
        let directory = tempfile::tempdir().expect("directory");
        let probe = Arc::new(Probe::default());
        let harness = harness(store, &probe).repository(
            crate::repository_fixture::repository_with_host_git(directory.path()),
        );
        let mut session = harness
            .session("write", schema())
            .create()
            .await
            .expect("session");
        let options = TurnOptions::new(
            schema(),
            ToolPolicy::default().allow(Tool::Write),
            TurnLimits::default(),
        );

        // Act
        let original = session
            .submit("write-id", "write", options.clone())
            .await
            .expect("write");
        std::fs::write(directory.path().join("file.txt"), "host edit").expect("host edit");
        let repeated = session
            .submit("write-id", "write", options)
            .await
            .expect("duplicate");
        let record = session
            .recover("write-id")
            .await
            .expect("lookup")
            .expect("record");

        // Assert
        assert_eq!(original, repeated);
        assert_eq!(original.report().tool_calls().len(), 1);
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
        assert_eq!(record.writes[0].status, WriteStatus::Applied);
        assert_eq!(
            std::fs::read_to_string(directory.path().join("file.txt")).expect("file"),
            "host edit"
        );
    }
}
