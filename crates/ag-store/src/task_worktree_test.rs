//! Task worktree record tests: the per-session limit, state transitions,
//! and session cascade.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ag_session::{TASK_WORKTREE_LIMIT, TaskWorktreeId};

use crate::{Database, TaskWorktreeReservation, TaskWorktreeRow, TaskWorktreeState};

#[tokio::test]
async fn reservations_stop_at_the_session_limit_without_affecting_other_sessions() {
    // Arrange
    let (database, _clock) = database_with_sessions().await;
    let worktrees = database.task_worktrees();
    for index in 0..TASK_WORKTREE_LIMIT {
        let reservation = worktrees
            .reserve_task_worktree(&worktree("session", &format!("writer-{index}")), "base")
            .await
            .expect("reservation within the limit");
        assert_eq!(reservation, TaskWorktreeReservation::Reserved);
    }

    // Act
    let over_limit = worktrees
        .reserve_task_worktree(&worktree("session", "extra"), "base")
        .await
        .expect("limit outcome");
    let other_session = worktrees
        .reserve_task_worktree(&worktree("other", "extra"), "base")
        .await
        .expect("independent session");

    // Assert
    assert_eq!(over_limit, TaskWorktreeReservation::LimitReached);
    assert_eq!(other_session, TaskWorktreeReservation::Reserved);
    assert_eq!(
        worktrees
            .list_task_worktrees("session")
            .await
            .expect("listing")
            .len(),
        TASK_WORKTREE_LIMIT
    );
}

#[tokio::test]
async fn reservations_record_the_base_commit_in_reservation_order() {
    // Arrange
    let (database, clock) = database_with_sessions().await;
    let worktrees = database.task_worktrees();

    // Act
    clock.store(200, Ordering::SeqCst);
    worktrees
        .reserve_task_worktree(&worktree("session", "later"), "commit-later")
        .await
        .expect("later reservation");
    clock.store(100, Ordering::SeqCst);
    for id in ["b", "a"] {
        worktrees
            .reserve_task_worktree(&worktree("other", id), &format!("commit-{id}"))
            .await
            .expect("same-second reservation");
    }
    let listed = worktrees
        .list_task_worktrees("other")
        .await
        .expect("listing");

    // Assert: same-second reservations list by identifier.
    assert_eq!(
        listed,
        [
            reserved_row("other", "a", "commit-a", 100),
            reserved_row("other", "b", "commit-b", 100),
        ]
    );
    assert_eq!(
        worktrees
            .list_task_worktrees("session")
            .await
            .expect("session listing"),
        [reserved_row("session", "later", "commit-later", 200)]
    );
}

#[tokio::test]
async fn reservations_reject_missing_sessions_duplicates_and_blank_values() {
    // Arrange
    let (database, _clock) = database_with_sessions().await;
    let worktrees = database.task_worktrees();
    worktrees
        .reserve_task_worktree(&worktree("session", "writer"), "base")
        .await
        .expect("first reservation");

    // Act
    let results = [
        worktrees
            .reserve_task_worktree(&worktree("missing", "writer"), "base")
            .await,
        worktrees
            .reserve_task_worktree(&worktree("session", "writer"), "base")
            .await,
        worktrees
            .reserve_task_worktree(&worktree("session", " "), "base")
            .await,
        worktrees
            .reserve_task_worktree(&worktree("other", "writer"), " ")
            .await,
    ];

    // Assert
    for result in results {
        assert!(result.is_err(), "{result:?}");
    }
    assert_eq!(
        worktrees
            .list_task_worktrees("other")
            .await
            .expect("listing"),
        Vec::<TaskWorktreeRow>::new()
    );
}

#[tokio::test]
async fn reservations_reject_identifiers_that_derive_taken_names() {
    // Arrange
    let (database, _clock) = database_with_sessions().await;
    let worktrees = database.task_worktrees();
    worktrees
        .reserve_task_worktree(&worktree("session", "writer-alpha"), "base")
        .await
        .expect("first reservation");

    // Act
    let shared_prefix = worktrees
        .reserve_task_worktree(&worktree("session", "writer-also"), "base")
        .await
        .expect("name outcome");
    let other_session = worktrees
        .reserve_task_worktree(&worktree("other", "writer-also"), "base")
        .await
        .expect("independent session");

    // Assert: both identifiers shorten to `writer-a`.
    assert_eq!(shared_prefix, TaskWorktreeReservation::NameTaken);
    assert_eq!(other_session, TaskWorktreeReservation::Reserved);
    assert_eq!(
        worktrees
            .list_task_worktrees("session")
            .await
            .expect("listing"),
        [reserved_row("session", "writer-alpha", "base", 100)]
    );
}

