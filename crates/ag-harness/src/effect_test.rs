use std::sync::Arc;

use tokio::sync::Mutex;

use crate::effect::Effects;

#[tokio::test]
async fn unacknowledged_worker_retains_admission_after_all_controls_drop() {
    // Arrange
    let admission = Arc::new(Mutex::new(()));
    let effects = Effects::default();
    effects.admit(Arc::new(admission.clone().lock_owned().await));
    let mut lease = effects.retain();
    lease.starting();

    // Act
    drop(lease);
    let error = effects.settled().await.expect_err("unresolved effect");
    drop(effects);

    // Assert
    assert!(error.is_unresolved());
    assert!(admission.try_lock().is_err());
}

#[tokio::test]
async fn unacknowledged_ephemeral_worker_is_observable() {
    // Arrange
    let effects = Effects::default();
    let mut lease = effects.retain();
    lease.starting();

    // Act
    drop(lease);

    // Assert
    assert!(
        effects
            .settled()
            .await
            .expect_err("unresolved")
            .is_unresolved()
    );
}

#[tokio::test]
async fn dropped_outcome_recording_reports_failure_without_claiming_unknown_effects() {
    // Arrange
    let effects = Effects::default();
    let mut lease = effects.retain();
    lease.starting();
    lease.acknowledged();

    // Act
    drop(lease);

    // Assert
    let error = effects.settled().await.expect_err("recording failed");
    assert!(!error.is_unresolved());
    assert!(error.to_string().contains("outcome recording"));
}
