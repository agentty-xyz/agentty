use std::sync::Arc;
use std::time::Duration;

use ag_harness::{CommandTermination, MemoryStore, SessionStore, SqliteStore, TurnError};

#[cfg(target_os = "macos")]
use super::fixture::wait_file;
use super::fixture::{Workspace, fault_store, schema};
#[tokio::test]
async fn native_durable_commands_survive_reopen_and_host_retries_do_not_spawn() {
    // Arrange
    let workspace = Workspace::new();
    let storage = tempfile::tempdir().expect("storage");
    let path = storage.path().join("commands.sqlite");
    let stores: Vec<Arc<dyn SessionStore>> = vec![
        Arc::new(MemoryStore::new()),
        Arc::new(SqliteStore::open(&path).await.expect("SQLite")),
    ];
    for (index, store) in stores.into_iter().enumerate() {
        let harness = workspace.harness().store(Arc::clone(&store));
        let mut session = harness
            .session("session", schema())
            .create()
            .await
            .expect("session");
        let options = workspace.options(Duration::from_secs(5), 128);
        let command = "printf x >> output/executions; printf done";

        // Act
        let first = session
            .submit("command-id", command, options.clone())
            .await
            .expect("first command");
        drop(session);
        drop(harness);
        let store: Arc<dyn SessionStore> = if index == 1 {
            drop(store);
            Arc::new(SqliteStore::open(&path).await.expect("reopen SQLite"))
        } else {
            store
        };
        let harness = workspace.harness().store(Arc::clone(&store));
        let mut session = harness
            .resume("session")
            .await
            .expect("resume Bash history");
        let duplicate = session
            .submit("command-id", command, options.clone())
            .await
            .expect("duplicate");
        let records = session.commands().await.expect("command records");
        let recovered = session
            .recover("command-id")
            .await
            .expect("recovery")
            .expect("record");

        // Assert
        assert_eq!(first, duplicate);
        assert_eq!(records.len(), 1);
        assert_eq!(session.writes().await.expect("writes"), []);
        assert_eq!(recovered.commands, records);
        assert!(!records[0].blocks_admission());
        assert_eq!(
            records[0].outcome.as_ref().expect("outcome").stdout,
            "done",
            "{records:?}"
        );
        session
            .submit(
                "next-id",
                "printf y >> output/executions; printf next",
                options.clone(),
            )
            .await
            .expect("new turn after reopening Bash history");
        let retry = session
            .submit("command-id", command, options)
            .await
            .expect("old ID after another turn");
        assert_eq!(retry, first);
        assert_eq!(session.commands().await.expect("two commands").len(), 2);
        let changed = workspace.options(Duration::from_secs(6), 128);
        assert!(matches!(
            session.submit("command-id", command, changed).await,
            Err(ag_harness::SessionError::HostTurnConflict)
        ));
    }
    let reopened = SqliteStore::open(&path).await.expect("reopen");
    assert_eq!(
        reopened
            .load_commands("session")
            .await
            .expect("reopened commands")
            .len(),
        2
    );
    // The read-only Linux policy denies the redirect, so only macOS observes
    // one filesystem effect per executed command.
    #[cfg(target_os = "macos")]
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("output/executions")).expect("effects"),
        "xyxy"
    );
    #[cfg(target_os = "linux")]
    assert!(!workspace.path().join("output/executions").exists());
}

