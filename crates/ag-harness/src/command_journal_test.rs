use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use crate::{
    CommandCleanupScope, CommandIntent, CommandOutcome, CommandTermination, MemoryStore,
    NewSession, OutputSchema, SessionError, SessionStore, SqliteStore, ToolPolicy, TurnInput,
    TurnLimits, TurnOptions, TurnOwner,
};

fn outcome() -> CommandOutcome {
    CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::ProcessGroupBestEffort,
        execution_failure: None,
        exit_code: Some(0),
        signal: None,
        stderr: String::new(),
        stdout: "observed".into(),
        termination: CommandTermination::Completed,
        truncated: false,
    }
}

#[tokio::test]
async fn both_stores_block_unknown_commands_and_reconcile_only_the_original_owner() {
    // Arrange
    let stores: Vec<Arc<dyn SessionStore>> = vec![
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("SQLite"),
        ),
    ];
    for store in stores {
        let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
        let options =
            TurnOptions::new(schema.clone(), ToolPolicy::default(), TurnLimits::default());
        store
            .create_session(&NewSession::new("commands", schema), None, 1024)
            .await
            .expect("session");
        let first = store
            .begin_turn(
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
        let blocked = store
            .begin_turn(
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
        let second = store
            .begin_turn(
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
        conflicting.exit_code = Some(7);
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
    let stores: Vec<Arc<dyn SessionStore>> = vec![
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("SQLite"),
        ),
    ];
    for store in stores {
        let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
        let options =
            TurnOptions::new(schema.clone(), ToolPolicy::default(), TurnLimits::default());
        store
            .create_session(&NewSession::new("commands", schema), None, 1024)
            .await
            .expect("session");
        let acquired = store
            .begin_turn(
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
        let blocked = store
            .begin_turn(
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
    let stores: Vec<Arc<dyn SessionStore>> = vec![
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("SQLite"),
        ),
    ];
    for store in stores {
        let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
        let options =
            TurnOptions::new(schema.clone(), ToolPolicy::default(), TurnLimits::default());
        store
            .create_session(&NewSession::new("commands", schema), None, 1024)
            .await
            .expect("session");
        let request =
            crate::HostRequest::from_configuration("host-id".into(), json!({"prompt":"run"}))
                .expect("request");
        let crate::HostTurnAcquisition::Acquired(acquired) = store
            .begin_request(
                Arc::clone(&store),
                "commands",
                &TurnInput::from("run"),
                &options,
                &request,
                0,
            )
            .await
            .expect("acquire")
        else {
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
        let duplicate = store
            .begin_request(
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
        let crate::HostTurnAcquisition::Recorded(record) = duplicate else {
            std::panic::resume_unwind(Box::new("duplicate must not execute"));
        };
        assert_eq!(record.commands.len(), 1);
        assert!(record.commands[0].blocks_admission());
        assert_eq!(record.status, crate::HostTurnStatus::InProgress);
        store
            .interrupt(acquired.owner())
            .await
            .expect("settle owner");
    }
}
