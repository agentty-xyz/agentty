//! Public lifecycle and journal contracts for every store backend.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::bash::{CommandCleanupScope, CommandExit, CommandIntent, CommandOutcome};
use ag_harness::event::{SessionId, TurnIndex};
use ag_harness::model::{ModelCompletion, ModelMessage, ModelRequest, ModelResponse};
#[cfg(feature = "sqlite")]
use ag_harness::store::SqliteStore;
use ag_harness::store::{
    AcquiredTurn, CompactionSource, HistoryProjection, MemoryStore, ModelSwitch, NewSession,
    SessionCheckpoint, SessionStore, StoreBackend, StoreIdentity, StoredTurnOptions, TurnOwner,
    TurnUsage, WriteStatus,
};
use ag_harness::tool::ToolCall;
use ag_harness::transcript::TurnStatus;
use ag_harness::{
    ErrorKind, Harness, Model, ModelError, SessionError, TurnError, TurnInput, TurnRange,
};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;

use crate::context::{HistoryTurn, TurnStop};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::model_profile_fixture::unbounded_model_profile;
use crate::recovery::{HostRequest, HostTurnAcquisition, HostTurnStatus};
use crate::reservation::tests::fixture::AcquiredTurnFixture as _;
use crate::store_fixture::{
    Echo, ExternalStore, HistoryTurnFixture as _, harness, image_input, lifecycle, loaded_turn,
    options, schema, stores,
};
use crate::store_gate::Gate;

#[tokio::test]
async fn command_reconciliation_preserves_records_and_validates_session_and_store() {
    // Arrange
    for store in stores().await.into_iter().skip(1) {
        let harness = harness(Arc::clone(&store));
        let session = harness
            .session("commands", schema())
            .create()
            .await
            .expect("session");
        let other_session = harness
            .session("other", schema())
            .create()
            .await
            .expect("other");
        let independent = self::harness(Arc::new(MemoryStore::new()))
            .session("commands", schema())
            .create()
            .await
            .expect("independent");
        let acquired = AcquiredTurn::begin(
            Arc::clone(&store),
            "commands",
            &TurnInput::from("run"),
            &options(),
            0,
        )
        .await
        .expect("owner");
        let intent = CommandIntent {
            call_id: "call".into(),
            command: "effect".into(),
            policy: json!({}),
            workspace: "/workspace".into(),
        };
        store
            .command_intent(acquired.owner(), &intent)
            .await
            .expect("intent");
        let record = session.commands().await.expect("records").remove(0);

        // Act / Assert
        assert!(
            session.reconcile_command(&record).await.is_err(),
            "live owner"
        );
        assert!(
            other_session.reconcile_command(&record).await.is_err(),
            "other session"
        );
        assert!(
            independent.reconcile_command(&record).await.is_err(),
            "other store"
        );
        store.interrupt(acquired.owner()).await.expect("interrupt");
        session
            .reconcile_command(&record)
            .await
            .expect("reconcile stopped owner");
        let records = session.commands().await.expect("records");
        assert_eq!(records[0].intent, intent);
        assert!(records[0].outcome.is_none());
        assert!(records[0].reconciled);
        assert!(!records[0].blocks_admission());
    }
}

#[tokio::test]
async fn external_store_without_command_support_fails_closed() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    store
        .create_session(&NewSession::new("unsupported", schema()), None, 1024)
        .await
        .expect("create");
    let acquired = AcquiredTurn::begin(
        Arc::clone(&store),
        "unsupported",
        &TurnInput::from("run"),
        &options(),
        0,
    )
    .await
    .expect("owner");
    let intent = CommandIntent {
        call_id: "call".into(),
        command: "effect".into(),
        policy: json!({}),
        workspace: "/workspace".into(),
    };
    let outcome = CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::PidNamespace,
        execution_failure: None,
        exit: CommandExit::new(Some(0), None),
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: String::new(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        truncated: false,
    };

    // Act / Assert
    assert!(matches!(
        store.load_commands("unsupported").await,
        Err(SessionError::InvalidData { .. })
    ));
    assert!(matches!(
        store.command_intent(acquired.owner(), &intent).await,
        Err(SessionError::InvalidData { .. })
    ));
    assert!(matches!(
        store.finish_command(acquired.owner(), 1, &outcome).await,
        Err(SessionError::InvalidData { .. })
    ));
    assert!(matches!(
        store.reconcile_command(acquired.owner(), 1).await,
        Err(SessionError::InvalidData { .. })
    ));
    store
        .record_usage(acquired.owner(), &TurnUsage::default())
        .await
        .expect("a store without usage storage records nothing");
    assert!(matches!(
        store.load_usage("unsupported").await,
        Err(SessionError::InvalidData { .. })
    ));
    store.interrupt(acquired.owner()).await.expect("interrupt");
}

#[tokio::test]
async fn backends_record_usage_only_under_a_live_owner() {
    for store in stores().await.into_iter().skip(1) {
        // Arrange
        store
            .create_session(&NewSession::new("usage", schema()), None, 1024)
            .await
            .expect("create");
        let acquired = AcquiredTurn::begin(
            Arc::clone(&store),
            "usage",
            &TurnInput::from("run"),
            &options(),
            0,
        )
        .await
        .expect("owner");
        let mut recorded = TurnUsage::default();
        recorded.usage.input = 7;

        // Act
        store
            .record_usage(acquired.owner(), &recorded)
            .await
            .expect("record usage");
        let while_running = store.load_usage("usage").await.expect("load usage");
        store
            .complete_turn(acquired.owner(), &[], HistoryProjection::default())
            .await
            .expect("complete");
        let after_completion = store
            .record_usage(acquired.owner(), &TurnUsage::default())
            .await;

        // Assert
        assert_eq!(
            while_running,
            Vec::<Option<TurnUsage>>::new(),
            "a running turn is left out"
        );
        assert_eq!(
            store.load_usage("usage").await.expect("load usage"),
            [Some(recorded)]
        );
        assert!(matches!(
            after_completion,
            Err(SessionError::OwnershipLost { .. })
        ));
        assert!(matches!(
            store.load_usage("missing").await,
            Err(SessionError::NotFound { .. })
        ));
    }
}

/// Begins a turn of a new session `id` in `store` and records usage of
/// `input` tokens on it, leaving the turn running.
async fn running_turn_with_usage(
    store: &Arc<dyn SessionStore>,
    id: &str,
    input: u64,
) -> (AcquiredTurn, TurnUsage) {
    store
        .create_session(&NewSession::new(id, schema()), None, 1024)
        .await
        .expect("create");
    let acquired = AcquiredTurn::begin(
        Arc::clone(store),
        id,
        &TurnInput::from("run"),
        &options(),
        0,
    )
    .await
    .expect("owner");
    let mut recorded = TurnUsage::default();
    recorded.usage.input = input;
    store
        .record_usage(acquired.owner(), &recorded)
        .await
        .expect("record usage");

    (acquired, recorded)
}

/// Asserts `store` counts the turn of session `crashed`, whose lease expired,
/// with its `recorded` usage, and settles it as interrupted.
async fn assert_expired_turn_counts_usage(store: &Arc<dyn SessionStore>, recorded: TurnUsage) {
    // Act
    let loaded = store.load_usage("crashed").await.expect("usage");

    // Assert
    assert_eq!(loaded, [Some(recorded)]);
    let transcript = store
        .load_transcript("crashed", &TurnRange::All)
        .await
        .expect("transcript");
    assert_eq!(transcript[0].status, TurnStatus::Interrupted);
}

#[tokio::test]
async fn memory_store_counts_a_turn_whose_lease_expired_with_its_recorded_usage() {
    // Arrange
    let memory: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    let (memory_turn, memory_usage) = running_turn_with_usage(&memory, "crashed", 3).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::time::resume();

    // Act / Assert
    assert_expired_turn_counts_usage(&memory, memory_usage).await;
    drop(memory_turn);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_store_counts_a_turn_whose_lease_expired_with_its_recorded_usage() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("usage.sqlite");
    let sqlite: Arc<dyn SessionStore> = Arc::new(SqliteStore::open(&path).await.expect("sqlite"));
    let (sqlite_turn, sqlite_usage) = running_turn_with_usage(&sqlite, "crashed", 5).await;
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .expect("expiry connection");
    sqlx::query("UPDATE session_turn SET lease_expires_at = 0 WHERE session_id = 'crashed'")
        .execute(&pool)
        .await
        .expect("expire the SQLite lease");

    // Act / Assert
    assert_expired_turn_counts_usage(&sqlite, sqlite_usage).await;
    drop(sqlite_turn);
}

#[tokio::test]
async fn all_backends_satisfy_lifecycle_and_journal_contract() {
    // Arrange / Act / Assert
    for store in stores().await {
        lifecycle(store).await;
    }
}

/// Returns the journaling backends this build includes.
#[cfg(feature = "sqlite")]
async fn journaled_stores() -> Vec<Arc<dyn SessionStore>> {
    vec![
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("sqlite"),
        ),
    ]
}

/// Returns the journaling backends this build includes.
#[cfg(not(feature = "sqlite"))]
fn journaled_stores() -> std::future::Ready<Vec<Arc<dyn SessionStore>>> {
    std::future::ready(vec![Arc::new(MemoryStore::new())])
}

fn write_call(id: &str) -> ToolCall {
    ToolCall::from_json(
        id.to_string(),
        "write",
        &json!({"path": "src/lib.rs", "patch": "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n"})
            .to_string(),
        None,
    )
    .expect("write call")
}

fn failed_stop() -> TurnStop {
    TurnStop {
        error_type: format!(
            "{:?}",
            TurnError::Model(ModelError::InvalidResponse).error_type()
        ),
        failed: true,
        ..TurnStop::default()
    }
}

