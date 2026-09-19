use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use tokio::time::Instant;

use super::Status;
use crate::session::Database;
use crate::store_conformance_test::{harness, options, schema};
use crate::{
    ComparisonBase, HostRequest, HostTurnAcquisition, HostTurnStatus, MemoryStore, ModelError,
    ModelMessage, ModelMetadata, NewSession, SessionError, SessionStore, StoreIdentity, TurnError,
    TurnInput, TurnOwner, WriteRecord, WriteStatus,
};

#[tokio::test]
async fn clones_share_state_independent_stores_and_one_shot_are_isolated() {
    // Arrange
    let store = MemoryStore::default();
    let shared = Arc::new(store.clone());
    let independent = Arc::new(MemoryStore::new());
    let first = harness(shared.clone());
    let mut session = first
        .session("same", schema())
        .create()
        .await
        .expect("create");
    let other = harness(independent.clone());

    // Act
    session.send("first").await.expect("send");
    drop(first);
    drop(session);
    let mut resumed = harness(Arc::new(store.clone()))
        .resume("same")
        .await
        .expect("resume clone");
    resumed.send("second").await.expect("shared history");
    let one_shot = other
        .run_once("stateless", schema())
        .await
        .expect("one-shot");

    // Assert
    assert_eq!(store.identity(), shared.identity());
    assert_ne!(store.identity(), independent.identity());
    assert_eq!(
        store
            .load_session("same")
            .await
            .expect("shared")
            .turns
            .len(),
        2
    );
    assert!(matches!(
        independent.load_session("same").await,
        Err(SessionError::NotFound { .. })
    ));
    assert_eq!(one_shot.output()["answer"], "stateless");
    assert!(independent.lock().sessions.is_empty());
    other
        .session("same", schema())
        .create()
        .await
        .expect("independent identifier");
    assert_eq!(
        independent
            .load_session("same")
            .await
            .expect("independent")
            .turns,
        Vec::<Vec<ModelMessage>>::new()
    );
}

#[tokio::test]
async fn creation_and_unknown_owner_errors_leave_state_unchanged() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    let config = NewSession::new("session", schema());
    let metadata = ModelMetadata::new("provider", "model").expect("metadata");
    let unknown = TurnOwner::new(store.identity().clone(), "missing".to_string(), 0, vec![0]);

    // Act
    store
        .create_session(&config, Some(metadata), 100)
        .await
        .expect("create");
    let duplicate = store.create_session(&config, None, 0).await;
    let invalid = store
        .create_session(&NewSession::new(" \n", schema()), None, 100)
        .await;
    let loaded = store.load_session("session").await.expect("load");

    // Assert
    assert!(matches!(duplicate, Err(SessionError::AlreadyExists { .. })));
    assert!(matches!(invalid, Err(SessionError::InvalidData { .. })));
    assert_eq!(loaded.model.as_deref(), Some("model"));
    assert_eq!(loaded.provider.as_deref(), Some("provider"));
    assert_eq!(loaded.max_history_bytes, 100);
    assert_eq!(
        store.load_writes("missing").await.expect("absent writes"),
        Vec::<WriteRecord>::new()
    );
    assert!(matches!(
        store
            .begin_turn(
                store.clone(),
                "missing",
                &TurnInput::from("prompt"),
                &options(),
                0
            )
            .await,
        Err(SessionError::NotFound { .. })
    ));
    assert!(matches!(
        store.renew(&unknown).await,
        Err(SessionError::OwnershipLost { .. })
    ));
    store
        .interrupt(&unknown)
        .await
        .expect("idempotent missing cleanup");
    let empty = TurnOwner::new(store.identity().clone(), "session".to_string(), 0, vec![0]);
    assert!(store.complete_turn(&empty, &[], None).await.is_err());
    store
        .interrupt(&empty)
        .await
        .expect("empty session cleanup");
    assert!(store.finish_write(&empty, 1, true).await.is_err());
}

