//! Reusable lifecycle conformance, compiled as an external consumer and for
//! coverage.

#[path = "store.rs"]
mod backend;

#[path = "store_gate.rs"]
mod gate;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::{
    AcquiredTurn, CommandCleanupScope, CommandIntent, CommandOutcome, CommandTermination, Harness,
    ImageContent, ImageMediaType, InputBlock, MemoryStore, Model, ModelCompletion, ModelError,
    ModelMessage, ModelRequest, ModelResponse, NewSession, OutputSchema, SessionCheckpoint,
    SessionError, SessionStore, SqliteStore, StoreIdentity, StoredTurnOptions, ToolPolicy,
    TurnError, TurnInput, TurnLimits, TurnOptions, TurnOwner, WriteStatus,
};
use async_trait::async_trait;
pub(crate) use backend::ExternalStore;
pub(crate) use gate::Gate;
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::Instant;

pub(crate) fn schema() -> OutputSchema {
    OutputSchema::new(json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false})).expect("schema")
}

pub(crate) fn options() -> TurnOptions {
    TurnOptions::new(schema(), ToolPolicy::default(), TurnLimits::default())
}

pub(crate) fn png_image(payload: &[u8]) -> ImageContent {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(payload);

    ImageContent::new(ImageMediaType::Png, bytes).expect("valid PNG image")
}

pub(crate) fn image_input(before: &str, payload: &[u8], after: &str) -> TurnInput {
    TurnInput::from_blocks(vec![
        InputBlock::Text(before.to_string()),
        InputBlock::Image(png_image(payload)),
        InputBlock::Text(after.to_string()),
    ])
    .expect("image input")
}

pub(crate) async fn stores() -> Vec<Arc<dyn SessionStore>> {
    vec![
        Arc::new(ExternalStore::new()),
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("sqlite"),
        ),
    ]
}

pub(crate) struct Echo {
    pub(crate) requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl Model for Echo {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let answer = request.prompt().to_string();
        self.requests.lock().expect("requests").push(request);

        Ok(
            ModelCompletion::from_response(ModelResponse::Output(json!({"answer":answer})))
                .with_provider_session_id("continuation"),
        )
    }
}

pub(crate) fn harness(store: Arc<dyn SessionStore>) -> Harness {
    Harness::new(Echo {
        requests: Arc::default(),
    })
    .store(store)
}

#[tokio::test]
async fn command_reconciliation_preserves_records_and_validates_session_and_store() {
    // Arrange
    for store in stores().await.into_iter().skip(1) {
        let harness = harness(Arc::clone(&store));
        let session = harness
            .session("commands", schema())
            .create()
            .await
            .expect("session");
        let other_session = harness
            .session("other", schema())
            .create()
            .await
            .expect("other");
        let independent = self::harness(Arc::new(MemoryStore::new()))
            .session("commands", schema())
            .create()
            .await
            .expect("independent");
        let acquired = store
            .begin_turn(
                Arc::clone(&store),
                "commands",
                &TurnInput::from("run"),
                &options(),
                0,
            )
            .await
            .expect("owner");
        let intent = CommandIntent {
            call_id: "call".into(),
            command: "effect".into(),
            policy: json!({}),
            workspace: "/workspace".into(),
        };
        store
            .command_intent(acquired.owner(), &intent)
            .await
            .expect("intent");
        let record = session.commands().await.expect("records").remove(0);

        // Act / Assert
        assert!(
            session.reconcile_command(&record).await.is_err(),
            "live owner"
        );
        assert!(
            other_session.reconcile_command(&record).await.is_err(),
            "other session"
        );
        assert!(
            independent.reconcile_command(&record).await.is_err(),
            "other store"
        );
        store.interrupt(acquired.owner()).await.expect("interrupt");
        session
            .reconcile_command(&record)
            .await
            .expect("reconcile stopped owner");
        let records = session.commands().await.expect("records");
        assert_eq!(records[0].intent, intent);
        assert!(records[0].outcome.is_none());
        assert!(records[0].reconciled);
        assert!(!records[0].blocks_admission());
    }
}

