use std::time::Duration;

use super::DiffPreviewLoads;

#[tokio::test]
async fn test_newer_load_supersedes_older_ticket_before_decode() {
    // Arrange
    let loads = DiffPreviewLoads::default();
    let older = loads.begin();
    let newer = loads.begin();

    // Act
    let older_permit = older.admit_decode().await;
    let newer_permit = newer.admit_decode().await;

    // Assert
    assert!(older_permit.is_none());
    assert!(newer_permit.is_some());
}

#[tokio::test]
async fn test_supersede_rejects_outstanding_ticket() {
    // Arrange
    let loads = DiffPreviewLoads::default();
    let ticket = loads.begin();

    // Act
    loads.supersede();
    let permit = ticket.admit_decode().await;

    // Assert
    assert!(permit.is_none());
}

#[tokio::test]
async fn test_decode_waits_for_the_held_permit() {
    // Arrange
    let loads = DiffPreviewLoads::default();
    let first = loads.begin();
    let held_permit = first.admit_decode().await;

    // Act
    let waiting = tokio::time::timeout(Duration::from_millis(20), first.admit_decode()).await;
    drop(held_permit);
    let admitted = first.admit_decode().await;

    // Assert
    assert!(waiting.is_err());
    assert!(admitted.is_some());
}