#[tokio::test]
async fn foreign_owners_cannot_mutate_and_foreign_acquisition_does_not_reserve() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    let foreign_store = Arc::new(MemoryStore::new());
    store
        .create_session(&NewSession::new("session", schema()), None, 100)
        .await
        .expect("create");

    // Act / Assert
    assert!(matches!(
        store
            .begin_turn(
                foreign_store,
                "session",
                &TurnInput::from("wrong"),
                &options(),
                0
            )
            .await,
        Err(SessionError::InvalidData { .. })
    ));
    let turn = store
        .begin_turn(
            store.clone(),
            "session",
            &TurnInput::from("right"),
            &options(),
            0,
        )
        .await
        .expect("not reserved");
    let foreign = TurnOwner::new(
        StoreIdentity::unique(),
        "session".to_string(),
        turn.owner().turn_position(),
        turn.owner().token().to_vec(),
    );
    assert!(store.renew(&foreign).await.is_err());
    assert!(store.complete_turn(&foreign, &[], None).await.is_err());
    assert!(
        store
            .fail_turn(&foreign, &TurnError::Model(ModelError::InvalidResponse))
            .await
            .is_err()
    );
    assert!(store.interrupt(&foreign).await.is_err());
    assert!(
        store
            .write_intent(&foreign, "call", Path::new("repo"), "file", None, b"new")
            .await
            .is_err()
    );
    assert!(store.finish_write(&foreign, 1, true).await.is_err());
    store.renew(turn.owner()).await.expect("owner preserved");
    let wrong_token = TurnOwner::new(
        store.identity().clone(),
        "session".to_string(),
        turn.owner().turn_position(),
        vec![255],
    );
    assert!(store.finish_write(&wrong_token, 1, true).await.is_err());
    assert!(store.finish_write(turn.owner(), 999, true).await.is_err());
    store.interrupt(&wrong_token).await.expect("stale cleanup");
    store.renew(turn.owner()).await.expect("still owned");
}

#[tokio::test]
async fn journal_ids_are_store_wide_and_paths_and_terminal_prompts_are_retained() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    let root = PathBuf::from(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    let mut ids = Vec::new();
    for name in ["first", "second"] {
        store
            .create_session(&NewSession::new(name, schema()), None, 1)
            .await
            .expect("create");
        let turn = store
            .begin_turn(
                store.clone(),
                name,
                &TurnInput::from("retained prompt"),
                &options(),
                0,
            )
            .await
            .expect("turn");

        // Act
        let id = store
            .write_intent(turn.owner(), "create", &root, "file", None, b"new")
            .await
            .expect("intent");
        ids.push(id);
        assert_eq!(
            store.load_writes(name).await.expect("pending")[0].status,
            WriteStatus::Pending
        );
        store
            .fail_turn(turn.owner(), &TurnError::Model(ModelError::InvalidResponse))
            .await
            .expect("fail");
        store
            .finish_write(turn.owner(), id, false)
            .await
            .expect("failed write");

        // Assert
        let loaded = store.load_session(name).await.expect("load");
        assert_eq!(loaded.turns, Vec::<Vec<ModelMessage>>::new());
        let writes = store.load_writes(name).await.expect("writes");
        assert_eq!(writes[0].repository_root, root);
        assert_eq!(writes[0].status, WriteStatus::Failed);
        assert_eq!(writes[0].expected_hash, None);
        let state = store.lock();
        let saved = &state.sessions[name].turns[0];
        assert!(saved.status == Status::Failed);
        assert_eq!(
            saved.error_type,
            Some(format!(
                "{:?}",
                TurnError::Model(ModelError::InvalidResponse).error_type()
            ))
        );
        assert_eq!(
            saved.messages,
            vec![ModelMessage::User("retained prompt".to_string())]
        );
    }
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn bounded_projection_keeps_complete_groups_and_canonical_records() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    store
        .create_session(&NewSession::new("session", schema()), None, 10)
        .await
        .expect("create");
    for prompt in ["one", "oversized prompt", "last"] {
        let turn = store
            .begin_turn(
                store.clone(),
                "session",
                &TurnInput::from(prompt),
                &options(),
                0,
            )
            .await
            .expect("turn");
        store
            .complete_turn(turn.owner(), &[], Some("native"))
            .await
            .expect("complete");
    }

    // Act
    let loaded = store.load_session("session").await.expect("bounded");

    // Assert
    assert_eq!(
        loaded.turns,
        vec![vec![ModelMessage::User("last".to_string())]]
    );
    assert_eq!(store.lock().sessions["session"].turns.len(), 3);
    store
        .lock()
        .sessions
        .get_mut("session")
        .expect("session")
        .configuration
        .max_history_bytes = 0;
    assert_eq!(
        store
            .load_session("session")
            .await
            .expect("zero budget")
            .turns,
        Vec::<Vec<ModelMessage>>::new()
    );
}