#[tokio::test]
async fn external_store_without_command_support_fails_closed() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    store
        .create_session(&NewSession::new("unsupported", schema()), None, 1024)
        .await
        .expect("create");
    let acquired = store
        .begin_turn(
            Arc::clone(&store),
            "unsupported",
            &TurnInput::from("run"),
            &options(),
            0,
        )
        .await
        .expect("owner");
    let intent = CommandIntent {
        call_id: "call".into(),
        command: "effect".into(),
        policy: json!({}),
        workspace: "/workspace".into(),
    };
    let outcome = CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::PidNamespace,
        execution_failure: None,
        exit_code: Some(0),
        signal: None,
        stdout: String::new(),
        stderr: String::new(),
        termination: CommandTermination::Completed,
        truncated: false,
    };

    // Act / Assert
    assert!(matches!(
        store.load_commands("unsupported").await,
        Err(SessionError::InvalidData { .. })
    ));
    assert!(matches!(
        store.command_intent(acquired.owner(), &intent).await,
        Err(SessionError::InvalidData { .. })
    ));
    assert!(matches!(
        store.finish_command(acquired.owner(), 1, &outcome).await,
        Err(SessionError::InvalidData { .. })
    ));
    assert!(matches!(
        store.reconcile_command(acquired.owner(), 1).await,
        Err(SessionError::InvalidData { .. })
    ));
    store.interrupt(acquired.owner()).await.expect("interrupt");
}

pub(crate) async fn lifecycle(store: Arc<dyn SessionStore>) {
    // Arrange
    let config = NewSession::new("session", schema())
        .with_optional_system_prompt(Some("policy".to_string()));
    store
        .create_session(&config, None, 256)
        .await
        .expect("create");
    let inputs: Vec<TurnInput> = ["first", "busy", "second"]
        .into_iter()
        .map(TurnInput::from)
        .collect();
    let acquired = store
        .begin_turn(Arc::clone(&store), "session", &inputs[0], &options(), 0)
        .await
        .expect("begin")
        .activate()
        .expect("activation remains idempotent");
    let owner = acquired.owner().clone();

    // Act
    assert!(matches!(
        store
            .begin_turn(Arc::clone(&store), "session", &inputs[1], &options(), 0)
            .await,
        Err(SessionError::Busy { .. })
    ));
    let write = store
        .write_intent(
            &owner,
            "call",
            Path::new("repository"),
            "file",
            Some(b"old"),
            b"new",
        )
        .await
        .expect("intent");
    store
        .complete_turn(
            &owner,
            &[ModelMessage::Assistant("answer".to_string())],
            Some("native"),
        )
        .await
        .expect("complete");
    store
        .finish_write(&owner, write, true)
        .await
        .expect("settle after terminal");
    let successor = store
        .begin_turn(Arc::clone(&store), "session", &inputs[2], &options(), 0)
        .await
        .expect("successor");
    store.interrupt(&owner).await.expect("stale interrupt");

    // Assert
    assert!(store.renew(successor.owner()).await.is_ok());
    assert!(matches!(
        store.renew(&owner).await,
        Err(SessionError::OwnershipLost { .. })
    ));
    assert!(
        store
            .finish_write(successor.owner(), write, false)
            .await
            .is_err()
    );
    let records = store.load_writes("session").await.expect("writes");
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(
        records[0].expected_hash.as_deref(),
        Some("cba06b5736faf67e54b07b561eae94395e774c517a7d910a54369e1263ccfbd4")
    );
    assert_eq!(owner.session_id(), "session");
    assert_eq!(owner.store_identity(), store.identity());
    assert_eq!(owner.interruption_error_type(), "cancelled");
    let loaded = store.load_session("session").await.expect("load");
    assert_eq!(loaded.system_prompt.as_deref(), Some("policy"));
    assert_eq!(loaded.turns.len(), 1);
    store
        .fail_turn(
            successor.owner(),
            &TurnError::Model(ModelError::InvalidResponse),
        )
        .await
        .expect("fail");
    assert!(
        store
            .load_session("session")
            .await
            .expect("failed")
            .provider_session_id
            .is_none()
    );
    assert_eq!(
        store
            .load_session("session")
            .await
            .expect("failed history")
            .turns
            .len(),
        1
    );
}

