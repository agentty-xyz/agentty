use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;

use crate::command_settlement::{Commands, retained};
use crate::effect::Effects;
use crate::execution::{ExecutionControl, ExecutionError};
use crate::store_conformance_test::{options, schema};
use crate::{
    CommandCleanupScope, CommandIntent, CommandOutcome, CommandTermination, MemoryStore,
    NewSession, OutputSchema, SessionStore, SqliteStore, ToolPolicy, TurnLimits, TurnOptions,
    store_coordinator,
};

struct Control {
    calls: AtomicUsize,
    failure: AtomicBool,
}

#[async_trait]
impl ExecutionControl for Control {
    fn cancel(&self) {}

    async fn cleanup(&self) -> Result<(), ExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.failure.load(Ordering::SeqCst) {
            return Err(ExecutionError::Cleanup);
        }

        Ok(())
    }
}

fn outcome(cleanup_failed: bool) -> CommandOutcome {
    CommandOutcome {
        cleanup_failed,
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        execution_failure: None,
        exit_code: Some(0),
        signal: None,
        stderr: String::new(),
        stdout: String::new(),
        termination: CommandTermination::Completed,
        truncated: false,
    }
}

#[tokio::test]
async fn command_completion_is_separate_from_filesystem_settlement_and_keeps_admission() {
    // Arrange
    let effects = Effects::default();
    let turn = effects.retain();
    let admission = Arc::new(Mutex::new(()));
    effects.admit(Arc::new(Arc::clone(&admission).lock_owned().await));
    let commands = effects.commands().clone();
    let worker = commands.retain();
    let control = Arc::new(Control {
        calls: AtomicUsize::new(0),
        failure: AtomicBool::new(true),
    });
    let operation = commands.register(control.clone(), None);
    operation.admitted(None);

    // Act
    drop(turn);
    effects.settled().await.expect("filesystem settlement");
    let early = tokio::time::timeout(Duration::from_millis(5), commands.settled()).await;
    operation.observed(outcome(true));
    drop(worker);
    let unresolved = commands.settled().await;
    let retry = commands.retry().await;
    control.failure.store(false, Ordering::SeqCst);
    commands.retry().await.expect("cleanup recovery");
    commands.retry().await.expect("stale retry");

    // Assert
    assert!(early.is_err());
    assert!(unresolved.is_err());
    assert!(retry.is_err());
    assert_eq!(control.calls.load(Ordering::SeqCst), 2);
    assert!(commands.settled().await.is_ok());
    assert!(admission.try_lock().is_ok());
}

#[tokio::test]
async fn active_commands_cannot_be_retried_or_reported_settled() {
    // Arrange
    let commands = Commands::default();
    let worker = commands.retain();

    // Act
    let retry = commands.retry().await;
    drop(worker);

    // Assert
    assert!(retry.is_err());
    assert!(commands.settled().await.is_ok());
}

#[tokio::test]
async fn cleanup_retry_releases_retained_controls_without_another_command() {
    // Arrange
    let commands = Commands::default();
    let worker = commands.retain();
    let control = Arc::new(Control {
        calls: AtomicUsize::new(0),
        failure: AtomicBool::new(false),
    });
    let weak = Arc::downgrade(&control);
    let operation = commands.register(control, None);
    operation.admitted(None);
    operation.observed(outcome(true));
    drop(operation);
    drop(worker);
    assert!(weak.upgrade().is_some());

    // Act
    commands.retry().await.expect("cleanup recovery");
    drop(commands);

    // Assert
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn reconciliation_releases_retained_store_and_control_after_caller_drop() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let weak_store = Arc::downgrade(&store);
    let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
    let options = TurnOptions::new(schema.clone(), ToolPolicy::default(), TurnLimits::default());
    store
        .create_session(&NewSession::new("retained", schema), None, 1024)
        .await
        .expect("session");
    let acquired = store
        .begin_turn(Arc::clone(&store), "retained", "run", &options, 0)
        .await
        .expect("turn");
    let journal = acquired.guard.write_journal();
    let id = journal
        .command_intent(&CommandIntent {
            call_id: "call".into(),
            command: "command".into(),
            policy: json!({}),
            workspace: "/workspace".into(),
        })
        .await
        .expect("intent");
    let commands = Commands::default();
    let worker = commands.retain();
    let control = Arc::new(Control {
        calls: AtomicUsize::new(0),
        failure: AtomicBool::new(true),
    });
    let weak_control = Arc::downgrade(&control);
    let operation = commands.register(control, Some(journal));
    operation.admitted(Some(id));
    operation.observed(outcome(true));
    drop(operation);
    drop(worker);
    drop(commands);
    store.interrupt(acquired.owner()).await.expect("interrupt");
    let record = store
        .load_commands("retained")
        .await
        .expect("records")
        .remove(0);
    store
        .reconcile_command(record.owner(), id)
        .await
        .expect("reconcile");
    // Disarm ownership before dropping the guard so no async cleanup retains
    // the store.
    let mut acquired = acquired;
    acquired.guard.disarm();
    drop(acquired);
    tokio::task::yield_now().await;
    drop(store);
    assert!(weak_control.upgrade().is_some());
    assert!(weak_store.upgrade().is_some());

    // Act
    Commands::reconcile(&record);

    // Assert
    assert!(weak_control.upgrade().is_none());
    assert!(weak_store.upgrade().is_none());
}