#[tokio::test]
async fn allocation_exhaustion_and_invalid_snapshots_never_reserve() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    store
        .create_session(&NewSession::new("session", schema()), None, 100)
        .await
        .expect("create");
    store
        .lock()
        .sessions
        .get_mut("session")
        .expect("session")
        .next_turn = i64::MAX;

    // Act / Assert
    assert!(matches!(
        store
            .begin_turn(
                store.clone(),
                "session",
                &TurnInput::from("exhausted"),
                &options(),
                0
            )
            .await,
        Err(SessionError::InvalidData { .. })
    ));
    store
        .lock()
        .sessions
        .get_mut("session")
        .expect("session")
        .next_turn = 0;
    let turn = store
        .begin_turn(
            store.clone(),
            "session",
            &TurnInput::from("available"),
            &options(),
            0,
        )
        .await
        .expect("not reserved");
    store.lock().next_write = i64::MAX;
    assert!(matches!(
        store
            .write_intent(
                turn.owner(),
                "call",
                Path::new("repo"),
                "file",
                None,
                b"new"
            )
            .await,
        Err(SessionError::InvalidData { .. })
    ));
    assert_eq!(
        store.load_writes("session").await.expect("no intent"),
        Vec::<WriteRecord>::new()
    );
    store
        .complete_turn(turn.owner(), &[], Some("native"))
        .await
        .expect("complete");
    store
        .lock()
        .sessions
        .get_mut("session")
        .expect("session")
        .turns[0]
        .options = "invalid".to_string();
    assert!(
        store
            .begin_turn(
                store.clone(),
                "session",
                &TurnInput::from("invalid snapshot"),
                &options(),
                0
            )
            .await
            .is_err()
    );
    assert_eq!(store.lock().sessions["session"].turns.len(), 1);
}

#[tokio::test]
async fn comparison_compatibility_matches_sqlite_without_live_repository_access() {
    // Arrange
    for store in crate::store_conformance_test::stores().await {
        store
            .create_session(&NewSession::new("comparison", schema()), None, 100)
            .await
            .expect("create");
        let selected =
            options().with_comparison_base(ComparisonBase::fixture("removed-repository"));
        let changed =
            options().with_comparison_base(ComparisonBase::fixture("different-repository"));

        // Act / Assert
        for (current, expected) in [
            (&selected, None),
            (&selected, Some("native")),
            (&changed, None),
            (&options(), None),
        ] {
            let turn = store
                .begin_turn(
                    store.clone(),
                    "comparison",
                    &TurnInput::from("prompt"),
                    current,
                    0,
                )
                .await
                .expect("acquire");
            assert_eq!(turn.provider_session_id.as_deref(), expected);
            store
                .complete_turn(turn.owner(), &[], Some("native"))
                .await
                .expect("complete");
        }
    }
}