#[tokio::test]
async fn all_backends_satisfy_lifecycle_and_journal_contract() {
    // Arrange / Act / Assert
    for store in stores().await {
        lifecycle(store).await;
    }
}

#[tokio::test]
async fn all_backends_preserve_ordered_image_input_in_history() {
    // Arrange
    for store in stores().await {
        let input = image_input("before", b"payload", "after");
        store
            .create_session(&NewSession::new("images", schema()), None, 4096)
            .await
            .expect("create");
        let acquired = store
            .begin_turn(Arc::clone(&store), "images", &input, &options(), 0)
            .await
            .expect("begin");

        // Act
        store
            .complete_turn(
                acquired.owner(),
                &[ModelMessage::Assistant("described".to_string())],
                None,
            )
            .await
            .expect("complete");
        drop(acquired);

        // Assert
        let loaded = store.load_session("images").await.expect("load");
        assert_eq!(
            loaded.turns,
            vec![vec![
                ModelMessage::UserInput(input),
                ModelMessage::Assistant("described".to_string()),
            ]]
        );
    }
}

#[tokio::test]
async fn all_backends_bound_history_to_the_checkpoint_and_reject_stale_publications() {
    for store in stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("checkpoints", schema()), None, 4096)
            .await
            .expect("create");
        for turn in ["zero", "one"] {
            let acquired = store
                .begin_turn(
                    Arc::clone(&store),
                    "checkpoints",
                    &TurnInput::from(turn),
                    &options(),
                    0,
                )
                .await
                .expect("begin");
            store
                .complete_turn(
                    acquired.owner(),
                    &[ModelMessage::Assistant(format!("{turn}-answer"))],
                    None,
                )
                .await
                .expect("complete");
        }
        let summary = json!({"context": "c", "decisions": [], "state": "s"});
        let checkpoint =
            SessionCheckpoint::new(0, 0, None, None, summary.clone()).expect("checkpoint");

        // Act
        store
            .publish_checkpoint("checkpoints", &checkpoint)
            .await
            .expect("publish");
        let regressed = SessionCheckpoint::new(0, 0, None, None, summary.clone()).expect("record");
        // Republishing the same coverage is idempotent, never a regression.
        store
            .publish_checkpoint("checkpoints", &regressed)
            .await
            .expect("republish current boundary");
        let beyond = SessionCheckpoint::new(9, 0, None, None, summary.clone()).expect("record");
        let beyond = store.publish_checkpoint("checkpoints", &beyond).await;
        let wrong_generation = SessionCheckpoint::new(1, 7, None, None, summary).expect("record");
        let wrong_generation = store
            .publish_checkpoint("checkpoints", &wrong_generation)
            .await;
        let missing = store.publish_checkpoint("missing", &checkpoint).await;

        // Assert
        assert!(matches!(missing, Err(SessionError::NotFound { .. })));
        assert!(matches!(beyond, Err(SessionError::CheckpointStale { .. })));
        assert!(matches!(
            wrong_generation,
            Err(SessionError::CheckpointStale { .. })
        ));
        let loaded = store.load_session("checkpoints").await.expect("load");
        assert_eq!(loaded.latest_completed_turn, Some(1));
        assert_eq!(
            loaded.checkpoint.expect("checkpoint").covered_through(),
            0,
            "the rejected publications changed nothing"
        );
        assert_eq!(
            loaded.turns.len(),
            1,
            "turn zero is covered by the checkpoint"
        );
        assert!(matches!(
            &loaded.turns[0][0],
            ModelMessage::User(text) if text == "one"
        ));
    }
}