#[tokio::test]
async fn all_backends_complete_after_appended_exchanges() {
    for store in stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("appended", schema()), None, 100_000)
            .await
            .expect("create");
        let turn = AcquiredTurn::begin(
            Arc::clone(&store),
            "appended",
            &TurnInput::from("done"),
            &options(),
            0,
        )
        .await
        .expect("turn");
        let call = ModelMessage::AssistantToolCall(write_call("call"));
        let result = ModelMessage::ToolResult {
            call_id: "call".to_string(),
            content: "{}".to_string(),
            name: "write".to_string(),
        };
        store
            .append_messages(turn.owner(), std::slice::from_ref(&call))
            .await
            .expect("append call");
        store
            .append_messages(turn.owner(), std::slice::from_ref(&result))
            .await
            .expect("append result");

        // Act
        store
            .complete_turn(
                turn.owner(),
                &[ModelMessage::Assistant("{}".to_string())],
                HistoryProjection::default(),
            )
            .await
            .expect("complete");
        let late = store
            .append_messages(turn.owner(), &[ModelMessage::Assistant("late".to_string())])
            .await;
        drop(turn);

        // Assert
        assert!(matches!(late, Err(SessionError::OwnershipLost { .. })));
        assert_eq!(
            store.load_session("appended").await.expect("history").turns,
            vec![(
                0,
                loaded_turn(vec![
                    ModelMessage::User("done".to_string()),
                    call,
                    result,
                    ModelMessage::Assistant("{}".to_string()),
                ])
            )]
        );
    }
}

#[tokio::test]
async fn journaled_backends_replay_stopped_turn_messages_and_unfinished_effects() {
    for store in journaled_stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("stopped", schema()), None, 100_000)
            .await
            .expect("create");
        let stopped = AcquiredTurn::begin(
            Arc::clone(&store),
            "stopped",
            &TurnInput::from("work"),
            &options(),
            0,
        )
        .await
        .expect("stopped turn");
        let owner = stopped.owner().clone();
        store
            .append_messages(
                &owner,
                &[ModelMessage::AssistantToolCall(write_call("work-call"))],
            )
            .await
            .expect("append call");
        store
            .write_intent(
                &owner,
                "work-call",
                Path::new("repo"),
                "src/lib.rs",
                None,
                b"new",
            )
            .await
            .expect("write intent");
        store
            .command_intent(
                &owner,
                &CommandIntent {
                    call_id: "command".to_string(),
                    command: "cargo test".to_string(),
                    policy: json!({}),
                    workspace: "repo".into(),
                },
            )
            .await
            .expect("command intent");

        // Act
        store
            .fail_turn(&owner, &TurnError::Model(ModelError::InvalidResponse))
            .await
            .expect("fail");
        drop(stopped);
        let loaded = store.load_session("stopped").await.expect("history");

        // Assert
        assert_eq!(loaded.latest_finished_turn, Some(0));
        assert_eq!(
            loaded.turns,
            vec![(
                0,
                loaded_turn(vec![
                    ModelMessage::User("work".to_string()),
                    ModelMessage::AssistantToolCall(write_call("work-call")),
                ])
                .with_stop(TurnStop {
                    unfinished_commands: vec!["cargo test".to_string()],
                    unfinished_writes: vec!["src/lib.rs".to_string()],
                    ..failed_stop()
                })
            )]
        );
    }
}

#[tokio::test]
async fn journaled_backends_keep_running_turn_effects_out_of_stopped_turn_notes() {
    for store in journaled_stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("running", schema()), None, 100_000)
            .await
            .expect("create");
        let stopped = AcquiredTurn::begin(
            Arc::clone(&store),
            "running",
            &TurnInput::from("work"),
            &options(),
            0,
        )
        .await
        .expect("stopped turn");
        store
            .fail_turn(
                stopped.owner(),
                &TurnError::Model(ModelError::InvalidResponse),
            )
            .await
            .expect("fail");
        drop(stopped);
        let running = AcquiredTurn::begin(
            Arc::clone(&store),
            "running",
            &TurnInput::from("next"),
            &options(),
            0,
        )
        .await
        .expect("running turn");
        store
            .write_intent(
                running.owner(),
                "next-call",
                Path::new("repo"),
                "src/main.rs",
                None,
                b"next",
            )
            .await
            .expect("write intent");
        store
            .command_intent(
                running.owner(),
                &CommandIntent {
                    call_id: "next-command".to_string(),
                    command: "cargo build".to_string(),
                    policy: json!({}),
                    workspace: "repo".into(),
                },
            )
            .await
            .expect("command intent");

        // Act
        let loaded = store.load_session("running").await.expect("history");
        drop(running);

        // Assert
        assert_eq!(
            loaded.turns,
            vec![(
                0,
                loaded_turn(vec![ModelMessage::User("work".to_string())]).with_stop(failed_stop())
            )]
        );
    }
}

#[tokio::test]
async fn journaled_backends_keep_only_the_input_of_a_stopped_turn_over_the_byte_budget() {
    for store in journaled_stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("brief", schema()), None, 4)
            .await
            .expect("create");
        let older = AcquiredTurn::begin(
            Arc::clone(&store),
            "brief",
            &TurnInput::from("old"),
            &options(),
            0,
        )
        .await
        .expect("older turn");
        store
            .complete_turn(older.owner(), &[], HistoryProjection::default())
            .await
            .expect("complete");
        drop(older);
        let stopped = AcquiredTurn::begin(
            Arc::clone(&store),
            "brief",
            &TurnInput::from("work"),
            &options(),
            0,
        )
        .await
        .expect("stopped turn");
        store
            .append_messages(
                stopped.owner(),
                &[ModelMessage::AssistantToolCall(write_call("call"))],
            )
            .await
            .expect("append");
        store
            .fail_turn(
                stopped.owner(),
                &TurnError::Model(ModelError::InvalidResponse),
            )
            .await
            .expect("fail");
        drop(stopped);

        // Act
        let loaded = store.load_session("brief").await.expect("history");

        // Assert
        assert_eq!(
            loaded.turns,
            vec![(
                1,
                loaded_turn(vec![ModelMessage::User("work".to_string())]).with_stop(failed_stop())
            )]
        );
    }
}