#[tokio::test]
async fn journal_retry_preserves_failure_and_requires_stopped_owner_for_reconciliation() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("commands.db");
    let store: Arc<dyn SessionStore> = Arc::new(SqliteStore::open(&path).await.expect("store"));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
        .await
        .expect("fault connection");
    store
        .create_session(&NewSession::new("retry", schema()), None, 1024)
        .await
        .expect("session");
    let effects = Effects::default();
    let commands = effects.commands().clone();
    let mut acquired = store_coordinator::acquire(
        Arc::clone(&store),
        ("retry".into(), 0),
        "run".into(),
        options(),
        None,
        effects,
    )
    .await
    .expect("acquire");
    let journal = acquired.guard.write_journal();
    let id = journal
        .command_intent(&CommandIntent {
            call_id: "call".into(),
            command: "effect".into(),
            policy: json!({}),
            workspace: Path::new("/workspace").into(),
        })
        .await
        .expect("intent");
    let worker = commands.retain();
    let operation = commands.register(
        Arc::new(Control {
            calls: AtomicUsize::new(0),
            failure: AtomicBool::new(false),
        }),
        Some(journal),
    );
    operation.admitted(Some(id));
    operation.observed(outcome(true));
    drop(worker);
    sqlx::query(
        "CREATE TRIGGER fail_outcome BEFORE UPDATE OF outcome ON session_command BEGIN SELECT \
         RAISE(FAIL, 'outcome unavailable'); END",
    )
    .execute(&pool)
    .await
    .expect("fault");

    // Act / Assert
    assert!(
        commands.retry().await.is_err(),
        "persistence failure stays unresolved"
    );
    assert!(
        store.load_commands("retry").await.expect("records")[0]
            .outcome
            .is_none()
    );
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(&pool)
        .await
        .expect("repair");
    assert!(
        commands.retry().await.is_err(),
        "live owner cannot reconcile"
    );
    assert_eq!(
        store.load_commands("retry").await.expect("records")[0].outcome,
        Some(outcome(true))
    );
    store.interrupt(acquired.owner()).await.expect("interrupt");
    acquired.guard.disarm();
    commands
        .retry()
        .await
        .expect("cleanup and journal reconciled");
    commands.retry().await.expect("stale retry");
    commands.settled().await.expect("settled");
    let record = store
        .load_commands("retry")
        .await
        .expect("records")
        .remove(0);
    assert!(record.reconciled);
    assert_eq!(
        record.outcome,
        Some(outcome(true)),
        "original diagnostic is retained"
    );
    pool.close().await;
}

#[tokio::test]
async fn reconciling_one_owner_does_not_release_another_owner() {
    // Arrange
    let mut turns = Vec::new();
    let mut tracked = Vec::new();
    for _ in 0..2 {
        let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
        store
            .create_session(&NewSession::new("scoped", schema()), None, 1024)
            .await
            .expect("session");
        let turn = store
            .begin_turn(Arc::clone(&store), "scoped", "run", &options(), 0)
            .await
            .expect("turn");
        let journal = turn.guard.write_journal();
        let id = journal
            .command_intent(&CommandIntent {
                call_id: "call".into(),
                command: "effect".into(),
                policy: json!({}),
                workspace: "/workspace".into(),
            })
            .await
            .expect("intent");
        let commands = Commands::default();
        let lease = commands.retain();
        let operation = commands.register(
            Arc::new(Control {
                calls: AtomicUsize::new(0),
                failure: AtomicBool::new(false),
            }),
            Some(journal),
        );
        operation.admitted(Some(id));
        operation.observed(outcome(true));
        drop(lease);
        store.interrupt(turn.owner()).await.expect("interrupt");
        let record = store
            .load_commands("scoped")
            .await
            .expect("records")
            .remove(0);
        turns.push(turn);
        tracked.push((store, commands, record));
    }

    // Act / Assert
    for index in 0..2 {
        let (store, commands, record) = &tracked[index];
        store
            .reconcile_command(record.owner(), record.id)
            .await
            .expect("reconcile");
        Commands::reconcile(record);
        commands.settled().await.expect("matching owner settled");
        if index == 0 {
            assert!(
                tracked[1].1.settled().await.is_err(),
                "same numeric ID in another store remains unresolved"
            );
        }
    }
    for mut turn in turns {
        turn.guard.disarm();
    }
}

#[test]
fn completion_racing_retention_does_not_keep_a_settled_control_alive() {
    // Arrange
    let commands = Commands::default();
    let lease = commands.retain();
    let control = Arc::new(Control {
        calls: AtomicUsize::new(0),
        failure: AtomicBool::new(false),
    });
    let weak = Arc::downgrade(&control);
    let operation = commands.register(control, None);
    operation.admitted(None);
    let retained = retained().lock().expect("registry");

    // Act
    let worker = std::thread::spawn(move || drop(lease));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while commands.0.borrow().pending != 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    let reached_retention = commands.0.borrow().pending == 0;
    operation.observed(outcome(false));
    drop(retained);
    worker.join().expect("worker");
    drop(operation);
    drop(commands);

    // Assert
    assert!(reached_retention);
    assert!(weak.upgrade().is_none());
}