#[tokio::test]
async fn write_settlement_retries_preserve_terminal_outcomes() {
    // Arrange
    for store in stores().await {
        for applied in [false, true] {
            let id = format!("settlement-{applied}");
            store
                .create_session(&NewSession::new(&id, schema()), None, 100)
                .await
                .expect("create");
            let turn = store
                .begin_turn(store.clone(), &id, &TurnInput::from("first"), &options(), 0)
                .await
                .expect("turn");
            let write = store
                .write_intent(
                    turn.owner(),
                    "call",
                    Path::new("repo"),
                    "file",
                    None,
                    b"new",
                )
                .await
                .expect("intent");

            // Act
            store
                .finish_write(turn.owner(), write, applied)
                .await
                .expect("initial settlement");
            let settled = store.load_writes(&id).await.expect("settled record");
            store
                .finish_write(turn.owner(), write, applied)
                .await
                .expect("idempotent retry");
            let active_conflict = store.finish_write(turn.owner(), write, !applied).await;
            store
                .complete_turn(turn.owner(), &[], None)
                .await
                .expect("complete");
            let successor = store
                .begin_turn(
                    store.clone(),
                    &id,
                    &TurnInput::from("successor"),
                    &options(),
                    0,
                )
                .await
                .expect("successor");
            store
                .finish_write(turn.owner(), write, applied)
                .await
                .expect("delayed idempotent retry");
            let delayed_conflict = store.finish_write(turn.owner(), write, !applied).await;

            // Assert
            assert!(active_conflict.is_err());
            assert!(delayed_conflict.is_err());
            assert_eq!(
                settled[0].status,
                if applied {
                    WriteStatus::Applied
                } else {
                    WriteStatus::Failed
                }
            );
            assert_eq!(store.load_writes(&id).await.expect("unchanged"), settled);
            store.renew(successor.owner()).await.expect("still owned");
        }
    }
}

#[tokio::test]
async fn conflicting_write_settlements_have_one_winner() {
    // Arrange
    for store in stores().await {
        store
            .create_session(&NewSession::new("settlement-race", schema()), None, 100)
            .await
            .expect("create");
        let turn = store
            .begin_turn(
                store.clone(),
                "settlement-race",
                &TurnInput::from("prompt"),
                &options(),
                0,
            )
            .await
            .expect("turn");
        let write = store
            .write_intent(
                turn.owner(),
                "call",
                Path::new("repo"),
                "file",
                None,
                b"new",
            )
            .await
            .expect("intent");

        // Act
        let (applied, failed) = tokio::join!(
            store.finish_write(turn.owner(), write, true),
            store.finish_write(turn.owner(), write, false),
        );

        // Assert
        assert_ne!(applied.is_ok(), failed.is_ok());
        let records = store.load_writes("settlement-race").await.expect("writes");
        assert_eq!(
            records[0].status,
            if applied.is_ok() {
                WriteStatus::Applied
            } else {
                WriteStatus::Failed
            }
        );
    }
}