/// Completes a turn whose tool traffic alone exceeds a small history budget.
async fn complete_tool_heavy_turn(store: &Arc<dyn SessionStore>, id: &str) {
    for (input, messages) in [
        ("old", Vec::new()),
        (
            "work",
            vec![
                ModelMessage::AssistantToolCall(
                    ToolCall::from_json("r1".to_string(), "read", r#"{"path":"big.txt"}"#, None)
                        .expect("read call"),
                ),
                ModelMessage::ToolResult {
                    call_id: "r1".to_string(),
                    content: "x".repeat(5000),
                    name: "read".to_string(),
                },
            ],
        ),
    ] {
        let turn = AcquiredTurn::begin(
            Arc::clone(store),
            id,
            &TurnInput::from(input),
            &options(),
            0,
        )
        .await
        .expect("turn");
        store
            .append_messages(turn.owner(), &messages)
            .await
            .expect("append");
        store
            .complete_turn(
                turn.owner(),
                &[ModelMessage::Assistant("{}".to_string())],
                HistoryProjection::default(),
            )
            .await
            .expect("complete");
    }
}

#[tokio::test]
async fn journaled_backends_keep_an_oversized_completed_turn_elided_or_brief() {
    for store in journaled_stores().await {
        // Arrange
        // At 600 bytes the elided form would fit, but the turn exceeds eight
        // times the limit, so it is never loaded to weigh that form.
        for (id, max_history_bytes) in [("elided", 1000), ("brief", 100), ("bounded", 600)] {
            store
                .create_session(&NewSession::new(id, schema()), None, max_history_bytes)
                .await
                .expect("create");
            complete_tool_heavy_turn(&store, id).await;
        }
        let old = loaded_turn(vec![
            ModelMessage::User("old".to_string()),
            ModelMessage::Assistant("{}".to_string()),
        ]);

        // Act
        let elided = store.load_session("elided").await.expect("history").turns;
        let brief = store.load_session("brief").await.expect("history").turns;
        let bounded = store.load_session("bounded").await.expect("history").turns;

        // Assert
        // The older turn still loads, and the oversized turn keeps its tool
        // group with the result replaced by a cleared notice.
        assert_eq!(elided.len(), 2);
        assert_eq!(elided[0], (0, old.clone()));
        assert_eq!(elided[1].1.messages.len(), 4);
        assert!(matches!(
            &elided[1].1.messages[2],
            ModelMessage::ToolResult { call_id, content, .. }
                if call_id == "r1" && content.starts_with(r#"{"context_cleared":"#)
        ));
        let briefly = vec![
            (0, old),
            (
                1,
                loaded_turn(vec![
                    ModelMessage::User("work".to_string()),
                    ModelMessage::Assistant("{}".to_string()),
                ]),
            ),
        ];
        assert_eq!(brief, briefly);
        assert_eq!(bounded, briefly);
    }
}

/// Completes one turn with `input` and a `{}` answer.
async fn complete_turn(store: &Arc<dyn SessionStore>, id: &str, input: &str) {
    let turn = AcquiredTurn::begin(
        Arc::clone(store),
        id,
        &TurnInput::from(input),
        &options(),
        0,
    )
    .await
    .expect("turn");
    store
        .complete_turn(
            turn.owner(),
            &[ModelMessage::Assistant("{}".to_string())],
            HistoryProjection::default(),
        )
        .await
        .expect("complete");
}

/// Completes one turn with `input` that reads one file of 500 bytes,
/// keeping `projection` as the session's history projection, and returns
/// its messages.
async fn complete_read_turn(
    store: &Arc<dyn SessionStore>,
    id: &str,
    input: &str,
    projection: HistoryProjection,
) -> HistoryTurn {
    complete_sized_read_turn(store, id, (input, 500), projection).await
}

/// Returns the completed turn with `input` that reads one file of `bytes`
/// bytes and answers `{}`.
fn read_turn(input: &str, bytes: usize) -> HistoryTurn {
    let call = ToolCall::from_json(
        format!("read {input}"),
        "read",
        &json!({ "path": format!("{input}.txt") }).to_string(),
        None,
    )
    .expect("read call");

    loaded_turn(vec![
        ModelMessage::User(input.to_string()),
        ModelMessage::AssistantToolCall(call),
        ModelMessage::ToolResult {
            call_id: format!("read {input}"),
            content: "x".repeat(bytes),
            name: "read".to_string(),
        },
        ModelMessage::Assistant("{}".to_string()),
    ])
}

/// Completes [`read_turn`] of `input` and `bytes`, keeping `projection` as
/// the session's history projection, and returns its messages.
async fn complete_sized_read_turn(
    store: &Arc<dyn SessionStore>,
    id: &str,
    (input, bytes): (&str, usize),
    projection: HistoryProjection,
) -> HistoryTurn {
    let completed = read_turn(input, bytes);
    let exchange = &completed.messages[1..3];
    let answer = &completed.messages[3..];
    let turn = AcquiredTurn::begin(
        Arc::clone(store),
        id,
        &TurnInput::from(input),
        &options(),
        0,
    )
    .await
    .expect("turn");
    store
        .append_messages(turn.owner(), exchange)
        .await
        .expect("append");
    store
        .complete_turn(turn.owner(), answer, projection)
        .await
        .expect("complete");

    completed
}

#[tokio::test]
async fn journaled_backends_replay_history_in_the_projection_the_last_turn_kept() {
    for store in journaled_stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("projected", schema()), None, 100_000)
            .await
            .expect("create");
        let mut turns = Vec::new();
        for input in ["zero", "one", "two"] {
            turns.push(
                complete_read_turn(&store, "projected", input, HistoryProjection::default()).await,
            );
        }
        let projection = HistoryProjection::new(1, 2, 3);
        turns.push(complete_read_turn(&store, "projected", "three", projection).await);

        // Act
        let loaded = store.load_session("projected").await.expect("history");
        let reserved = AcquiredTurn::begin(
            Arc::clone(&store),
            "projected",
            &TurnInput::from("four"),
            &options(),
            0,
        )
        .await
        .expect("turn");

        // Assert
        assert_eq!(loaded.projection, projection);
        let positions = loaded
            .turns
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>();
        assert_eq!(positions, [1, 2, 3]);
        assert_eq!(
            loaded.turns[0].1.messages,
            [
                ModelMessage::User("one".to_string()),
                ModelMessage::Assistant("{}".to_string()),
            ]
        );
        assert_eq!(loaded.turns[1].1.messages.len(), 4);
        assert!(matches!(
            &loaded.turns[1].1.messages[2],
            ModelMessage::ToolResult { content, .. } if content.starts_with(r#"{"context_cleared":"#)
        ));
        assert_eq!(loaded.turns[2].1, turns[3]);
        assert_eq!(reserved.owner().turn_position(), 4);
    }
}

#[tokio::test]
async fn journaled_backends_reproject_outgrown_history_no_more_completely_than_before() {
    for store in journaled_stores().await {
        // Arrange
        let retained = |turn: &HistoryTurn| {
            turn.messages
                .iter()
                .map(ModelMessage::retained_bytes)
                .sum::<usize>()
        };
        let mut turns = Vec::new();
        let sizes = [("zero", 300), ("one", 300), ("two", 2000), ("three", 300)];
        // The newest two turns whole outgrow the budget by a byte.
        let budget = retained(&read_turn("two", 2000)) + retained(&read_turn("three", 300)) - 1;
        store
            .create_session(&NewSession::new("capped", schema()), None, budget)
            .await
            .expect("create");
        for (input, bytes) in sizes {
            let projection = if input == "three" {
                HistoryProjection::new(0, 1, 2)
            } else {
                HistoryProjection::default()
            };
            turns
                .push(complete_sized_read_turn(&store, "capped", (input, bytes), projection).await);
        }

        // Act
        let loaded = store.load_session("capped").await.expect("history");

        // Assert
        // Half the budget holds the newest turn whole and the others shorter;
        // the brief and elided turns stay so although their full forms fit.
        assert!(retained(&turns[2]) + retained(&turns[3]) > budget);
        assert_eq!(loaded.projection, HistoryProjection::new(0, 1, 3));
        let positions = loaded
            .turns
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>();
        assert_eq!(positions, [0, 1, 2, 3]);
        assert_eq!(loaded.turns[0].1.messages.len(), 2);
        for (_, elided) in &loaded.turns[1..3] {
            assert_eq!(elided.messages.len(), 4);
            assert!(matches!(
                &elided.messages[2],
                ModelMessage::ToolResult { content, .. }
                    if content.starts_with(r#"{"context_cleared":"#)
            ));
        }
        assert_eq!(loaded.turns[3].1, turns[3]);
    }
}

#[tokio::test]
async fn all_backends_reset_the_history_projection_when_the_model_switches() {
    for store in stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("switched", schema()), None, 100_000)
            .await
            .expect("create");
        let first = complete_read_turn(&store, "switched", "zero", HistoryProjection::default());
        let first = first.await;
        complete_read_turn(&store, "switched", "one", HistoryProjection::new(1, 1, 1)).await;
        let identity = ag_harness::recovery::ExecutionIdentity::new("next", "1").expect("identity");
        let capabilities = ag_harness::model::ModelCapabilities {
            image_input: false,
            profile: unbounded_model_profile(),
            tool_calls: true,
        };
        let projected = store.load_session("switched").await.expect("history");

        // Act
        store
            .switch_model(
                "switched",
                &ModelSwitch::new(identity, None, capabilities, 0),
            )
            .await
            .expect("switch");
        let switched = store.load_session("switched").await.expect("history");

        // Assert
        assert_eq!(projected.turns.len(), 1);
        assert_eq!(switched.projection, HistoryProjection::default());
        assert_eq!(switched.turns.len(), 2);
        assert_eq!(switched.turns[0], (0, first));
    }
}

#[tokio::test]
async fn journaled_backends_load_the_oldest_uncovered_turns_as_the_compaction_source() {
    for store in journaled_stores().await {
        // Arrange
        for id in ["elided", "brief"] {
            store
                .create_session(&NewSession::new(id, schema()), None, 4096)
                .await
                .expect("create");
            complete_tool_heavy_turn(&store, id).await;
            complete_turn(&store, id, "new").await;
        }
        store
            .create_session(&NewSession::new("forced", schema()), None, 4096)
            .await
            .expect("create");
        complete_turn(&store, "forced", &"x".repeat(50)).await;
        complete_turn(&store, "forced", "next").await;
        let failed = AcquiredTurn::begin(
            Arc::clone(&store),
            "forced",
            &TurnInput::from("work"),
            &options(),
            0,
        )
        .await
        .expect("stopped turn");
        store
            .fail_turn(
                failed.owner(),
                &TurnError::Model(ModelError::InvalidResponse),
            )
            .await
            .expect("fail");
        drop(failed);
        let brief = |input: &str| {
            loaded_turn(vec![
                ModelMessage::User(input.to_string()),
                ModelMessage::Assistant("{}".to_string()),
            ])
        };

        // Act
        let elided = store
            .load_compaction_source("elided", 1000)
            .await
            .expect("source");
        let briefly = store
            .load_compaction_source("brief", 100)
            .await
            .expect("source");
        let forced = store
            .load_compaction_source("forced", 4)
            .await
            .expect("source");
        let everything = store
            .load_compaction_source("forced", 4096)
            .await
            .expect("source");

        // Assert
        // Older turns keep their most complete fitting form and the newest
        // turn still follows, unlike replayed history, which keeps the newest.
        let positions = |source: &CompactionSource| -> Vec<i64> {
            source.turns.iter().map(|(position, _)| *position).collect()
        };
        assert_eq!(positions(&elided), [0, 1, 2]);
        assert_eq!(elided.turns[0].1, brief("old"));
        assert_eq!(elided.turns[1].1.messages.len(), 4);
        assert!(matches!(
            &elided.turns[1].1.messages[2],
            ModelMessage::ToolResult { content, .. } if content.starts_with(r#"{"context_cleared":"#)
        ));
        assert_eq!(elided.turns[2].1, brief("new"));
        assert_eq!(positions(&briefly), [0, 1, 2]);
        assert_eq!(briefly.turns[1].1, brief("work"));
        // The oldest turn is kept briefly even beyond the limit, and nothing
        // after it fits.
        assert_eq!(forced.turns, [(0, brief(&"x".repeat(50)))]);
        assert_eq!(positions(&everything), [0, 1, 2]);
        assert_eq!(everything.turns[2].1.stop, Some(failed_stop()));
    }
}

#[tokio::test]
async fn all_backends_load_the_compaction_source_after_the_checkpoint_boundary() {
    for store in stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("source", schema()), None, 4096)
            .await
            .expect("create");
        for input in ["zero", "one", "two"] {
            complete_turn(&store, "source", input).await;
        }
        let checkpoint = SessionCheckpoint::new(
            0,
            0,
            None,
            None,
            json!({"context": "c", "decisions": [], "state": "s"}),
        )
        .expect("checkpoint");
        store
            .publish_checkpoint("source", &checkpoint)
            .await
            .expect("publish");

        // Act
        let source = store
            .load_compaction_source("source", 4096)
            .await
            .expect("source");
        let missing = store
            .load_compaction_source("missing", 4096)
            .await
            .expect("missing source");

        // Assert
        assert_eq!(source.checkpoint, Some(checkpoint));
        let inputs: Vec<(i64, ModelMessage)> = source
            .turns
            .iter()
            .map(|(position, turn)| (*position, turn.messages[0].clone()))
            .collect();
        assert_eq!(
            inputs,
            [
                (1, ModelMessage::User("one".to_string())),
                (2, ModelMessage::User("two".to_string())),
            ]
        );
        assert_eq!(missing, CompactionSource::default());
    }
}

