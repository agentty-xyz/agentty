use std::sync::{Arc, Mutex};

use crate::store_conformance_test::{lifecycle, options, schema, stores};
use crate::store_coordinator::{AdmittedStore, admission};
use crate::{HostRequest, HostTurnAcquisition, HostTurnStatus, NewSession, SessionStore};

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
                    native_continuation: true,
                    tool_calls: true,
                },
            )
            .await
            .expect("switch through decorator");
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
                "prompt",
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