#[tokio::test]
async fn concurrent_creation_and_acquisition_have_one_winner() {
    // Arrange
    for store in stores().await {
        let config = NewSession::new("race", schema());
        let selected = options();

        // Act
        let (first, second) = tokio::join!(
            store.create_session(&config, None, 100),
            store.create_session(&config, None, 100),
        );
        let creations = [first, second];
        let first_input = TurnInput::from("first");
        let second_input = TurnInput::from("second");
        let (first, second) = tokio::join!(
            store.begin_turn(store.clone(), "race", &first_input, &selected, 0),
            store.begin_turn(store.clone(), "race", &second_input, &selected, 0),
        );
        let acquisitions = [first, second];

        // Assert
        assert_eq!(creations.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            creations
                .iter()
                .filter(|result| matches!(result, Err(SessionError::AlreadyExists { .. })))
                .count(),
            1
        );
        assert_eq!(
            acquisitions.iter().filter(|result| result.is_ok()).count(),
            1
        );
        assert_eq!(
            acquisitions
                .iter()
                .filter(|result| matches!(result, Err(SessionError::Busy { .. })))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn owner_lookup_preserves_identity_across_activation() {
    // Arrange
    for store in stores().await {
        store
            .create_session(&NewSession::new("identity", schema()), None, 256)
            .await
            .expect("create");
        let acquired = store
            .begin_turn(
                Arc::clone(&store),
                "identity",
                &TurnInput::from("prompt"),
                &options(),
                0,
            )
            .await
            .expect("acquire");
        let active = acquired.owner();
        let reserved = TurnOwner::new(
            active.store_identity().clone(),
            active.session_id().to_string(),
            active.turn_position(),
            active.token().to_vec(),
        );
        let reservations = HashMap::from([(reserved.clone(), "reservation")]);

        // Act / Assert
        assert_ne!(
            reserved.interruption_error_type(),
            active.interruption_error_type()
        );
        assert_eq!(&reserved, active);
        assert_eq!(reservations.get(active), Some(&"reservation"));
        store.renew(active).await.expect("validate activated owner");
        for foreign in [
            TurnOwner::new(
                StoreIdentity::unique(),
                active.session_id().to_string(),
                active.turn_position(),
                active.token().to_vec(),
            ),
            TurnOwner::new(
                active.store_identity().clone(),
                "other".to_string(),
                active.turn_position(),
                active.token().to_vec(),
            ),
            TurnOwner::new(
                active.store_identity().clone(),
                active.session_id().to_string(),
                active.turn_position() + 1,
                active.token().to_vec(),
            ),
            TurnOwner::new(
                active.store_identity().clone(),
                active.session_id().to_string(),
                active.turn_position(),
                b"different-token".to_vec(),
            ),
        ] {
            assert_ne!(&foreign, active);
            assert_eq!(reservations.get(&foreign), None);
        }
    }
}

struct GatedModel {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Model for GatedModel {
    async fn complete(&self, _: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.entered.notify_one();
        self.release.notified().await;

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({"answer":"done"}),
        )))
    }
}

#[tokio::test(start_paused = true)]
async fn short_and_shortened_leases_renew_through_real_turn_completion() {
    for initial_seconds in [30, 300] {
        // Arrange
        let store = Arc::new(ExternalStore::with_leases(
            Duration::from_secs(initial_seconds),
            Duration::from_secs(6),
        ));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut session = Harness::new(GatedModel {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        })
        .store(store.clone())
        .session("lease", schema())
        .create()
        .await
        .expect("session");
        let task = tokio::spawn(async move { session.send("work").await });
        entered.notified().await;

        // Act
        let first_renewal = (initial_seconds / 2).min(100);
        tokio::time::advance(Duration::from_secs(first_renewal - 1)).await;
        tokio::task::yield_now().await;
        assert_eq!(store.renewals.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        for expected in 1..=7 {
            tokio::time::timeout(Duration::from_secs(1), store.renewed.notified())
                .await
                .expect("renew before confirmed expiry");
            assert_eq!(store.renewals.load(Ordering::SeqCst), expected);
            assert!(!task.is_finished());
            if expected != 7 {
                tokio::time::advance(Duration::from_secs(3)).await;
            }
        }
        release.notify_one();
        let outcome = task
            .await
            .expect("turn task")
            .expect("complete after renewal");

        // Assert
        assert_eq!(outcome.output()["answer"], "done");
        assert_eq!(
            store
                .load_session("lease")
                .await
                .expect("history")
                .turns
                .len(),
            1
        );
    }
}

#[tokio::test(start_paused = true)]
async fn expired_renewal_acknowledgment_cancels_a_short_lease_turn() {
    // Arrange
    let store = Arc::new(ExternalStore::with_leases(
        Duration::from_secs(30),
        Duration::ZERO,
    ));
    let entered = Arc::new(Notify::new());
    let mut session = Harness::new(GatedModel {
        entered: Arc::clone(&entered),
        release: Arc::new(Notify::new()),
    })
    .store(store.clone())
    .session("expired", schema())
    .create()
    .await
    .expect("session");
    let task = tokio::spawn(async move { session.send("work").await });
    entered.notified().await;

    // Act
    tokio::time::advance(Duration::from_secs(15)).await;
    let outcome = task.await.expect("turn task");

    // Assert
    assert!(matches!(outcome, Err(SessionError::OwnershipLost { .. })));
    assert_eq!(store.renewals.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load_session("expired").await.expect("history").turns,
        Vec::<Vec<ModelMessage>>::new()
    );
}

#[tokio::test]
async fn real_turns_resume_and_preserve_captured_store_and_options() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let original = Harness::new(Echo {
            requests: Arc::clone(&requests),
        })
        .store(Arc::clone(&store));
        let builder = original
            .session("capture", schema())
            .system_prompt("policy");
        let other = Arc::new(ExternalStore::new());
        let changed = original.store(other);
        let mut session = builder.create().await.expect("create");

        // Act
        session.send("one").await.expect("first");
        drop(session);
        let mut session = harness(Arc::clone(&store))
            .resume("capture")
            .await
            .expect("resume");
        let result = session.send("two").await.expect("second");

        // Assert
        assert_eq!(result.output()["answer"], "two");
        assert!(matches!(
            changed.resume("capture").await,
            Err(SessionError::NotFound { .. })
        ));
        assert!(matches!(
            requests.lock().expect("requests")[0].messages()[0],
            ModelMessage::System(_)
        ));
        assert_eq!(
            store
                .load_session("capture")
                .await
                .expect("history")
                .turns
                .len(),
            2
        );
    }
}