#[tokio::test]
async fn journaled_backends_size_a_brief_turn_with_its_final_reasoning_cleared() {
    for store in journaled_stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("reasoned", schema()), None, 120)
            .await
            .expect("create");
        for (input, final_message) in [
            ("old", ModelMessage::Assistant("{}".to_string())),
            (
                "work",
                ModelMessage::AssistantReasoning {
                    content: "{}".to_string(),
                    reasoning_content: "r".repeat(500),
                },
            ),
        ] {
            let turn = AcquiredTurn::begin(
                Arc::clone(&store),
                "reasoned",
                &TurnInput::from(input),
                &options(),
                0,
            )
            .await
            .expect("turn");
            store
                .append_messages(
                    turn.owner(),
                    &[
                        ModelMessage::AssistantToolCall(
                            ToolCall::from_json("r1".to_string(), "read", r#"{"path":"a"}"#, None)
                                .expect("read call"),
                        ),
                        ModelMessage::ToolResult {
                            call_id: "r1".to_string(),
                            content: "x".repeat(input.len() * 20),
                            name: "read".to_string(),
                        },
                    ],
                )
                .await
                .expect("append");
            store
                .complete_turn(turn.owner(), &[final_message], HistoryProjection::default())
                .await
                .expect("complete");
        }

        // Act
        let loaded = store.load_session("reasoned").await.expect("history");

        // Assert
        // Only the brief form fits the newest turn, and it counts the final
        // reasoning as the cleared placeholder it replays, leaving room for
        // the older turn in brief form too within half the outgrown budget.
        assert_eq!(
            loaded.turns,
            vec![
                (
                    0,
                    loaded_turn(vec![
                        ModelMessage::User("old".to_string()),
                        ModelMessage::Assistant("{}".to_string()),
                    ])
                ),
                (
                    1,
                    loaded_turn(vec![
                        ModelMessage::User("work".to_string()),
                        ModelMessage::AssistantReasoning {
                            content: "{}".to_string(),
                            reasoning_content: "[reasoning cleared to fit the context window]"
                                .to_string(),
                        },
                    ])
                ),
            ]
        );
    }
}

#[tokio::test]
async fn all_backends_preserve_ordered_image_input_in_history() {
    // Arrange
    for store in stores().await {
        let input = image_input("before", b"payload", "after");
        store
            .create_session(&NewSession::new("images", schema()), None, 4096)
            .await
            .expect("create");
        let acquired = AcquiredTurn::begin(Arc::clone(&store), "images", &input, &options(), 0)
            .await
            .expect("begin");

        // Act
        store
            .complete_turn(
                acquired.owner(),
                &[ModelMessage::Assistant("described".to_string())],
                HistoryProjection::default(),
            )
            .await
            .expect("complete");
        drop(acquired);

        // Assert
        let loaded = store.load_session("images").await.expect("load");
        assert_eq!(
            loaded.turns,
            vec![(
                0,
                loaded_turn(vec![
                    ModelMessage::UserInput(input),
                    ModelMessage::Assistant("described".to_string()),
                ])
            )]
        );
    }
}

#[tokio::test]
async fn all_backends_bound_history_to_the_checkpoint_and_reject_stale_publications() {
    for store in stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("checkpoints", schema()), None, 4096)
            .await
            .expect("create");
        for turn in ["zero", "one"] {
            let acquired = AcquiredTurn::begin(
                Arc::clone(&store),
                "checkpoints",
                &TurnInput::from(turn),
                &options(),
                0,
            )
            .await
            .expect("begin");
            store
                .complete_turn(
                    acquired.owner(),
                    &[ModelMessage::Assistant(format!("{turn}-answer"))],
                    HistoryProjection::default(),
                )
                .await
                .expect("complete");
        }
        let summary = json!({"context": "c", "decisions": [], "state": "s"});
        let checkpoint =
            SessionCheckpoint::new(0, 0, None, None, summary.clone()).expect("checkpoint");

        // Act
        store
            .publish_checkpoint("checkpoints", &checkpoint)
            .await
            .expect("publish");
        let regressed = SessionCheckpoint::new(0, 0, None, None, summary.clone()).expect("record");
        // Republishing the same coverage is idempotent, never a regression.
        store
            .publish_checkpoint("checkpoints", &regressed)
            .await
            .expect("republish current boundary");
        let beyond = SessionCheckpoint::new(9, 0, None, None, summary.clone()).expect("record");
        let beyond = store.publish_checkpoint("checkpoints", &beyond).await;
        let wrong_generation = SessionCheckpoint::new(1, 7, None, None, summary).expect("record");
        let wrong_generation = store
            .publish_checkpoint("checkpoints", &wrong_generation)
            .await;
        let missing = store.publish_checkpoint("missing", &checkpoint).await;

        // Assert
        assert!(matches!(missing, Err(SessionError::NotFound { .. })));
        assert!(matches!(beyond, Err(SessionError::CheckpointStale { .. })));
        assert!(matches!(
            wrong_generation,
            Err(SessionError::CheckpointStale { .. })
        ));
        let loaded = store.load_session("checkpoints").await.expect("load");
        assert_eq!(loaded.latest_finished_turn, Some(1));
        assert_eq!(
            loaded.checkpoint.expect("checkpoint").covered_through(),
            0,
            "the rejected publications changed nothing"
        );
        assert_eq!(
            loaded.turns.len(),
            1,
            "turn zero is covered by the checkpoint"
        );
        assert!(matches!(
            &loaded.turns[0].1.messages[0],
            ModelMessage::User(text) if text == "one"
        ));
    }
}

#[tokio::test]
async fn write_settlement_retries_preserve_terminal_outcomes() {
    // Arrange
    for store in stores().await {
        for applied in [false, true] {
            let id = format!("settlement-{applied}");
            store
                .create_session(&NewSession::new(&id, schema()), None, 100)
                .await
                .expect("create");
            let turn =
                AcquiredTurn::begin(store.clone(), &id, &TurnInput::from("first"), &options(), 0)
                    .await
                    .expect("turn");
            let write = store
                .write_intent(
                    turn.owner(),
                    "call",
                    Path::new("repo"),
                    "file",
                    None,
                    b"new",
                )
                .await
                .expect("intent");

            // Act
            store
                .finish_write(turn.owner(), write, applied)
                .await
                .expect("initial settlement");
            let settled = store.load_writes(&id).await.expect("settled record");
            store
                .finish_write(turn.owner(), write, applied)
                .await
                .expect("idempotent retry");
            let active_conflict = store.finish_write(turn.owner(), write, !applied).await;
            store
                .complete_turn(turn.owner(), &[], HistoryProjection::default())
                .await
                .expect("complete");
            let successor = AcquiredTurn::begin(
                store.clone(),
                &id,
                &TurnInput::from("successor"),
                &options(),
                0,
            )
            .await
            .expect("successor");
            store
                .finish_write(turn.owner(), write, applied)
                .await
                .expect("delayed idempotent retry");
            let delayed_conflict = store.finish_write(turn.owner(), write, !applied).await;

            // Assert
            assert!(active_conflict.is_err());
            assert!(delayed_conflict.is_err());
            assert_eq!(
                settled[0].status,
                if applied {
                    WriteStatus::Applied
                } else {
                    WriteStatus::Failed
                }
            );
            assert_eq!(store.load_writes(&id).await.expect("unchanged"), settled);
            store.renew(successor.owner()).await.expect("still owned");
        }
    }
}

#[tokio::test]
async fn conflicting_write_settlements_have_one_winner() {
    // Arrange
    for store in stores().await {
        store
            .create_session(&NewSession::new("settlement-race", schema()), None, 100)
            .await
            .expect("create");
        let turn = AcquiredTurn::begin(
            store.clone(),
            "settlement-race",
            &TurnInput::from("prompt"),
            &options(),
            0,
        )
        .await
        .expect("turn");
        let write = store
            .write_intent(
                turn.owner(),
                "call",
                Path::new("repo"),
                "file",
                None,
                b"new",
            )
            .await
            .expect("intent");

        // Act
        let (applied, failed) = tokio::join!(
            store.finish_write(turn.owner(), write, true),
            store.finish_write(turn.owner(), write, false),
        );

        // Assert
        assert_ne!(applied.is_ok(), failed.is_ok());
        let records = store.load_writes("settlement-race").await.expect("writes");
        assert_eq!(
            records[0].status,
            if applied.is_ok() {
                WriteStatus::Applied
            } else {
                WriteStatus::Failed
            }
        );
    }
}

