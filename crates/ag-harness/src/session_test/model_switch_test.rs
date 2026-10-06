use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tokio::sync::Notify;

use crate::SessionError;
use crate::input::TurnInput;
use crate::model::{ModelCapabilities, ModelMessage};
use crate::recovery::ExecutionIdentity;
use crate::session::tests::support::{schema, turn_options};
use crate::session::{Database, LoadedSession, ReservationObserver};
use crate::store::{AcquiredTurn, ModelSwitch, NewSession, SessionStore};

struct Validated {
    entered: Notify,
    once: AtomicBool,
    release: Notify,
}

#[async_trait]
impl ReservationObserver for Validated {
    async fn model_validated(&self) {
        if self.once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    async fn committed(&self) -> Result<(), SessionError> {
        Ok(())
    }
}

#[tokio::test]
async fn switch_revalidates_history_completed_during_validation() {
    // Arrange
    let reserve_successor = false;

    // Act
    let (result, loaded) = switch_across_concurrent_unsupported_history(reserve_successor).await;

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::UnsupportedModelHistory { .. })
    ));
    assert_eq!(loaded.model_generation, 0);
    assert_eq!(
        loaded.provider_session_id.as_deref(),
        Some("old-continuation")
    );
}

#[tokio::test]
async fn switch_reports_unsupported_history_before_a_concurrent_successor_turn() {
    // Arrange
    let reserve_successor = true;

    // Act
    let (result, loaded) = switch_across_concurrent_unsupported_history(reserve_successor).await;

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::UnsupportedModelHistory { .. })
    ));
    assert_eq!(loaded.model_generation, 0);
}

/// Completes a turn with unsupported reasoning while a model switch waits
/// between history validation and its writer transaction, optionally
/// reserving an active successor turn before the switch resumes.
async fn switch_across_concurrent_unsupported_history(
    reserve_successor: bool,
) -> (Result<i64, SessionError>, LoadedSession) {
    let mut database = Database::open_in_memory().await.expect("database");
    let observer = Arc::new(Validated {
        entered: Notify::new(),
        once: AtomicBool::new(true),
        release: Notify::new(),
    });
    database.reservation_observer = observer.clone();
    let store: Arc<dyn SessionStore> = Arc::new(database);
    store
        .create_session(&NewSession::new("switch", schema()), None, 1024)
        .await
        .expect("session");
    let acquired = AcquiredTurn::begin(
        Arc::clone(&store),
        "switch",
        &TurnInput::from("first"),
        &turn_options(),
        0,
    )
    .await
    .expect("acquire");
    let switching = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .switch_model(
                    "switch",
                    &ModelSwitch::new(
                        ExecutionIdentity::new("b", "1").expect("identity"),
                        None,
                        ModelCapabilities::default(),
                        0,
                    ),
                )
                .await
        })
    };
    observer.entered.notified().await;
    store
        .complete_turn(
            acquired.owner(),
            &[ModelMessage::AssistantReasoning {
                content: "answer".into(),
                reasoning_content: "provider state".into(),
            }],
            Some("old-continuation"),
        )
        .await
        .expect("complete");
    let _successor = if reserve_successor {
        Some(
            AcquiredTurn::begin(
                Arc::clone(&store),
                "switch",
                &TurnInput::from("second"),
                &turn_options(),
                0,
            )
            .await
            .expect("successor"),
        )
    } else {
        None
    };
    observer.release.notify_one();
    let result = switching.await.expect("switch task");
    let loaded = store.load_session("switch").await.expect("load");

    (result, loaded)
}