#[tokio::test]
async fn bounded_history_and_continuation_policy_are_shared() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let harness = Harness::new(Echo {
            requests: Arc::clone(&requests),
        })
        .store(Arc::clone(&store))
        .max_history_bytes(std::num::NonZeroUsize::new(50).expect("budget"));
        let mut session = harness
            .session("bounded", schema())
            .create()
            .await
            .expect("create");

        // Act
        session.send("one").await.expect("first");
        session.send("two").await.expect("second");
        let mut changed_schema = schema().value().clone();
        changed_schema["description"] = json!("Changed contract");
        let changed = TurnOptions::new(
            OutputSchema::new(changed_schema).expect("changed schema"),
            ToolPolicy::default(),
            TurnLimits::default(),
        );
        session
            .send_with_options("three", changed)
            .await
            .expect("changed");

        // Assert
        {
            let requests = requests.lock().expect("requests");
            assert_eq!(requests[1].provider_session_id(), Some("continuation"));
            assert_eq!(requests[2].provider_session_id(), None);
        }
        let loaded = store.load_session("bounded").await.expect("bounded");
        assert!(
            loaded
                .turns
                .iter()
                .flatten()
                .map(ModelMessage::retained_bytes)
                .sum::<usize>()
                <= 50
        );
        assert!(loaded.turns.len() < 3);
    }
}

#[tokio::test]
async fn external_reservation_rejects_foreign_store_and_expired_acknowledgment() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    let foreign = TurnOwner::new(
        StoreIdentity::new("test", "foreign"),
        "session".to_string(),
        1,
        vec![1],
    );

    // Act / Assert
    assert!(
        AcquiredTurn::new(
            Arc::clone(&store),
            foreign,
            Instant::now(),
            Vec::new(),
            None
        )
        .is_err()
    );
    let owner = TurnOwner::new(store.identity().clone(), "session".to_string(), 1, vec![1]);
    let expired = AcquiredTurn::new(
        Arc::clone(&store),
        owner,
        Instant::now() - Duration::from_secs(1),
        Vec::new(),
        None,
    )
    .expect("reservation");
    assert!(matches!(
        expired.activate(),
        Err(SessionError::OwnershipLost { .. })
    ));
    assert_eq!(
        StoreIdentity::new("backend", "key"),
        StoreIdentity::new("backend", "key")
    );
    assert_ne!(StoreIdentity::unique(), StoreIdentity::unique());
    assert!(
        StoredTurnOptions::decode(&StoredTurnOptions::encode(&options()))
            .expect("snapshot")
            .continuation_compatible(&options())
    );
}

struct BlockingModel {
    entered: Arc<Notify>,
}

