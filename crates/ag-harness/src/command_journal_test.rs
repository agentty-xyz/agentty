//! Command journal unit tests for outcome decoding, unknown-command blocking,
//! owner reconciliation, and duplicate request classification.

#[cfg(feature = "sqlite")]
use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use crate::bash::{
    CommandCleanupScope, CommandExit, CommandIntent, CommandOutcome, CommandTermination,
};
use crate::reservation::tests::fixture::AcquiredTurnFixture as _;
#[cfg(feature = "sqlite")]
use crate::store::SqliteStore;
use crate::store::{AcquiredTurn, MemoryStore, NewSession, SessionStore, TurnOwner};
use crate::{OutputSchema, SessionError, TurnInput, TurnOptions};

/// Returns an empty memory store and an empty in-memory SQLite store.
#[cfg(feature = "sqlite")]
async fn command_stores() -> Vec<Arc<dyn SessionStore>> {
    vec![
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("SQLite"),
        ),
    ]
}

/// Returns an empty memory store, the only command store without the
/// `sqlite` feature.
#[cfg(not(feature = "sqlite"))]
fn command_stores() -> std::future::Ready<Vec<Arc<dyn SessionStore>>> {
    std::future::ready(vec![Arc::new(MemoryStore::new())])
}

fn outcome() -> CommandOutcome {
    CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        execution_failure: None,
        exit: CommandExit::new(Some(0), None),
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: "observed".into(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        truncated: false,
    }
}

#[test]
fn prefix_only_outcomes_recorded_before_head_and_tail_capture_still_decode() {
    // Arrange
    let recorded = json!({
        "cleanup_failed": false,
        "cleanup_scope": "process_group_best_effort",
        "execution_failure": null,
        "exit_code": 0,
        "signal": null,
        "stderr": "",
        "stdout": "observed",
        "termination": "completed",
        "truncated": false,
    });

    // Act
    let decoded: CommandOutcome = serde_json::from_value(recorded).expect("legacy outcome");

    // Assert
    assert_eq!(decoded, outcome());
}

#[test]
fn exit_fields_stay_top_level_and_pipe_status_is_encoded_only_for_a_pipeline() {
    // Arrange
    let plain = outcome();
    let mut piped = outcome();
    piped.exit = CommandExit::new(Some(0), None).with_pipe_status(vec![101, 0]);

    // Act
    let plain_encoded = serde_json::to_value(&plain).expect("plain outcome");
    let piped_encoded = serde_json::to_value(&piped).expect("piped outcome");

    // Assert
    assert_eq!(
        (&plain_encoded["exit_code"], &plain_encoded["signal"]),
        (&json!(0), &json!(null))
    );
    assert!(
        plain_encoded.get("pipe_status").is_none(),
        "{plain_encoded}"
    );
    assert!(plain_encoded.get("exit").is_none(), "{plain_encoded}");
    assert_eq!(piped_encoded["pipe_status"], json!([101, 0]));
    assert_eq!(
        serde_json::from_value::<CommandOutcome>(piped_encoded).expect("decoded"),
        piped
    );
}

#[test]
fn command_exits_describe_the_exit_signal_and_pipeline() {
    for (exit, expected) in [
        (CommandExit::new(Some(101), None), "exit 101"),
        (
            CommandExit::new(Some(0), None).with_pipe_status(vec![101, 0]),
            "exit 0, pipeline 101 0",
        ),
        (CommandExit::new(None, Some(9)), "signal 9"),
        (CommandExit::default(), ""),
        (
            CommandExit::new(None, Some(9)).with_termination(CommandTermination::Deadline),
            "timed out, signal 9",
        ),
        (
            CommandExit::new(Some(130), None).with_termination(CommandTermination::Cancelled),
            "cancelled, exit 130",
        ),
        (
            CommandExit::default().with_termination(CommandTermination::Failed),
            "failed to run",
        ),
    ] {
        // Act
        let described = exit.to_string();

        // Assert
        assert_eq!(described, expected);
    }
}