#[tokio::test]
async fn native_journal_failures_prevent_spawn_or_retain_observed_effects() {
    // Arrange
    let workspace = Workspace::new();
    let storage = tempfile::tempdir().expect("storage");
    let path = storage.path().join("commands.sqlite");
    let (store, pool) = fault_store(&path).await;
    let harness = workspace.harness().store(store);
    let mut session = harness
        .session("faults", schema())
        .create()
        .await
        .expect("session");
    let options = workspace.options(Duration::from_secs(5), 128);
    let command = "printf x >> output/executions; printf observed";
    sqlx::query(
        "CREATE TRIGGER fail_intent BEFORE INSERT ON session_command BEGIN SELECT RAISE(FAIL, \
         'injected intent failure'); END",
    )
    .execute(&pool)
    .await
    .expect("intent fault");

    // Act
    let denied = session.submit("intent", command, options.clone()).await;
    let spawned = workspace.path().join("output/executions").exists();
    let records = session.commands().await.expect("no intents");
    sqlx::query("DROP TRIGGER fail_intent")
        .execute(&pool)
        .await
        .expect("remove intent fault");
    sqlx::query(
        "CREATE TRIGGER fail_outcome BEFORE UPDATE OF outcome ON session_command BEGIN SELECT \
         RAISE(FAIL, 'injected outcome failure'); END",
    )
    .execute(&pool)
    .await
    .expect("outcome fault");
    let turn = session
        .submit_controlled("outcome", command, options.clone())
        .expect("turn");
    let control = turn.control();
    let failed = turn.await;
    control
        .settled()
        .await
        .expect("persistence independently settled");
    let settlement = control.commands_settled().await;
    let pending = session.commands().await.expect("pending intent");
    let blocked = session.submit("successor", command, options.clone()).await;
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(&pool)
        .await
        .expect("remove outcome fault");
    control
        .retry_commands()
        .await
        .expect("retry original recording");
    control.commands_settled().await.expect("settled command");
    let recorded = session.commands().await.expect("observed outcome");
    let duplicate = session.submit("outcome", command, options.clone()).await;
    session
        .submit("successor", "printf next", options)
        .await
        .expect("successor admission");
    control.retry_commands().await.expect("stale retry");

    // Assert
    assert!(matches!(
        denied,
        Err(ag_harness::SessionError::Turn(TurnError::CommandJournal {
            outcome: None,
            ..
        }))
    ));
    assert!(!spawned);
    assert_eq!(records, []);
    assert!(matches!(
        failed,
        Err(ag_harness::SessionError::Turn(TurnError::CommandJournal {
            outcome: Some(_),
            ..
        }))
    ));
    assert!(settlement.is_err());
    assert_eq!(pending.len(), 1);
    assert!(pending[0].outcome.is_none());
    assert!(matches!(
        blocked,
        Err(ag_harness::SessionError::Busy { .. })
    ));
    assert_eq!(
        recorded[0].outcome.as_ref().expect("outcome").stdout,
        "observed"
    );
    assert_eq!(
        control.command_outcomes(),
        vec![recorded[0].outcome.clone()]
    );
    assert!(duplicate.is_err(), "failed host requests never rerun");
    #[cfg(target_os = "macos")]
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("output/executions"))
            .expect("actual effects"),
        "x"
    );
}

#[tokio::test]
async fn native_lease_loss_cancels_execution_and_records_late_outcome() {
    // Arrange
    let workspace = Workspace::new();
    let storage = tempfile::tempdir().expect("storage");
    let path = storage.path().join("lease.sqlite");
    let (store, pool) = fault_store(&path).await;
    let harness = workspace.harness().store(store);
    let mut session = harness
        .session("lease", schema())
        .create()
        .await
        .expect("session");
    let turn = session
        .submit_controlled(
            "owner",
            "printf ready > output/ready; /bin/sleep 180",
            workspace.options(Duration::from_secs(300), 128),
        )
        .expect("controlled submission");
    let control = turn.control();
    let mut turn = Box::pin(turn);
    #[cfg(target_os = "macos")]
    {
        let ready = workspace.path().join("output/ready");
        tokio::select! {
            () = wait_file(&ready) => {},
            result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected early turn: {result:?}"))),
        }
    }
    // The read-only Linux policy leaves no observable marker; wait for the
    // committed acquisition, then allow a bounded start window.
    #[cfg(target_os = "linux")]
    tokio::select! {
        acquired = tokio::time::timeout(Duration::from_secs(5), async {
            while sqlx::query("SELECT 1 FROM session_turn WHERE session_id = 'lease'")
                .fetch_optional(&pool)
                .await
                .expect("acquisition probe")
                .is_none()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }) => acquired.expect("committed acquisition"),
        result = &mut turn => std::panic::resume_unwind(Box::new(format!("unexpected early turn: {result:?}"))),
    }

    // Act
    sqlx::query("UPDATE session_turn SET lease_expires_at = 0 WHERE session_id = 'lease'")
        .execute(&pool)
        .await
        .expect("expire ownership");
    let failure = tokio::time::timeout(Duration::from_secs(110), &mut turn)
        .await
        .expect("renewal failure");
    drop(turn);
    control.settled().await.expect("persistence settlement");
    control.commands_settled().await.expect("command cleanup");
    let records = session.commands().await.expect("late record");

    // Assert
    assert!(matches!(
        failure,
        Err(ag_harness::SessionError::OwnershipLost { .. })
    ));
    assert_eq!(records.len(), 1);
    let outcome = records[0]
        .outcome
        .as_ref()
        .expect("outcome after expired lease");
    assert_eq!(outcome.termination, CommandTermination::Cancelled);
    assert!(!outcome.cleanup_failed);
    assert!(!records[0].blocks_admission());
}