#[async_trait]
impl Model for BlockingModel {
    async fn complete(&self, _: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn separate_handles_share_admission_and_different_sessions_progress() {
    // Arrange
    for store in stores().await {
        let entered = Arc::new(Notify::new());
        let first = Harness::new(BlockingModel {
            entered: Arc::clone(&entered),
        })
        .store(Arc::clone(&store));
        let mut session = first
            .session("shared", schema())
            .create()
            .await
            .expect("create");
        let task = tokio::spawn(async move { session.send("blocked").await });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("model entered");
        let other = harness(Arc::clone(&store));
        let mut competing = other.resume("shared").await.expect("resume");

        // Act / Assert
        assert!(matches!(
            competing.send("busy").await,
            Err(SessionError::Busy { .. })
        ));
        let mut independent = other
            .session("independent", schema())
            .create()
            .await
            .expect("independent");
        independent
            .send("works")
            .await
            .expect("other session progresses");
        task.abort();
        assert!(task.await.expect_err("aborted").is_cancelled());
        tokio::task::yield_now().await;
        competing
            .send("recovered")
            .await
            .expect("cleanup before successor");
    }
}

#[tokio::test]
async fn abandoned_acquisition_retains_admission_and_never_executes() {
    // Arrange
    for store in stores().await {
        for after_commit in [false, true] {
            let id = format!("cancel-{after_commit}");
            let gate = Arc::new(Gate::new(Arc::clone(&store), after_commit));
            gate.fail_cleanup.store(true, Ordering::SeqCst);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let first = Harness::new(Echo {
                requests: Arc::clone(&requests),
            })
            .store(gate.clone());
            let mut session = first.session(&id, schema()).create().await.expect("create");
            let task = tokio::spawn(async move { session.send("abandoned").await });
            tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
                .await
                .expect("acquisition entered");
            let mut successor = harness(Arc::clone(&store))
                .resume(&id)
                .await
                .expect("resume");

            // Act
            task.abort();
            assert!(task.await.expect_err("aborted").is_cancelled());
            assert!(matches!(
                successor.send("blocked").await,
                Err(SessionError::Busy { .. })
            ));
            gate.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), gate.interrupted.notified())
                .await
                .expect("cleanup attempted");
            let unresolved = successor.send("still blocked").await;

            // Assert
            assert!(matches!(unresolved, Err(SessionError::Store { .. })));
            assert!(requests.lock().expect("requests").is_empty());
            gate.fail_cleanup.store(false, Ordering::SeqCst);
            successor
                .send("recovered")
                .await
                .expect("owner cleanup and successor");
            assert_eq!(
                store.load_session(&id).await.expect("history").turns.len(),
                1
            );
        }
    }
}

#[tokio::test]
async fn sqlite_reopen_and_configuration_precedence_preserve_stores() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("session.db");
    let external: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    let original = harness(Arc::clone(&external));
    let captured = original.session("external", schema());
    let sqlite = original.database(&path);
    let mut session = sqlite
        .session("disk", schema())
        .create()
        .await
        .expect("disk session");

    // Act
    session.send("persisted").await.expect("turn");
    drop(session);
    drop(sqlite);
    let store: Arc<dyn SessionStore> = Arc::new(SqliteStore::open(&path).await.expect("reopen"));
    let independent: Arc<dyn SessionStore> =
        Arc::new(SqliteStore::open(&path).await.expect("independent pool"));
    let mut external_session = captured.create().await.expect("captured external");
    external_session
        .send("external")
        .await
        .expect("external turn");

    // Assert
    assert_eq!(store.identity(), independent.identity());
    assert_eq!(
        store
            .load_session("disk")
            .await
            .expect("reopened history")
            .turns
            .len(),
        1
    );
    assert!(matches!(
        store.load_session("external").await,
        Err(SessionError::NotFound { .. })
    ));
    let mut resumed = harness(store).resume("disk").await.expect("resume");
    resumed.send("second").await.expect("resumed turn");
    assert_eq!(
        independent
            .load_session("disk")
            .await
            .expect("shared pool identity")
            .turns
            .len(),
        2
    );
    harness(external)
        .run_once("stateless", schema())
        .await
        .expect("stateless");
}

