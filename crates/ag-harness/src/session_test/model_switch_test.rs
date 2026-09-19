use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tokio::sync::Notify;

use crate::input::TurnInput;
use crate::session::tests::support::{schema, turn_options};
use crate::session::{Database, ReservationObserver};
use crate::{
    ExecutionIdentity, ModelCapabilities, ModelMessage, NewSession, SessionError, SessionStore,
};

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

    async fn committed(&self) {}
}

#[tokio::test]
async fn switch_revalidates_history_completed_during_validation() {
    // Arrange
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
    let acquired = store
        .begin_turn(
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
                    0,
                    &ExecutionIdentity::new("b", "1").expect("identity"),
                    None,
                    ModelCapabilities::default(),
                )
                .await
        })
    };
    observer.entered.notified().await;

    // Act
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
    observer.release.notify_one();
    let result = switching.await.expect("switch task");

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::UnsupportedModelHistory { .. })
    ));
    let loaded = store.load_session("switch").await.expect("load");
    assert_eq!(loaded.model_generation, 0);
    assert_eq!(
        loaded.provider_session_id.as_deref(),
        Some("old-continuation")
    );
}