#[tokio::test]
async fn removal_wins_over_a_later_ready_transition() {
    // Arrange
    let (database, clock) = database_with_sessions().await;
    let worktrees = database.task_worktrees();
    let ready = worktree("session", "ready");
    let removed = worktree("session", "removed");
    for id in [&ready, &removed] {
        worktrees
            .reserve_task_worktree(id, "base")
            .await
            .expect("reservation");
    }

    // Act
    clock.store(300, Ordering::SeqCst);
    let marked_ready = worktrees
        .mark_task_worktree_ready(&ready)
        .await
        .expect("ready transition");
    let began_removal = worktrees
        .begin_task_worktree_removal(&removed)
        .await
        .expect("removal transition");
    let ready_after_removal = worktrees
        .mark_task_worktree_ready(&removed)
        .await
        .expect("rejected ready transition");
    let retried_removal = worktrees
        .begin_task_worktree_removal(&removed)
        .await
        .expect("retried removal");
    let missing_ready = worktrees
        .mark_task_worktree_ready(&worktree("session", "missing"))
        .await
        .expect("missing ready transition");
    let missing_removal = worktrees
        .begin_task_worktree_removal(&worktree("session", "missing"))
        .await
        .expect("missing removal");
    let listed = worktrees
        .list_task_worktrees("session")
        .await
        .expect("listing");

    // Assert
    assert!(marked_ready);
    assert!(began_removal);
    assert!(!ready_after_removal);
    assert!(retried_removal);
    assert!(!missing_ready);
    assert!(!missing_removal);
    assert_eq!(
        listed
            .iter()
            .map(|row| (row.id.worktree_id.as_str(), row.state))
            .collect::<Vec<_>>(),
        [
            ("ready", TaskWorktreeState::Ready),
            ("removed", TaskWorktreeState::Removing),
        ]
    );
    assert_eq!((listed[1].created_at, listed[1].updated_at), (100, 300));
}

#[tokio::test]
async fn deleting_records_and_sessions_frees_the_limit() {
    // Arrange
    let (database, _clock) = database_with_sessions().await;
    let worktrees = database.task_worktrees();
    for id in ["first", "second"] {
        worktrees
            .reserve_task_worktree(&worktree("session", id), "base")
            .await
            .expect("session reservation");
    }
    worktrees
        .reserve_task_worktree(&worktree("other", "first"), "base")
        .await
        .expect("other reservation");

    // Act
    worktrees
        .delete_task_worktree(&worktree("session", "first"))
        .await
        .expect("record deletion");
    worktrees
        .delete_task_worktree(&worktree("session", "missing"))
        .await
        .expect("missing record deletion");
    let freed = worktrees
        .reserve_task_worktree(&worktree("session", "third"), "base")
        .await
        .expect("freed reservation");
    database
        .sessions()
        .delete_session("other")
        .await
        .expect("session deletion");

    // Assert
    assert_eq!(freed, TaskWorktreeReservation::Reserved);
    assert_eq!(
        worktrees
            .list_task_worktrees("session")
            .await
            .expect("session listing")
            .iter()
            .map(|row| row.id.worktree_id.as_str())
            .collect::<Vec<_>>(),
        ["second", "third"]
    );
    assert_eq!(
        worktrees
            .list_task_worktrees("other")
            .await
            .expect("cascaded listing"),
        Vec::<TaskWorktreeRow>::new()
    );
}

/// Opens a database with the `session` and `other` sessions already
/// persisted, and returns the clock it reads, which starts at `100` and
/// moves only when a test stores a new time.
async fn database_with_sessions() -> (Database, Arc<AtomicI64>) {
    let clock = Arc::new(AtomicI64::new(100));
    let source = Arc::clone(&clock);
    let database =
        Database::in_memory_with_timestamp_source(Arc::new(move || source.load(Ordering::SeqCst)))
            .await
            .expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    for session in ["session", "other"] {
        database
            .sessions()
            .insert_session(session, "gpt-6.1-sol", "main", "Review", project)
            .await
            .expect("session");
    }

    (database, clock)
}

fn worktree(session_id: &str, worktree_id: &str) -> TaskWorktreeId {
    TaskWorktreeId {
        session_id: session_id.into(),
        worktree_id: worktree_id.to_string(),
    }
}

/// Builds the record of a freshly reserved worktree.
fn reserved_row(
    session_id: &str,
    worktree_id: &str,
    base_commit: &str,
    created_at: i64,
) -> TaskWorktreeRow {
    TaskWorktreeRow {
        base_commit: base_commit.to_string(),
        created_at,
        id: worktree(session_id, worktree_id),
        state: TaskWorktreeState::Creating,
        updated_at: created_at,
    }
}