#[tokio::test]
async fn expiry_and_stale_cleanup_match_sqlite() {
    // Arrange
    for recover_on_load in [false, true] {
        for ExpiringStore { store, expire } in expiring_stores().await {
            let (first_input, expired_input, next_input) = (
                TurnInput::from("first"),
                TurnInput::from("expired"),
                TurnInput::from("next"),
            );
            store
                .create_session(&NewSession::new("expiry", schema()), None, 100)
                .await
                .expect("create");
            let first = store
                .begin_turn(store.clone(), "expiry", &first_input, &options(), 0)
                .await
                .expect("first");
            store
                .complete_turn(first.owner(), &[], Some("native"))
                .await
                .expect("complete first");
            let old = store
                .begin_turn(store.clone(), "expiry", &expired_input, &options(), 0)
                .await
                .expect("old");
            let write = store
                .write_intent(old.owner(), "call", Path::new("repo"), "file", None, b"new")
                .await
                .expect("intent");

            // Act
            expire();
            assert!(store.renew(old.owner()).await.is_err());
            assert!(store.complete_turn(old.owner(), &[], None).await.is_err());
            assert!(
                store
                    .fail_turn(old.owner(), &TurnError::Model(ModelError::InvalidResponse))
                    .await
                    .is_err()
            );
            assert!(
                store
                    .write_intent(
                        old.owner(),
                        "late",
                        Path::new("repo"),
                        "file",
                        None,
                        b"late"
                    )
                    .await
                    .is_err()
            );
            if recover_on_load {
                assert_eq!(
                    store
                        .load_session("expiry")
                        .await
                        .expect("recover")
                        .provider_session_id,
                    None
                );
            }
            let next = store
                .begin_turn(store.clone(), "expiry", &next_input, &options(), 0)
                .await
                .expect("recover acquisition");
            store
                .complete_turn(next.owner(), &[], Some("successor"))
                .await
                .expect("complete successor");
            store.interrupt(old.owner()).await.expect("stale interrupt");
            store
                .interrupt(old.owner())
                .await
                .expect("repeat interrupt");
            store
                .finish_write(old.owner(), write, true)
                .await
                .expect("settle expired owner");

            // Assert
            let loaded = store.load_session("expiry").await.expect("history");
            assert_eq!(loaded.provider_session_id.as_deref(), Some("successor"));
            assert_eq!(loaded.turns.len(), 2);
            assert_ne!(old.owner(), next.owner());
            assert_eq!(
                store.load_writes("expiry").await.expect("writes")[0].status,
                WriteStatus::Applied
            );
            assert!(
                store
                    .finish_write(next.owner(), write, false)
                    .await
                    .is_err()
            );
        }
    }
}

struct ExpiringStore {
    expire: Box<dyn Fn()>,
    store: Arc<dyn SessionStore>,
}

async fn expiring_stores() -> Vec<ExpiringStore> {
    let memory = Arc::new(MemoryStore::new());
    let timestamp = Arc::new(AtomicI64::new(1000));
    let clock = Arc::clone(&timestamp);
    let sqlite = Arc::new(
        Database::open_with_timestamp_source(
            Path::new(":memory:"),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .await
        .expect("sqlite"),
    );
    let memory_clock = Arc::clone(&memory);

    vec![
        ExpiringStore {
            store: memory,
            expire: Box::new(move || {
                memory_clock
                    .lock()
                    .sessions
                    .get_mut("expiry")
                    .expect("session")
                    .turns
                    .last_mut()
                    .expect("turn")
                    .deadline = Instant::now() - Duration::from_secs(1);
            }),
        },
        ExpiringStore {
            store: sqlite,
            expire: Box::new(move || {
                timestamp.store(2000, Ordering::SeqCst);
            }),
        },
    ]
}

#[tokio::test]
async fn host_recovery_requires_atomic_terminal_output() {
    // Arrange
    let store = Arc::new(MemoryStore::new());
    store
        .create_session(&NewSession::new("host", schema()), None, 1024)
        .await
        .expect("session");
    let request =
        HostRequest::from_configuration("id".into(), serde_json::json!({})).expect("request");
    let HostTurnAcquisition::Acquired(turn) = store
        .begin_request(
            store.clone(),
            "host",
            &TurnInput::from("prompt"),
            &options(),
            &request,
            0,
        )
        .await
        .expect("acquire")
    else {
        std::panic::resume_unwind(Box::new("expected acquisition"));
    };

    // Act
    let result = store.complete_turn(turn.owner(), &[], None).await;
    let record = store
        .load_request("host", "id")
        .await
        .expect("lookup")
        .expect("record");

    // Assert
    assert!(matches!(result, Err(SessionError::InvalidData { .. })));
    assert!(matches!(record.status, HostTurnStatus::InProgress));
}
