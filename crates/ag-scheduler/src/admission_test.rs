use std::num::NonZeroUsize;

use crate::admission::SessionAdmission;

#[tokio::test]
async fn shared_admission_waits_for_a_turn_and_releases_on_drop() {
    // Arrange
    let admission = SessionAdmission::new(NonZeroUsize::MIN);
    let first = admission.acquire().await.expect("first slot");
    let second_admission = admission.clone();
    let second = tokio::spawn(async move { second_admission.acquire().await });

    // Act
    tokio::task::yield_now().await;
    let waiting = !second.is_finished();
    drop(first);
    let acquired = second.await.expect("waiter task").expect("released slot");

    // Assert
    assert!(waiting);
    drop(acquired);
    assert!(admission.acquire().await.is_ok());
}

#[tokio::test]
async fn closing_admission_wakes_waiters_with_an_error() {
    // Arrange
    let admission = SessionAdmission::new(NonZeroUsize::MIN);
    let first = admission.acquire().await.expect("first slot");
    let first_cleanup = admission.acquire_cleanup().await.expect("cleanup slot");
    let waiting_admission = admission.clone();
    let waiter = tokio::spawn(async move { waiting_admission.acquire().await });
    let waiting_cleanup = admission.clone();
    let cleanup_waiter = tokio::spawn(async move { waiting_cleanup.acquire_cleanup().await });

    // Act
    tokio::task::yield_now().await;
    admission.close();
    let result = waiter.await.expect("waiter task");
    let cleanup_result = cleanup_waiter.await.expect("cleanup waiter task");

    // Assert
    assert!(result.is_err());
    assert!(cleanup_result.is_err());
    drop(first);
    drop(first_cleanup);
    assert!(admission.acquire().await.is_err());
    assert!(admission.acquire_cleanup().await.is_err());
}

#[tokio::test]
async fn cleanup_slots_are_shared_without_consuming_turn_capacity() {
    // Arrange
    let admission = SessionAdmission::new(NonZeroUsize::MIN);
    let turn = admission.acquire().await.expect("turn slot");
    let cleanup = admission.acquire_cleanup().await.expect("cleanup slot");
    let waiting_admission = admission.clone();
    let waiter = tokio::spawn(async move { waiting_admission.acquire_cleanup().await });

    // Act
    tokio::task::yield_now().await;
    let waiting = !waiter.is_finished();
    drop(cleanup);
    let next_cleanup = waiter.await.expect("waiter task").expect("released slot");

    // Assert
    assert!(waiting);
    drop(next_cleanup);
    drop(turn);
    assert!(admission.acquire_cleanup().await.is_ok());
}
