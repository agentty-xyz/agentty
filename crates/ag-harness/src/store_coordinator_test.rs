use std::sync::{Arc, Mutex};

use crate::store_conformance_test::{lifecycle, options, schema, stores};
use crate::store_coordinator::{AdmittedStore, admission};
use crate::{
    CommandIntent, HostRequest, HostTurnAcquisition, HostTurnStatus, NewSession, SessionStore,
    TurnInput,
};

#[tokio::test]
async fn admission_decorator_forwards_the_complete_store_contract() {
    // Arrange
    for store in stores().await {
        let admission = admission(store.identity(), "session").expect("admission");
        let decorated: Arc<dyn SessionStore> = Arc::new(AdmittedStore {
            admission: Mutex::new(Some(Arc::new(admission))),
            lease: Mutex::new(None),
            settlement: None,
            store,
        });

        // Act / Assert
        lifecycle(Arc::clone(&decorated)).await;
        decorated
            .switch_model(
                "session",
                0,
                &crate::ExecutionIdentity::new("next", "1").expect("identity"),
                None,
                crate::ModelCapabilities {
                    context_budget: None,
                    image_input: false,
                    native_continuation: true,
                    tool_calls: true,
                },
            )
            .await
            .expect("switch through decorator");
        let checkpoint = crate::SessionCheckpoint::new(
            0,
            1,
            None,
            None,
            serde_json::json!({"context": "c", "decisions": [], "state": "s"}),
        )
        .expect("checkpoint");
        decorated
            .publish_checkpoint("session", &checkpoint)
            .await
            .expect("publish through decorator");
        let loaded = decorated.load_session("session").await.expect("load");
        assert_eq!(loaded.checkpoint, Some(checkpoint));
    }
}

#[tokio::test]
async fn admission_decorator_forwards_host_recovery() {
    // Arrange
    for store in stores().await {
        let admission = admission(store.identity(), "session").expect("admission");
        let decorated: Arc<dyn SessionStore> = Arc::new(AdmittedStore {
            admission: Mutex::new(Some(Arc::new(admission))),
            lease: Mutex::new(None),
            settlement: None,
            store,
        });
        decorated
            .create_session(&NewSession::new("session", schema()), None, 1024)
            .await
            .expect("session");
        let request =
            HostRequest::from_configuration("id".into(), serde_json::json!({})).expect("request");

        // Act
        let turn = decorated
            .begin_request(
                Arc::clone(&decorated),
                "session",
                &TurnInput::from("prompt"),
                &options(),
                &request,
                0,
            )
            .await
            .expect("turn");
        let record = decorated
            .load_request("session", "id")
            .await
            .expect("lookup")
            .expect("record");

        // Assert
        assert!(matches!(turn, HostTurnAcquisition::Acquired(_)));
        assert!(matches!(record.status, HostTurnStatus::InProgress));
        assert_eq!(record.request, request);
    }
}

#[tokio::test]
async fn admission_decorator_preserves_command_owner_and_unknown_outcome() {
    // Arrange
    for store in stores().await.into_iter().skip(1) {
        let guard = admission(store.identity(), "commands").expect("admission");
        let decorated: Arc<dyn SessionStore> = Arc::new(AdmittedStore {
            admission: Mutex::new(Some(Arc::new(guard))),
            lease: Mutex::new(None),
            settlement: None,
            store: Arc::clone(&store),
        });
        decorated
            .create_session(&NewSession::new("commands", schema()), None, 1024)
            .await
            .expect("session");
        let mut turn = decorated
            .begin_turn(
                Arc::clone(&decorated),
                "commands",
                &TurnInput::from("run"),
                &options(),
                0,
            )
            .await
            .expect("turn");
        let intent = CommandIntent {
            call_id: "call".into(),
            command: "effect".into(),
            policy: serde_json::json!({}),
            workspace: "/workspace".into(),
        };
        let id = decorated
            .command_intent(turn.owner(), &intent)
            .await
            .expect("intent");

        // Act
        decorated.interrupt(turn.owner()).await.expect("interrupt");
        decorated
            .reconcile_command(turn.owner(), id)
            .await
            .expect("reconcile");
        turn.guard.disarm();
        let records = decorated.load_commands("commands").await.expect("records");

        // Assert
        assert_eq!(
            records,
            store
                .load_commands("commands")
                .await
                .expect("underlying records")
        );
        assert_eq!(records[0].intent, intent);
        assert_eq!(records[0].owner(), turn.owner());
        assert!(records[0].outcome.is_none());
        assert!(records[0].reconciled);
    }
}