#[tokio::test]
async fn concurrent_creation_and_acquisition_have_one_winner() {
    // Arrange
    for store in stores().await {
        let config = NewSession::new("race", schema());
        let selected = options();

        // Act
        let (first, second) = tokio::join!(
            store.create_session(&config, None, 100),
            store.create_session(&config, None, 100),
        );
        let creations = [first, second];
        let first_input = TurnInput::from("first");
        let second_input = TurnInput::from("second");
        let (first, second) = tokio::join!(
            AcquiredTurn::begin(store.clone(), "race", &first_input, &selected, 0),
            AcquiredTurn::begin(store.clone(), "race", &second_input, &selected, 0),
        );
        let acquisitions = [first, second];

        // Assert
        assert_eq!(creations.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            creations
                .iter()
                .filter(|result| matches!(result, Err(SessionError::AlreadyExists { .. })))
                .count(),
            1
        );
        assert_eq!(
            acquisitions.iter().filter(|result| result.is_ok()).count(),
            1
        );
        assert_eq!(
            acquisitions
                .iter()
                .filter(|result| matches!(result, Err(SessionError::Busy { .. })))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn owner_lookup_preserves_identity_across_activation() {
    // Arrange
    for store in stores().await {
        store
            .create_session(&NewSession::new("identity", schema()), None, 256)
            .await
            .expect("create");
        let acquired = AcquiredTurn::begin(
            Arc::clone(&store),
            "identity",
            &TurnInput::from("prompt"),
            &options(),
            0,
        )
        .await
        .expect("acquire");
        let active = acquired.owner();
        let reserved = TurnOwner::new(
            active.store_identity().clone(),
            active.session_id().to_string(),
            active.turn_position(),
            active.token().to_vec(),
        );
        let reservations = HashMap::from([(reserved.clone(), "reservation")]);

        // Act / Assert
        assert_ne!(
            reserved.interruption_error_type(),
            active.interruption_error_type()
        );
        assert_eq!(&reserved, active);
        assert_eq!(reservations.get(active), Some(&"reservation"));
        store.renew(active).await.expect("validate activated owner");
        for foreign in [
            TurnOwner::new(
                StoreIdentity::unique(),
                active.session_id().to_string(),
                active.turn_position(),
                active.token().to_vec(),
            ),
            TurnOwner::new(
                active.store_identity().clone(),
                "other".to_string(),
                active.turn_position(),
                active.token().to_vec(),
            ),
            TurnOwner::new(
                active.store_identity().clone(),
                active.session_id().to_string(),
                active.turn_position() + 1,
                active.token().to_vec(),
            ),
            TurnOwner::new(
                active.store_identity().clone(),
                active.session_id().to_string(),
                active.turn_position(),
                b"different-token".to_vec(),
            ),
        ] {
            assert_ne!(&foreign, active);
            assert_eq!(reservations.get(&foreign), None);
        }
    }
}

struct GatedModel {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Model for GatedModel {
    async fn complete(&self, _: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.entered.notify_one();
        self.release.notified().await;

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({"answer":"done"}),
        )))
    }
}

#[tokio::test(start_paused = true)]
async fn short_and_shortened_leases_renew_through_real_turn_completion() {
    for initial_seconds in [30, 300] {
        // Arrange
        let store = Arc::new(ExternalStore::with_leases(
            Duration::from_secs(initial_seconds),
            Duration::from_secs(6),
        ));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut session = Harness::new(
            GatedModel {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            },
            unbounded_context_budget(),
        )
        .store(store.clone())
        .session("lease", schema())
        .create()
        .await
        .expect("session");
        let task = tokio::spawn(async move { session.send("work").await });
        entered.notified().await;

        // Act
        let first_renewal = (initial_seconds / 2).min(100);
        tokio::time::advance(Duration::from_secs(first_renewal - 1)).await;
        tokio::task::yield_now().await;
        assert_eq!(store.renewals.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        for expected in 1..=7 {
            tokio::time::timeout(Duration::from_secs(1), store.renewed.notified())
                .await
                .expect("renew before confirmed expiry");
            assert_eq!(store.renewals.load(Ordering::SeqCst), expected);
            assert!(!task.is_finished());
            if expected != 7 {
                tokio::time::advance(Duration::from_secs(3)).await;
            }
        }
        release.notify_one();
        let outcome = task
            .await
            .expect("turn task")
            .expect("complete after renewal");

        // Assert
        assert_eq!(outcome.output()["answer"], "done");
        assert_eq!(
            store
                .load_session("lease")
                .await
                .expect("history")
                .turns
                .len(),
            1
        );
    }
}

#[tokio::test(start_paused = true)]
async fn expired_renewal_acknowledgment_cancels_a_short_lease_turn() {
    // Arrange
    let store = Arc::new(ExternalStore::with_leases(
        Duration::from_secs(30),
        Duration::ZERO,
    ));
    let entered = Arc::new(Notify::new());
    let mut session = Harness::new(
        GatedModel {
            entered: Arc::clone(&entered),
            release: Arc::new(Notify::new()),
        },
        unbounded_context_budget(),
    )
    .store(store.clone())
    .session("expired", schema())
    .create()
    .await
    .expect("session");
    let task = tokio::spawn(async move { session.send("work").await });
    entered.notified().await;

    // Act
    tokio::time::advance(Duration::from_secs(15)).await;
    let outcome = task.await.expect("turn task");

    // Assert
    assert!(matches!(outcome, Err(SessionError::OwnershipLost { .. })));
    assert_eq!(store.renewals.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load_session("expired").await.expect("history").turns,
        vec![(
            0,
            loaded_turn(vec![ModelMessage::User("work".to_string())]).with_stop(TurnStop {
                error_type: "interrupted".to_string(),
                ..TurnStop::default()
            })
        )]
    );
}

#[tokio::test]
async fn real_turns_resume_and_preserve_captured_store_and_options() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let original = Harness::new(
            Echo {
                requests: Arc::clone(&requests),
            },
            unbounded_context_budget(),
        )
        .store(Arc::clone(&store));
        let builder = original
            .session("capture", schema())
            .system_prompt("policy");
        let other = Arc::new(ExternalStore::new());
        let changed = original.store(other);
        let mut session = builder.create().await.expect("create");

        // Act
        session.send("one").await.expect("first");
        drop(session);
        let mut session = harness(Arc::clone(&store))
            .resume("capture")
            .await
            .expect("resume");
        let result = session.send("two").await.expect("second");

        // Assert
        assert_eq!(result.output()["answer"], "two");
        assert!(matches!(
            changed.resume("capture").await,
            Err(SessionError::NotFound { .. })
        ));
        assert!(matches!(
            requests.lock().expect("requests")[0].messages()[0],
            ModelMessage::System(_)
        ));
        assert_eq!(
            store
                .load_session("capture")
                .await
                .expect("history")
                .turns
                .len(),
            2
        );
    }
}

#[tokio::test]
async fn bounded_history_policy_is_shared() {
    // Arrange
    for store in stores().await {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let harness = Harness::new(
            Echo {
                requests: Arc::clone(&requests),
            },
            unbounded_context_budget(),
        )
        .store(Arc::clone(&store))
        .max_history_bytes(std::num::NonZeroUsize::new(50).expect("budget"));
        let mut session = harness
            .session("bounded", schema())
            .create()
            .await
            .expect("create");

        // Act
        session.send("one").await.expect("first");
        session.send("two").await.expect("second");
        session.send("three").await.expect("third");

        // Assert
        assert_eq!(requests.lock().expect("requests").len(), 3);
        let loaded = store.load_session("bounded").await.expect("bounded");
        assert!(
            loaded
                .turns
                .iter()
                .flat_map(|(_, turn)| &turn.messages)
                .map(ModelMessage::retained_bytes)
                .sum::<usize>()
                <= 50
        );
        assert!(loaded.turns.len() < 3);
    }
}

#[tokio::test]
async fn expired_reservation_acknowledgment_is_interrupted_before_execution() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    store
        .create_session(&NewSession::new("session", schema()), None, 1024)
        .await
        .expect("session");
    let gate = Arc::new(Gate::new(Arc::clone(&store), true));
    let expired = {
        let gate = Arc::clone(&gate);
        tokio::spawn(async move {
            AcquiredTurn::begin(gate, "session", &TurnInput::from("expired"), &options(), 0).await
        })
    };
    gate.entered.notified().await;

    // Act
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();
    gate.release.notify_one();
    let expired = expired.await.expect("acquisition");
    tokio::time::timeout(Duration::from_secs(2), gate.interrupted.notified())
        .await
        .expect("expired acknowledgment interrupted");
    let successor = AcquiredTurn::begin(
        Arc::clone(&store),
        "session",
        &TurnInput::from("successor"),
        &options(),
        0,
    )
    .await
    .expect("successor");

    // Assert
    assert!(matches!(expired, Err(SessionError::OwnershipLost { .. })));
    assert_eq!(successor.owner().turn_position(), 1);
    assert_ne!(StoreIdentity::unique(), StoreIdentity::unique());
    assert!(StoredTurnOptions::decode(&StoredTurnOptions::encode(&options())).is_ok());
}

#[tokio::test]
async fn reservations_owned_by_another_store_or_session_never_execute() {
    // Arrange
    for store in stores().await {
        store
            .create_session(&NewSession::new("other", schema()), None, 1024)
            .await
            .expect("other session");
        let mut foreign_store = Gate::new(Arc::clone(&store), true);
        foreign_store.identity = StoreIdentity::unique();
        let mut foreign_session = Gate::new(Arc::clone(&store), true);
        foreign_session.reserve_session = Some("other".to_string());

        for (gate, session_id) in [
            (foreign_store, "foreign-store"),
            (foreign_session, "foreign-session"),
        ] {
            gate.release.notify_one();
            let requests: Arc<Mutex<Vec<ModelRequest>>> = Arc::default();
            let mut session = Harness::new(
                Echo {
                    requests: Arc::clone(&requests),
                },
                unbounded_context_budget(),
            )
            .store(Arc::new(gate))
            .session(session_id, schema())
            .create()
            .await
            .expect("session");

            // Act
            let result = session.send("prompt").await;

            // Assert
            assert!(
                matches!(result, Err(SessionError::InvalidData { .. })),
                "{session_id}"
            );
            assert!(requests.lock().expect("requests").is_empty());
        }
    }
}

struct BlockingModel {
    entered: Arc<Notify>,
}