#[tokio::test]
async fn both_stores_block_unknown_commands_and_reconcile_only_the_original_owner() {
    // Arrange
    for store in command_stores().await {
        let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
        let options = TurnOptions::from_output(schema.clone());
        store
            .create_session(&NewSession::new("commands", schema), None, 1024)
            .await
            .expect("session");
        let first = AcquiredTurn::begin(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("first"),
            &options,
            0,
        )
        .await
        .expect("first");
        let owner = first.owner().clone();
        let intent = CommandIntent {
            call_id: "call".into(),
            command: "echo original".into(),
            policy: json!({"revision":"1"}),
            workspace: "/workspace".into(),
        };
        let id = store.command_intent(&owner, &intent).await.expect("intent");

        // Act
        assert!(store.reconcile_command(&owner, id).await.is_err());
        store.interrupt(&owner).await.expect("interrupt");
        let blocked = AcquiredTurn::begin(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("second"),
            &options,
            0,
        )
        .await;
        let records = store.load_commands("commands").await.expect("records");
        let stale = TurnOwner::new(
            store.identity().clone(),
            "commands".into(),
            owner.turn_position(),
            b"wrong-token".to_vec(),
        );
        assert!(store.finish_command(&stale, id, &outcome()).await.is_err());
        assert!(store.reconcile_command(&stale, id).await.is_err());
        store
            .reconcile_command(&owner, id)
            .await
            .expect("explicit reconciliation");
        let second = AcquiredTurn::begin(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("second"),
            &options,
            0,
        )
        .await
        .expect("successor");
        store
            .finish_command(&owner, id, &outcome())
            .await
            .expect("late original outcome");
        store
            .finish_command(&owner, id, &outcome())
            .await
            .expect("identical retry");
        let mut conflicting = outcome();
        conflicting.exit.exit_code = Some(7);
        let conflict = store.finish_command(&owner, id, &conflicting).await;

        // Assert
        assert!(matches!(blocked, Err(SessionError::Busy { .. })));
        assert_eq!(records.len(), 1);
        assert!(records[0].blocks_admission());
        assert_eq!(records[0].intent, intent);
        assert!(records[0].outcome.is_none());
        assert!(conflict.is_err());
        assert_ne!(second.owner(), &owner);
        let records = store.load_commands("commands").await.expect("records");
        assert!(records[0].reconciled);
        assert_eq!(records[0].outcome, Some(outcome()));
        assert!(!records[0].blocks_admission());
        assert!(store.command_intent(&owner, &intent).await.is_err());
        store
            .interrupt(second.owner())
            .await
            .expect("settle successor");
    }
}

#[tokio::test]
async fn unresolved_cleanup_remains_blocking_after_outcome_recording() {
    // Arrange
    for store in command_stores().await {
        let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
        let options = TurnOptions::from_output(schema.clone());
        store
            .create_session(&NewSession::new("commands", schema), None, 1024)
            .await
            .expect("session");
        let acquired = AcquiredTurn::begin(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("run"),
            &options,
            0,
        )
        .await
        .expect("acquire");
        let mut result = outcome();
        result.cleanup_failed = true;
        let id = store
            .command_intent(
                acquired.owner(),
                &CommandIntent {
                    call_id: "call".into(),
                    command: "command".into(),
                    policy: json!({}),
                    workspace: "/workspace".into(),
                },
            )
            .await
            .expect("intent");

        // Act
        store
            .finish_command(acquired.owner(), id, &result)
            .await
            .expect("outcome");
        store.interrupt(acquired.owner()).await.expect("interrupt");
        let blocked = AcquiredTurn::begin(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("next"),
            &options,
            0,
        )
        .await;

        // Assert
        assert!(matches!(blocked, Err(SessionError::Busy { .. })));
        assert!(store.load_commands("commands").await.expect("records")[0].blocks_admission());
        assert_eq!(store.load_writes("commands").await.expect("writes"), []);
    }
}

#[tokio::test]
async fn duplicate_requests_classify_before_pending_command_admission() {
    // Arrange
    for store in command_stores().await {
        let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
        let options = TurnOptions::from_output(schema.clone());
        store
            .create_session(&NewSession::new("commands", schema), None, 1024)
            .await
            .expect("session");
        let request = crate::recovery::HostRequest::from_configuration(
            "host-id".into(),
            json!({"prompt":"run"}),
        )
        .expect("request");
        let crate::recovery::HostTurnAcquisition::Acquired(acquired) = AcquiredTurn::begin_request(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("run"),
            &options,
            &request,
            0,
        )
        .await
        .expect("acquire") else {
            std::panic::resume_unwind(Box::new("expected acquisition"));
        };
        store
            .command_intent(
                acquired.owner(),
                &CommandIntent {
                    call_id: "call".into(),
                    command: "command".into(),
                    policy: json!({}),
                    workspace: "/workspace".into(),
                },
            )
            .await
            .expect("intent");

        // Act
        let duplicate = AcquiredTurn::begin_request(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("run"),
            &options,
            &request,
            0,
        )
        .await
        .expect("duplicate");

        // Assert
        let crate::recovery::HostTurnAcquisition::Recorded(record) = duplicate else {
            std::panic::resume_unwind(Box::new("duplicate must not execute"));
        };
        assert_eq!(record.commands.len(), 1);
        assert!(record.commands[0].blocks_admission());
        assert_eq!(record.status, crate::recovery::HostTurnStatus::InProgress);
        store
            .interrupt(acquired.owner())
            .await
            .expect("settle owner");
    }
}
