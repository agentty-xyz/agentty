use std::sync::Mutex;
use std::time::Duration;

use crate::{HeartbeatClock, MockOperationRepository, execute, recover};

#[tokio::test(start_paused = true)]
async fn heartbeats_and_terminal_tracking_preserve_operation_result() {
    for fail in [false, true] {
        // Arrange
        let mut store = MockOperationRepository::<String>::new();
        store
            .expect_heartbeat()
            .withf(|id| id == "operation")
            .times(1)
            .returning(|_| Err("heartbeat unavailable".into()));
        if fail {
            store
                .expect_mark_session_operation_failed()
                .withf(|id, error| id == "operation" && error == "publish failed")
                .times(1)
                .returning(|_, _| Err("tracking unavailable".into()));
        } else {
            store
                .expect_mark_session_operation_done()
                .times(1)
                .returning(|_| Ok(()));
        }
        let errors = Mutex::new(Vec::new());
        // Act
        let result = execute(
            &store,
            &HeartbeatClock,
            "operation",
            async {
                tokio::time::sleep(Duration::from_secs(31)).await;
                if fail { Err("publish failed") } else { Ok(()) }
            },
            |error| errors.lock().expect("test operation succeeds").push(error),
        )
        .await;
        // Assert
        assert_eq!(result.is_err(), fail);
        assert_eq!(
            errors.lock().expect("test operation succeeds").len(),
            if fail { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn recovery_requires_successful_load_and_host_reconciliation() {
    // Arrange
    let mut store = MockOperationRepository::<String>::new();
    store
        .expect_load_unfinished_session_operations()
        .times(1)
        .returning(|| Err("read failed".into()));
    // Act
    let result: Result<(), String> = recover(&store, "restart", |_| async { Ok(()) }).await;
    // Assert
    assert_eq!(result, Err("read failed".into()));
    store.checkpoint();
    store
        .expect_load_unfinished_session_operations()
        .times(1)
        .returning(|| Ok(vec![]));
    let result: Result<(), String> = recover(&store, "restart", |_| async {
        Err("reconcile failed".into())
    })
    .await;
    assert_eq!(result, Err("reconcile failed".into()));
    store.checkpoint();
    store
        .expect_load_unfinished_session_operations()
        .times(2)
        .returning(|| Ok(vec![]));
    store
        .expect_fail_unfinished_session_operations()
        .withf(|reason| reason == "restart")
        .times(1)
        .returning(|_| Ok(()));
    let result: Result<(), String> = recover(&store, "restart", |operations| async move {
        assert!(operations.is_empty());
        Ok(())
    })
    .await;
    assert_eq!(result, Ok(()));
    store
        .expect_fail_unfinished_session_operations()
        .times(1)
        .returning(|_| Err("write failed".into()));
    let result: Result<(), String> = recover(&store, "restart", |_| async { Ok(()) }).await;
    assert_eq!(result, Err("write failed".into()));
}