#[async_trait]
impl Model for BlockingModel {
    async fn complete(&self, _: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn separate_handles_share_admission_and_different_sessions_progress() {
    // Arrange
    for store in stores().await {
        let entered = Arc::new(Notify::new());
        let first = Harness::new(
            BlockingModel {
                entered: Arc::clone(&entered),
            },
            unbounded_context_budget(),
        )
        .store(Arc::clone(&store));
        let mut session = first
            .session("shared", schema())
            .create()
            .await
            .expect("create");
        let task = tokio::spawn(async move { session.send("blocked").await });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("model entered");
        let other = harness(Arc::clone(&store));
        let mut competing = other.resume("shared").await.expect("resume");

        // Act / Assert
        assert!(matches!(
            competing.send("busy").await,
            Err(SessionError::Busy { .. })
        ));
        let mut independent = other
            .session("independent", schema())
            .create()
            .await
            .expect("independent");
        independent
            .send("works")
            .await
            .expect("other session progresses");
        task.abort();
        assert!(task.await.expect_err("aborted").is_cancelled());
        tokio::task::yield_now().await;
        competing
            .send("recovered")
            .await
            .expect("cleanup before successor");
    }
}

#[tokio::test]
async fn abandoned_acquisition_retains_admission_and_never_executes() {
    // Arrange
    for store in stores().await {
        for after_commit in [false, true] {
            let id = format!("cancel-{after_commit}");
            let gate = Arc::new(Gate::new(Arc::clone(&store), after_commit));
            gate.fail_cleanup.store(true, Ordering::SeqCst);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let first = Harness::new(
                Echo {
                    requests: Arc::clone(&requests),
                },
                unbounded_context_budget(),
            )
            .store(gate.clone());
            let mut session = first.session(&id, schema()).create().await.expect("create");
            let task = tokio::spawn(async move { session.send("abandoned").await });
            tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
                .await
                .expect("acquisition entered");
            let mut successor = harness(Arc::clone(&store))
                .resume(&id)
                .await
                .expect("resume");

            // Act
            task.abort();
            assert!(task.await.expect_err("aborted").is_cancelled());
            assert!(matches!(
                successor.send("blocked").await,
                Err(SessionError::Busy { .. })
            ));
            gate.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), gate.interrupted.notified())
                .await
                .expect("cleanup attempted");
            let unresolved = successor.send("still blocked").await;

            // Assert
            assert!(matches!(unresolved, Err(SessionError::Store { .. })));
            assert!(requests.lock().expect("requests").is_empty());
            gate.fail_cleanup.store(false, Ordering::SeqCst);
            successor
                .send("recovered")
                .await
                .expect("owner cleanup and successor");
            let loaded = store.load_session(&id).await.expect("history");
            assert_eq!(loaded.turns.len(), 2);
            assert!(loaded.turns[0].1.stop.is_some());
            assert!(loaded.turns[1].1.stop.is_none());
        }
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_reopen_and_configuration_precedence_preserve_stores() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("session.db");
    let external: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    let original = harness(Arc::clone(&external));
    let captured = original.session("external", schema());
    let sqlite = original.database(&path);
    let mut session = sqlite
        .session("disk", schema())
        .create()
        .await
        .expect("disk session");

    // Act
    session.send("persisted").await.expect("turn");
    drop(session);
    drop(sqlite);
    let store: Arc<dyn SessionStore> = Arc::new(SqliteStore::open(&path).await.expect("reopen"));
    let independent: Arc<dyn SessionStore> =
        Arc::new(SqliteStore::open(&path).await.expect("independent pool"));
    let mut external_session = captured.create().await.expect("captured external");
    external_session
        .send("external")
        .await
        .expect("external turn");

    // Assert
    assert_eq!(store.identity(), independent.identity());
    assert_eq!(
        store
            .load_session("disk")
            .await
            .expect("reopened history")
            .turns
            .len(),
        1
    );
    assert!(matches!(
        store.load_session("external").await,
        Err(SessionError::NotFound { .. })
    ));
    let mut resumed = harness(store).resume("disk").await.expect("resume");
    resumed.send("second").await.expect("resumed turn");
    assert_eq!(
        independent
            .load_session("disk")
            .await
            .expect("shared pool identity")
            .turns
            .len(),
        2
    );
    harness(external)
        .run_once("stateless", schema())
        .await
        .expect("stateless");
}

#[tokio::test]
async fn acquisition_task_failure_retains_backend_error_and_releases_admission() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    let gate = Arc::new(Gate::new(Arc::clone(&store), true));
    gate.panic_acquire.store(true, Ordering::SeqCst);
    let mut session = harness(gate)
        .session("panic", schema())
        .create()
        .await
        .expect("session");

    // Act
    let error = session
        .send("panic")
        .await
        .expect_err("backend task panicked");
    let mut successor = harness(store).resume("panic").await.expect("resume");

    // Assert
    assert!(error.to_string().contains("acquire session turn"));
    assert!(std::error::Error::source(&error).is_some());
    successor
        .send("retry")
        .await
        .expect("no reservation was submitted");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn temporary_sqlite_handles_share_one_database_until_dropped() {
    // Arrange
    for path in ["", ":memory:"] {
        let database = SqliteStore::open(Path::new(path))
            .await
            .expect("temporary SQLite");
        let shared: Arc<dyn SessionStore> = Arc::new(database.clone());
        let independent = SqliteStore::open(Path::new(path))
            .await
            .expect("independent SQLite");
        let mut session = harness(Arc::clone(&shared))
            .session("temporary", schema())
            .create()
            .await
            .expect("session");

        // Act
        session.send("first").await.expect("first turn");
        let mut resumed = harness(Arc::new(database))
            .resume("temporary")
            .await
            .expect("shared instance");
        resumed.send("second").await.expect("second turn");

        // Assert
        assert_ne!(shared.identity(), independent.identity());
        assert_eq!(
            shared
                .load_session("temporary")
                .await
                .expect("retained history")
                .turns
                .len(),
            2
        );
        assert!(matches!(
            independent.load_session("temporary").await,
            Err(SessionError::NotFound { .. })
        ));
    }
}

#[tokio::test]
async fn unresolved_commands_fence_model_switches_until_owner_reconciliation() {
    // Arrange
    for store in stores().await.into_iter().skip(1) {
        store
            .create_session(&NewSession::new("switch-commands", schema()), None, 1024)
            .await
            .expect("session");
        let turn = AcquiredTurn::begin(
            Arc::clone(&store),
            "switch-commands",
            &TurnInput::from("run"),
            &options(),
            0,
        )
        .await
        .expect("turn");
        let id = store
            .command_intent(
                turn.owner(),
                &CommandIntent {
                    call_id: "command".into(),
                    command: "effect".into(),
                    policy: json!({}),
                    workspace: "/workspace".into(),
                },
            )
            .await
            .expect("intent");
        store.interrupt(turn.owner()).await.expect("stop owner");
        let identity = ag_harness::recovery::ExecutionIdentity::new("next", "1").expect("identity");
        let capabilities = ag_harness::model::ModelCapabilities {
            image_input: false,
            profile: unbounded_model_profile(),
            tool_calls: true,
        };

        // Act / Assert
        assert!(matches!(
            store
                .switch_model(
                    "switch-commands",
                    &ModelSwitch::new(identity.clone(), None, capabilities, 0)
                )
                .await,
            Err(SessionError::Busy { .. })
        ));
        let unchanged = store
            .load_session("switch-commands")
            .await
            .expect("unchanged selection");
        assert_eq!(unchanged.model_generation, 0);
        assert_eq!(unchanged.registration_identity, None);
        store
            .reconcile_command(turn.owner(), id)
            .await
            .expect("owner reconciliation");
        assert_eq!(
            store
                .switch_model(
                    "switch-commands",
                    &ModelSwitch::new(identity.clone(), None, capabilities, 0)
                )
                .await
                .expect("switch after reconciliation"),
            1
        );
        assert!(matches!(
            AcquiredTurn::begin(
                Arc::clone(&store),
                "switch-commands",
                &TurnInput::from("stale"),
                &options(),
                0
            )
            .await,
            Err(SessionError::StaleModel { .. })
        ));
        let next = AcquiredTurn::begin(
            Arc::clone(&store),
            "switch-commands",
            &TurnInput::from("next"),
            &options(),
            1,
        )
        .await
        .expect("new model turn");
        assert_ne!(next.owner(), turn.owner());
        let records = store
            .load_commands("switch-commands")
            .await
            .expect("records");
        assert_eq!(records[0].owner(), turn.owner());
        assert!(records[0].reconciled);
        assert!(records[0].outcome.is_none());
        store.interrupt(next.owner()).await.expect("stop successor");
    }
}

#[tokio::test]
async fn journaled_backends_fork_settled_turns_with_their_records_and_covered_checkpoint() {
    for store in journaled_stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("source", schema()), None, 4096)
            .await
            .expect("create");
        complete_forkable_turn(&store, "zero", true, HistoryProjection::default()).await;
        let projection = HistoryProjection::new(1, 1, 1);
        complete_forkable_turn(&store, "one", false, projection).await;
        let checkpoint = SessionCheckpoint::new(
            1,
            0,
            None,
            None,
            json!({"context": "c", "decisions": [], "state": "s"}),
        )
        .expect("checkpoint");
        store
            .publish_checkpoint("source", &checkpoint)
            .await
            .expect("publish");

        // Act
        store
            .fork_session("source", 1, "whole")
            .await
            .expect("fork whole");
        store
            .fork_session("source", 0, "early")
            .await
            .expect("fork early");

        // Assert
        assert_eq!(
            store
                .load_session("source")
                .await
                .expect("source")
                .projection,
            projection
        );
        let whole = store.load_session("whole").await.expect("whole");
        assert_eq!(whole.checkpoint, Some(checkpoint));
        assert_eq!(whole.projection, HistoryProjection::default());
        assert_eq!(
            store.load_session("early").await.expect("early").checkpoint,
            None
        );
        let writes = store.load_writes("whole").await.expect("writes");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, "a.txt");
        let commands = store.load_commands("whole").await.expect("commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].owner().session_id(), "whole");
        assert_eq!(commands[0].outcome, Some(completed_command()));
        let copied = store
            .load_transcript("whole", &TurnRange::All)
            .await
            .expect("whole transcript");
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[1].messages[0], ModelMessage::User("one".to_string()));
        assert_eq!(
            store
                .load_transcript("early", &TurnRange::All)
                .await
                .expect("early transcript")
                .len(),
            1
        );
        assert_eq!(
            store
                .load_transcript("source", &TurnRange::All)
                .await
                .expect("source transcript")
                .len(),
            2,
            "the source is unchanged"
        );
    }
}