#[tokio::test]
async fn acquisition_task_failure_retains_backend_error_and_releases_admission() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    let gate = Arc::new(Gate::new(Arc::clone(&store), true));
    gate.panic_acquire.store(true, Ordering::SeqCst);
    let mut session = harness(gate)
        .session("panic", schema())
        .create()
        .await
        .expect("session");

    // Act
    let error = session
        .send("panic")
        .await
        .expect_err("backend task panicked");
    let mut successor = harness(store).resume("panic").await.expect("resume");

    // Assert
    assert!(error.to_string().contains("acquire session turn"));
    assert!(std::error::Error::source(&error).is_some());
    successor
        .send("retry")
        .await
        .expect("no reservation was submitted");
}

#[tokio::test]
async fn temporary_sqlite_handles_share_one_database_until_dropped() {
    // Arrange
    for path in ["", ":memory:"] {
        let database = SqliteStore::open(Path::new(path))
            .await
            .expect("temporary SQLite");
        let shared: Arc<dyn SessionStore> = Arc::new(database.clone());
        let independent = SqliteStore::open(Path::new(path))
            .await
            .expect("independent SQLite");
        let mut session = harness(Arc::clone(&shared))
            .session("temporary", schema())
            .create()
            .await
            .expect("session");

        // Act
        session.send("first").await.expect("first turn");
        let mut resumed = harness(Arc::new(database))
            .resume("temporary")
            .await
            .expect("shared instance");
        resumed.send("second").await.expect("second turn");

        // Assert
        assert_ne!(shared.identity(), independent.identity());
        assert_eq!(
            shared
                .load_session("temporary")
                .await
                .expect("retained history")
                .turns
                .len(),
            2
        );
        assert!(matches!(
            independent.load_session("temporary").await,
            Err(SessionError::NotFound { .. })
        ));
    }
}

#[tokio::test]
async fn unresolved_commands_fence_model_switches_until_owner_reconciliation() {
    // Arrange
    for store in stores().await.into_iter().skip(1) {
        store
            .create_session(&NewSession::new("switch-commands", schema()), None, 1024)
            .await
            .expect("session");
        let turn = store
            .begin_turn(
                Arc::clone(&store),
                "switch-commands",
                &TurnInput::from("run"),
                &options(),
                0,
            )
            .await
            .expect("turn");
        let id = store
            .command_intent(
                turn.owner(),
                &CommandIntent {
                    call_id: "command".into(),
                    command: "effect".into(),
                    policy: json!({}),
                    workspace: "/workspace".into(),
                },
            )
            .await
            .expect("intent");
        store.interrupt(turn.owner()).await.expect("stop owner");
        let identity = ag_harness::ExecutionIdentity::new("next", "1").expect("identity");
        let capabilities = ag_harness::ModelCapabilities {
            context_budget: None,
            image_input: false,
            native_continuation: false,
            tool_calls: true,
        };

        // Act / Assert
        assert!(matches!(
            store
                .switch_model("switch-commands", 0, &identity, None, capabilities)
                .await,
            Err(SessionError::Busy { .. })
        ));
        let unchanged = store
            .load_session("switch-commands")
            .await
            .expect("unchanged selection");
        assert_eq!(unchanged.model_generation, 0);
        assert_eq!(unchanged.registration_identity, None);
        store
            .reconcile_command(turn.owner(), id)
            .await
            .expect("owner reconciliation");
        assert_eq!(
            store
                .switch_model("switch-commands", 0, &identity, None, capabilities)
                .await
                .expect("switch after reconciliation"),
            1
        );
        assert!(matches!(
            store
                .begin_turn(
                    Arc::clone(&store),
                    "switch-commands",
                    &TurnInput::from("stale"),
                    &options(),
                    0
                )
                .await,
            Err(SessionError::StaleModel { .. })
        ));
        let next = store
            .begin_turn(
                Arc::clone(&store),
                "switch-commands",
                &TurnInput::from("next"),
                &options(),
                1,
            )
            .await
            .expect("new model turn");
        assert_ne!(next.owner(), turn.owner());
        let records = store
            .load_commands("switch-commands")
            .await
            .expect("records");
        assert_eq!(records[0].owner(), turn.owner());
        assert!(records[0].reconciled);
        assert!(records[0].outcome.is_none());
        store.interrupt(next.owner()).await.expect("stop successor");
    }
}