#[tokio::test]
async fn journaled_backends_reject_forks_in_contract_order_changing_nothing() {
    for store in journaled_stores().await {
        // Arrange
        store
            .create_session(&NewSession::new("source", schema()), None, 4096)
            .await
            .expect("create");
        complete_forkable_turn(&store, "zero", true, HistoryProjection::default()).await;
        store
            .fork_session("source", 0, "whole")
            .await
            .expect("fork whole");
        let running = AcquiredTurn::begin(
            Arc::clone(&store),
            "source",
            &TurnInput::from("one"),
            &options(),
            0,
        )
        .await
        .expect("running turn");

        // Act
        let running_turn = store.fork_session("source", 1, "late").await;
        let missing = store.fork_session("missing", 0, "other").await;
        let taken = store.fork_session("source", 0, "whole").await;
        let taken_and_unsettled = store.fork_session("source", 1, "whole").await;
        let missing_transcript = store.load_transcript("missing", &TurnRange::All).await;

        // Assert
        assert!(matches!(
            running_turn,
            Err(SessionError::TurnNotFound { turn: 1, .. })
        ));
        assert!(matches!(missing, Err(SessionError::NotFound { .. })));
        assert!(matches!(taken, Err(SessionError::AlreadyExists { .. })));
        assert!(
            matches!(
                taken_and_unsettled,
                Err(SessionError::TurnNotFound { turn: 1, .. })
            ),
            "the turn is checked before the target"
        );
        assert!(matches!(
            missing_transcript,
            Err(SessionError::NotFound { .. })
        ));
        for rejected in ["late", "other"] {
            assert!(matches!(
                store.load_session(rejected).await,
                Err(SessionError::NotFound { .. })
            ));
        }
        assert_eq!(
            store
                .load_transcript("whole", &TurnRange::All)
                .await
                .expect("whole transcript")
                .len(),
            1
        );
        assert_eq!(store.load_writes("whole").await.expect("writes").len(), 1);
        store.interrupt(running.owner()).await.expect("interrupt");
    }
}

#[tokio::test]
async fn memory_backend_transcripts_and_forks_recover_a_turn_whose_lease_expired() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    store
        .create_session(&NewSession::new("source", schema()), None, 4096)
        .await
        .expect("create");
    let running = AcquiredTurn::begin(
        Arc::clone(&store),
        "source",
        &TurnInput::from("expired"),
        &options(),
        0,
    )
    .await
    .expect("running turn");
    let live = store
        .load_transcript("source", &TurnRange::All)
        .await
        .expect("live transcript");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::time::resume();

    // Act
    let transcript = store
        .load_transcript("source", &TurnRange::All)
        .await
        .expect("recovered transcript");
    let fork = store.fork_session("source", 0, "fork").await;

    // Assert
    assert_eq!(live, Vec::new(), "a live turn is not settled");
    assert_eq!(transcript.len(), 1);
    assert_eq!(transcript[0].status, TurnStatus::Interrupted);
    assert_eq!(transcript[0].error_type.as_deref(), Some("interrupted"));
    assert!(fork.is_ok(), "the recovered turn can be forked at");
    drop(running);
}

#[tokio::test]
async fn external_store_without_transcript_support_fails_closed() {
    // Arrange
    let store: Arc<dyn SessionStore> = Arc::new(ExternalStore::new());
    let harness = harness(Arc::clone(&store));
    let session = harness
        .session("external", schema())
        .create()
        .await
        .expect("session");

    // Act
    let loaded = store.load_transcript("external", &TurnRange::All).await;
    let forked = store.fork_session("external", 0, "copy").await;
    let transcript = session.transcript(TurnRange::All).await;
    let fork = harness
        .fork(
            &SessionId::new("external"),
            TurnIndex::new(0),
            SessionId::new("copy"),
        )
        .await;

    // Assert
    assert!(matches!(loaded, Err(SessionError::InvalidData { .. })));
    assert!(matches!(forked, Err(SessionError::InvalidData { .. })));
    assert_eq!(
        transcript.err().map(|error| error.kind()),
        Some(ErrorKind::Store)
    );
    assert_eq!(fork.err().map(|error| error.kind()), Some(ErrorKind::Store));
}

/// Completes a turn of session `source` for `input` keeping `projection`,
/// first recording an applied write and a completed command when
/// `with_effects`.
async fn complete_forkable_turn(
    store: &Arc<dyn SessionStore>,
    input: &str,
    with_effects: bool,
    projection: HistoryProjection,
) {
    let turn = AcquiredTurn::begin(
        Arc::clone(store),
        "source",
        &TurnInput::from(input),
        &options(),
        0,
    )
    .await
    .expect("turn");
    if with_effects {
        let write = store
            .write_intent(
                turn.owner(),
                "call",
                Path::new("/repo"),
                "a.txt",
                None,
                b"new",
            )
            .await
            .expect("write intent");
        store
            .finish_write(turn.owner(), write, true)
            .await
            .expect("finish write");
        let intent = CommandIntent {
            call_id: "call".into(),
            command: "true".into(),
            policy: json!({}),
            workspace: "/repo".into(),
        };
        let command = store
            .command_intent(turn.owner(), &intent)
            .await
            .expect("command intent");
        store
            .finish_command(turn.owner(), command, &completed_command())
            .await
            .expect("finish command");
    }
    let output = json!({"answer": input}).to_string();
    store
        .complete_turn(turn.owner(), &[ModelMessage::Assistant(output)], projection)
        .await
        .expect("complete");
}

fn completed_command() -> CommandOutcome {
    CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::PidNamespace,
        execution_failure: None,
        exit: CommandExit::new(Some(0), None),
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: String::new(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        truncated: false,
    }
}

#[tokio::test]
async fn backend_duplicate_acquisition_is_atomic_and_recovers_pending_effects() {
    // Arrange
    for store in stores().await {
        let harness = harness(Arc::clone(&store)).execution_identity(
            ag_harness::recovery::ExecutionIdentity::new("echo", "1").expect("identity"),
        );
        let mut seed = harness
            .session("seed", schema())
            .create()
            .await
            .expect("seed");
        seed.turn("hello")
            .options(options())
            .host_id("id")
            .await
            .expect("seed result");
        let request = seed
            .recover("id")
            .await
            .expect("lookup")
            .expect("record")
            .request;
        store
            .create_session(&NewSession::new("race", schema()), None, 1000)
            .await
            .expect("session");
        let options = options();

        // Act
        let input = TurnInput::from("hello");
        let (left, right) = tokio::join!(
            AcquiredTurn::begin_request(Arc::clone(&store), "race", &input, &options, &request, 0),
            AcquiredTurn::begin_request(Arc::clone(&store), "race", &input, &options, &request, 0)
        );
        let ((HostTurnAcquisition::Acquired(acquired), HostTurnAcquisition::Recorded(duplicate))
        | (HostTurnAcquisition::Recorded(duplicate), HostTurnAcquisition::Acquired(acquired))) =
            (left.expect("left"), right.expect("right"))
        else {
            std::panic::resume_unwind(Box::new("exactly one reservation"));
        };
        let owner = acquired.owner();
        let write_id = store
            .write_intent(owner, "call", Path::new("workspace"), "file", None, b"new")
            .await
            .expect("intent");
        store.interrupt(owner).await.expect("interrupt");
        let recovered = store
            .load_request("race", "id")
            .await
            .expect("lookup")
            .expect("record");
        store
            .finish_write(owner, write_id, true)
            .await
            .expect("late outcome");
        let settled = store
            .load_request("race", "id")
            .await
            .expect("lookup")
            .expect("record");

        // Assert
        assert!(matches!(duplicate.status, HostTurnStatus::InProgress));
        assert!(matches!(
            recovered.status,
            HostTurnStatus::Interrupted { .. }
        ));
        assert_eq!(recovered.writes[0].status, WriteStatus::Pending);
        assert_eq!(settled.writes[0].status, WriteStatus::Applied);
        let mut changed = serde_json::to_value(&request).expect("request encoding");
        changed["fingerprint"] = json!("changed");
        let changed: HostRequest = serde_json::from_value(changed).expect("request");
        assert!(matches!(
            AcquiredTurn::begin_request(
                Arc::clone(&store),
                "race",
                &TurnInput::from("changed"),
                &options,
                &changed,
                0
            )
            .await,
            Err(SessionError::HostTurnConflict)
        ));
        assert!(matches!(
            store.load_request("missing", "id").await,
            Err(SessionError::NotFound { .. })
        ));
    }
}

/// Never answers a request.
struct HangingModel;

#[async_trait]
impl Model for HangingModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        std::future::pending().await
    }
}

/// Reads `stream` through the first event whose kind `last` accepts,
/// without waiting for the run's outcome, and returns the kinds read.
async fn kinds_until(
    stream: &mut ag_harness::EventStream,
    last: impl Fn(&ag_harness::RunEventKind) -> bool,
) -> Vec<ag_harness::RunEventKind> {
    let mut kinds = Vec::new();
    while let Some(event) = std::future::poll_fn(|context| {
        futures_core::Stream::poll_next(std::pin::Pin::new(&mut *stream), context)
    })
    .await
    {
        let done = last(&event.kind);
        kinds.push(event.kind);
        if done {
            break;
        }
    }

    kinds
}

fn is_completed(kind: &ag_harness::RunEventKind) -> bool {
    matches!(kind, ag_harness::RunEventKind::Completed(_))
}

#[tokio::test]
async fn completed_events_follow_the_settled_status_of_cancelled_turns() {
    for store in journaled_stores().await {
        // Arrange
        let gate = Arc::new(Gate::new(store, true));
        gate.pause_interrupt.store(true, Ordering::SeqCst);
        let session = Harness::new(HangingModel, unbounded_context_budget())
            .execution_identity(
                ag_harness::recovery::ExecutionIdentity::new("hanging", "1").expect("identity"),
            )
            .store(Arc::clone(&gate) as Arc<dyn SessionStore>)
            .session("settled", schema())
            .create()
            .await
            .expect("session");
        let (mut unadmitted, handle) = session
            .run(ag_harness::RunRequest::new("hang"))
            .into_parts();
        handle.cancel();
        let unadmitted_outcome = tokio::spawn(handle.outcome());
        let unadmitted_kinds = kinds_until(&mut unadmitted, is_completed).await;
        let before_admission = session
            .transcript(TurnRange::All)
            .await
            .expect("transcript");
        let unadmitted_outcome = unadmitted_outcome.await.expect("outcome");
        let (mut admitted, handle) = session
            .run(ag_harness::RunRequest::new("hang"))
            .into_parts();
        let cancel = handle.cancel_token();
        let admitted_outcome = tokio::spawn(handle.outcome());
        gate.entered.notified().await;
        gate.release.notify_one();
        let started = kinds_until(&mut admitted, |kind| {
            matches!(kind, ag_harness::RunEventKind::Started { .. })
        })
        .await;

        // Act
        cancel.cancel();
        gate.entered.notified().await;
        let while_interrupting = std::future::poll_fn(|context| {
            std::task::Poll::Ready(
                futures_core::Stream::poll_next(std::pin::Pin::new(&mut admitted), context)
                    .is_pending(),
            )
        })
        .await;
        let before_settlement = session
            .transcript(TurnRange::All)
            .await
            .expect("transcript");
        gate.release.notify_one();
        let admitted_kinds = kinds_until(&mut admitted, is_completed).await;
        let after_admission = session
            .transcript(TurnRange::All)
            .await
            .expect("transcript");

        // Assert
        assert!(matches!(
            unadmitted_kinds.as_slice(),
            [ag_harness::RunEventKind::Completed(_)]
        ));
        assert!(
            before_admission.is_empty(),
            "a run cancelled before admission records no turn"
        );
        assert_eq!(
            unadmitted_outcome.result.err().map(|error| error.kind()),
            Some(ErrorKind::Cancelled)
        );
        assert!(matches!(
            started.as_slice(),
            [ag_harness::RunEventKind::Started { .. }]
        ));
        assert!(
            while_interrupting,
            "`completed` waits for the interrupted status"
        );
        assert_eq!(before_settlement, Vec::<ag_harness::TurnRecord>::new());
        assert!(matches!(
            admitted_kinds.as_slice(),
            [ag_harness::RunEventKind::Completed(_)]
        ));
        let statuses: Vec<_> = after_admission.iter().map(|record| record.status).collect();
        assert_eq!(
            statuses,
            [TurnStatus::Interrupted],
            "the turn is settled once `completed` arrives"
        );
        assert_eq!(
            admitted_outcome
                .await
                .expect("outcome")
                .result
                .err()
                .map(|error| error.kind()),
            Some(ErrorKind::Cancelled)
        );
    }
}

/// Answers `{"answer": <prompt>}`, reporting 1,000 input tokens.
struct UsageModel;

#[async_trait]
impl Model for UsageModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let usage = ag_harness::model::CompletionUsage::default().with_input_tokens(Some(1_000));

        Ok(ModelCompletion::new(
            ag_harness::model::CompletionMetadata::new("stop").with_usage(Some(usage)),
            ModelResponse::Output(json!({"answer": request.prompt()})),
        ))
    }
}

#[tokio::test]
async fn store_that_cannot_record_usage_fails_the_turn_at_admission_or_after_a_request() {
    for records in [0, 1] {
        // Arrange
        let memory: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
        let gate = Gate::new(Arc::clone(&memory), false);
        gate.usage_records.store(records, Ordering::SeqCst);
        let harness = Harness::new(UsageModel, unbounded_context_budget())
            .execution_identity(
                ag_harness::recovery::ExecutionIdentity::new("usage", "1").expect("identity"),
            )
            .store(Arc::new(gate));
        let mut session = harness
            .session("failing", schema())
            .create()
            .await
            .expect("session");

        // Act
        let error = session
            .turn("plain")
            .host_id("request")
            .await
            .expect_err("the turn must fail");
        let stored = memory
            .load_transcript("failing", &TurnRange::All)
            .await
            .expect("transcript");
        let totals = session.usage().await.expect("usage");

        // Assert
        assert!(matches!(
            &error,
            SessionError::Turn(TurnError::UsageJournal { .. })
        ));
        assert_eq!(
            ag_harness::HarnessError::from(error).kind(),
            ErrorKind::Store
        );
        assert_eq!(stored[0].status, TurnStatus::Failed);
        let recorded_zero = (records == 1).then_some(0);
        assert_eq!(
            stored[0].usage.map(|usage| usage.usage.input),
            recorded_zero,
            "only the admission record succeeded"
        );
        assert_eq!(totals.unknown_turns, u64::from(records == 0));
    }
}

/// Answers every prompt with its name, reported as its provider and model,
/// and records each request.
struct NamedModel {
    name: &'static str,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl Model for NamedModel {
    fn metadata(&self) -> Option<ag_harness::model::ModelMetadata> {
        Some(ag_harness::model::ModelMetadata::new(self.name, self.name).expect("metadata"))
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests.lock().expect("requests").push(request);

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({"answer": self.name}),
        )))
    }
}

/// Returns a registry holding a [`NamedModel`] under its name with `profile`.
fn named_registry(
    name: &'static str,
    requests: &Arc<Mutex<Vec<ModelRequest>>>,
    profile: ag_harness::model::ModelProfile,
) -> ag_harness::model::ModelRegistry {
    let mut registry = ag_harness::model::ModelRegistry::new();
    registry
        .register(
            ag_harness::recovery::ExecutionIdentity::new(name, "1").expect("identity"),
            NamedModel {
                name,
                requests: Arc::clone(requests),
            },
            ag_harness::model::ModelCapabilities::new(profile).with_tool_calls(true),
        )
        .expect("register");

    registry
}

#[tokio::test]
async fn stale_publication_is_rejected_after_a_concurrent_turn() {
    for store in stores().await {
        // Arrange
        let requests = Arc::default();
        let registry = named_registry("stale", &requests, unbounded_model_profile());
        let harness = Harness::from_registry(&registry, "stale")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("stale", schema())
            .create()
            .await
            .expect("session");
        session.send("only").await.expect("only");
        let summary = json!({"context": "c", "decisions": [], "state": "s"});
        let checkpoint = SessionCheckpoint::new(
            0,
            0,
            Some("stale".into()),
            Some("stale".into()),
            summary.clone(),
        )
        .expect("checkpoint");

        // Act: publish a checkpoint whose generation no longer matches after a
        // switch, and one whose boundary exceeds the completed turns.
        session
            .switch_model(&registry, "stale")
            .await
            .expect("switch back is a no-op key");
        let stale_generation = store.publish_checkpoint("stale", &checkpoint).await;
        let generation = store
            .load_session("stale")
            .await
            .expect("load")
            .model_generation;
        let beyond_completed = SessionCheckpoint::new(
            5,
            generation,
            Some("stale".into()),
            Some("stale".into()),
            summary,
        )
        .expect("checkpoint");
        let beyond = store.publish_checkpoint("stale", &beyond_completed).await;

        // Assert
        assert!(matches!(
            stale_generation,
            Err(SessionError::CheckpointStale { .. })
        ));
        assert!(matches!(beyond, Err(SessionError::CheckpointStale { .. })));
        assert!(
            store
                .load_session("stale")
                .await
                .expect("load")
                .checkpoint
                .is_none(),
            "a rejected publication changes nothing"
        );
    }
}

#[tokio::test]
async fn heavy_summary_is_dropped_from_projection_and_rejected_as_a_source() {
    for store in stores().await {
        // Arrange: a published summary heavier than the whole request budget.
        let requests = Arc::default();
        let registry = named_registry(
            "heavy",
            &requests,
            ag_harness::model::ModelProfile::new(500),
        );
        let harness = Harness::from_registry(&registry, "heavy")
            .expect("harness")
            .store(Arc::clone(&store));
        let mut session = harness
            .session("heavy", schema())
            .create()
            .await
            .expect("session");
        session.send("one").await.expect("one");
        let decisions: Vec<String> = (0..8).map(|_| "d".repeat(400)).collect();
        let oversized_summary = json!({
            "context": "c".repeat(4000),
            "decisions": decisions,
            "state": "s".repeat(2000)
        });
        let heavy = SessionCheckpoint::new(0, 0, None, None, oversized_summary).expect("record");
        store
            .publish_checkpoint("heavy", &heavy)
            .await
            .expect("publish");

        // Act: projection admits recent turns without the summary, while
        // generation cannot admit the summary-bearing source at all.
        let two = session.send("two").await.expect("post-checkpoint turn");
        let rejected = session.compact().await;

        // Assert
        let requests = requests.lock().expect("requests");
        assert!(
            !requests
                .last()
                .expect("request")
                .messages()
                .iter()
                .any(|message| matches!(
                    message,
                    ModelMessage::User(text) if text.contains("Compaction checkpoint summarizing")
                )),
            "a summary beyond the budget falls back to recent history"
        );
        // The checkpoint covers the only completed turn, so nothing is
        // replayed and the dropped summary is visible in the report.
        assert!(!two.report().history().checkpoint_replayed());
        assert_eq!(two.report().history().replayed_turns(), 0);
        assert_eq!(two.report().history().evicted_turns(), 0);
        assert!(matches!(
            rejected,
            Err(SessionError::Turn(TurnError::ContextBudgetExceeded { .. }))
        ));
    }
}
