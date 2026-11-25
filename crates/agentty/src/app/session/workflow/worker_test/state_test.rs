//! Turn event consumption, progress and activity publication, and status
//! update tests for the session worker.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::{
    ActivityEvent, ActivityKind, ActivityStatus, AgentError, AgentRequestKind, MockAgentChannel,
    OutputEvent, OutputKind, TurnEvent, TurnNotice, TurnResult,
};
use ag_forge as forge;
use ag_git::{DiffStats, MockGitClient};
use ag_protocol::AgentResponse;
use ag_worker::SessionRunClient;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::super::post_turn::status_update_after_turn_result;
use super::super::super::turn::{consume_turn_events, run_channel_turn};
use super::super::{SessionWorkerContext, SessionWorkerService};
use super::support::{
    auto_commit_run_client, default_turn_metadata, empty_transcript,
    insert_in_progress_test_session, mock_fs_client_with_existing_directories,
    mock_git_client_detecting_main_repo, queue_test_context, seed_recovery_test_operation,
    successful_turn_result, transcript_text,
};
use crate::app::AppEvent;
use crate::app::cargo_cache::CargoCacheWarmer;
use crate::app::graph_activity::GraphActivityRegistry;
use crate::app::service::HarnessModels;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::Status;
use crate::domain::turn_timeline::{RowState, TimelineRow};
use crate::infra::cargo_cache::MockCargoCacheClient;
use crate::infra::clock::Clock;
use crate::infra::db::AppRepositories;
use crate::test_support::{FixedClock, enable_tracing_log_fallback, poison_lock};

/// Returns a clock pinned to the Unix epoch.
fn fixed_clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::unix_epoch())
}

#[tokio::test]
/// Verifies process-only events do not append transcript content.
async fn test_consume_turn_events_ignores_pid_only_events_for_transcript_messages() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let child_pid = Arc::new(Mutex::new(None));

    event_tx
        .send(TurnEvent::PidUpdate(Some(4242)))
        .expect("failed to send pid update");
    drop(event_tx);

    // Act
    consume_turn_events(
        event_rx,
        app_event_tx,
        "session-1".into(),
        Arc::clone(&child_pid),
        fixed_clock(),
    )
    .await;

    // Assert
    assert_eq!(*child_pid.lock().expect("pid lock poisoned"), Some(4242));
    assert!(app_event_rx.try_recv().is_err());
}

#[tokio::test]
/// Verifies thought deltas update the loader state without appending
/// transcript messages.
async fn test_consume_turn_events_routes_thought_delta_to_progress_state_only() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let child_pid = Arc::new(Mutex::new(None));

    event_tx
        .send(TurnEvent::ThoughtDelta("Inspecting files".to_string()))
        .expect("failed to send thought delta");
    drop(event_tx);

    // Act
    consume_turn_events(
        event_rx,
        app_event_tx,
        "session-1".into(),
        child_pid,
        fixed_clock(),
    )
    .await;

    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        events,
        vec![
            AppEvent::SessionProgressUpdated {
                progress_message: Some("Inspecting files".to_string()),
                session_id: "session-1".into(),
            },
            AppEvent::SessionProgressUpdated {
                progress_message: None,
                session_id: "session-1".into(),
            },
        ]
    );
}

#[tokio::test]
/// Verifies ready thought-delta bursts enqueue only the latest progress
/// update before the final clear event.
async fn test_consume_turn_events_coalesces_ready_thought_delta_bursts() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let child_pid = Arc::new(Mutex::new(None));

    for thought in ["first", "second", "third"] {
        event_tx
            .send(TurnEvent::ThoughtDelta(thought.to_string()))
            .expect("failed to send thought delta");
    }
    drop(event_tx);

    // Act
    consume_turn_events(
        event_rx,
        app_event_tx,
        "session-1".into(),
        child_pid,
        fixed_clock(),
    )
    .await;

    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        events,
        vec![
            AppEvent::SessionProgressUpdated {
                progress_message: Some("third".to_string()),
                session_id: "session-1".into(),
            },
            AppEvent::SessionProgressUpdated {
                progress_message: None,
                session_id: "session-1".into(),
            },
        ]
    );
}

#[tokio::test]
/// Verifies blank thoughts, reserved terminal events, and a repeated thought
/// after an exhausted coalescing budget publish no extra progress updates.
async fn test_consume_turn_events_skips_blank_terminal_and_repeated_progress_events() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let completed = || TurnEvent::Completed {
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
    };
    let mut events = vec![
        TurnEvent::ThoughtDelta("   ".to_string()),
        completed(),
        TurnEvent::Failed("reserved".to_string()),
        TurnEvent::ThoughtDelta("same".to_string()),
        completed(),
        TurnEvent::Failed("reserved".to_string()),
        TurnEvent::ThoughtDelta(" ".to_string()),
        TurnEvent::PidUpdate(Some(4242)),
    ];
    // Fill the rest of the coalescing budget and leave one repeated thought
    // for the next consumer iteration.
    events.extend((0..60).map(|_| TurnEvent::ThoughtDelta("same".to_string())));
    for event in events {
        event_tx.send(event).expect("failed to send turn event");
    }
    drop(event_tx);
    let child_pid = Arc::new(Mutex::new(None));
    enable_tracing_log_fallback();

    // Act
    consume_turn_events(
        event_rx,
        app_event_tx,
        "session-1".into(),
        Arc::clone(&child_pid),
        fixed_clock(),
    )
    .await;
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert_eq!(*child_pid.lock().expect("pid lock poisoned"), Some(4242));
    assert_eq!(
        events,
        vec![
            AppEvent::SessionProgressUpdated {
                progress_message: Some("same".to_string()),
                session_id: "session-1".into(),
            },
            AppEvent::SessionProgressUpdated {
                progress_message: None,
                session_id: "session-1".into(),
            },
        ]
    );
}

#[tokio::test]
/// Verifies a poisoned PID slot keeps its last value instead of failing the
/// event consumer.
async fn test_consume_turn_events_skips_pid_updates_for_a_poisoned_pid_slot() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let child_pid = Arc::new(Mutex::new(None));
    poison_lock(Arc::clone(&child_pid));
    event_tx
        .send(TurnEvent::PidUpdate(Some(4242)))
        .expect("failed to send pid update");
    drop(event_tx);

    // Act
    consume_turn_events(
        event_rx,
        mpsc::unbounded_channel().0,
        "session-1".into(),
        Arc::clone(&child_pid),
        fixed_clock(),
    )
    .await;

    // Assert
    let pid = child_pid
        .lock()
        .expect_err("pid slot stays poisoned")
        .into_inner();
    assert_eq!(*pid, None);
}

#[tokio::test]
/// Verifies a turn that dirties the main checkout records a warning while
/// preserving the successful agent response.
async fn test_run_channel_turn_warns_when_main_checkout_status_changes() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_test_session(&db).await;

    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .once()
        .returning(|_session_id, _req, events| {
            Box::pin(async move {
                events
                    .send(TurnEvent::Activity(activity_event()))
                    .expect("test fixture should succeed");
                Ok(TurnResult {
                    assistant_message: AgentResponse::plain("done"),
                    context_reset: false,
                    harness_report: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });

    let status_call_count = Arc::new(Mutex::new(0));
    let mut mock_git_client = mock_git_client_detecting_main_repo(base_dir.path().join("main"));
    mock_git_client
        .expect_tracked_worktree_status()
        .times(2)
        .returning(move |_| {
            let status_call_count = Arc::clone(&status_call_count);

            Box::pin(async move {
                let mut call_count = status_call_count
                    .lock()
                    .expect("status call count lock poisoned");
                *call_count += 1;
                if *call_count == 1 {
                    Ok(String::new())
                } else {
                    Ok(" M README.md\n".to_string())
                }
            })
        });
    mock_git_client
        .expect_diff_stats()
        .returning(|_, _| Box::pin(async { Ok(DiffStats::default()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));

    let transcript = empty_transcript();
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        available_agent_kinds: Arc::from(AgentKind::ALL),
        graph_activity: GraphActivityRegistry::default(),
        harness_models: HarnessModels::default(),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        cargo_cache: CargoCacheWarmer::default(),
        session_run: SessionRunClient::from_channel("sess1".to_string(), Arc::new(mock_channel)),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: Arc::clone(&transcript),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    };

    // Act
    let result = run_channel_turn(
        &context,
        auto_commit_run_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;

    // Assert
    assert!(result.is_ok(), "main checkout changes should warn only");
    let output_text = transcript_text(&transcript);
    assert!(output_text.contains("[Main Checkout Warning]"));
    assert!(output_text.contains("tracked-file status changed"));
    assert!(output_text.contains("done"));
    let messages = db.sessions().load_session_messages("sess1").await;
    let messages = messages.expect("messages");
    let answer_index = messages
        .iter()
        .position(|message| message.kind == "assistant_answer")
        .expect("test fixture should succeed");
    let summary = &messages[answer_index - 1];
    assert_eq!(summary.kind, "activity_summary");
    assert!(summary.content.contains("\nUsed\tRead ×1\n"));
    assert!(summary.content.ends_with(" · 1 tool"));
    assert!(!output_text.contains("─ Worked"));
}

#[test]
fn test_status_update_after_turn_result_skips_stopped_by_user() {
    // Arrange
    let result = Err(SessionError::StoppedByUser(
        "[Stopped] Session interrupted by user.".to_string(),
    ));

    // Act
    let status_update = status_update_after_turn_result(&result);

    // Assert
    assert_eq!(status_update, None);
}

#[tokio::test]
/// Verifies recovery returns an operation-interruption failure after
/// session reconciliation rather than admitting normal work.
async fn test_fail_unfinished_operations_from_previous_run_returns_operation_update_error() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    seed_recovery_test_operation(&db, Status::InProgress, "reply").await;
    sqlx::query(
        "CREATE TRIGGER fail_recovery_operation_update BEFORE UPDATE OF status ON \
         session_operation BEGIN SELECT RAISE(FAIL, 'operation update failed'); END",
    )
    .execute(&pool)
    .await
    .expect("failed to create operation update trigger");
    let mut mock_git_client = MockGitClient::new();
    mock_git_client.expect_is_rebase_in_progress().times(0);
    mock_git_client.expect_abort_rebase().times(0);

    // Act
    let result = SessionWorkerService::fail_unfinished_operations_from_previous_run_at(
        &db,
        base_dir.path(),
        Arc::new(mock_git_client),
        300,
    )
    .await;
    let sessions = db
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    let operation_is_unfinished = db
        .operations()
        .is_session_operation_unfinished("op-1")
        .await
        .expect("failed to check operation status");

    // Assert
    assert!(matches!(result, Err(SessionError::Db(_))));
    assert_eq!(sessions[0].status, "Review");
    assert!(operation_is_unfinished);
}

fn activity_event() -> ActivityEvent {
    ActivityEvent {
        attempt_id: "attempt".into(),
        exit_code: None,
        facts: Box::default(),
        id: "call".into(),
        input: None,
        kind: ActivityKind::Tool,
        name: "Read".into(),
        observed_at: std::time::SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status: ActivityStatus::Completed,
    }
}

#[tokio::test]
async fn test_consume_activity_survives_progress_coalescing() {
    // Arrange
    let (sender, receiver) = mpsc::unbounded_channel();
    sender
        .send(TurnEvent::Activity(activity_event()))
        .expect("test fixture should succeed");
    sender
        .send(TurnEvent::ThoughtDelta("thinking".into()))
        .expect("test fixture should succeed");
    let mut second = activity_event();
    second.id = "second".into();
    sender
        .send(TurnEvent::Activity(second))
        .expect("test fixture should succeed");
    drop(sender);

    // Act
    let activity = consume_turn_events(
        receiver,
        mpsc::unbounded_channel().0,
        "session".into(),
        Arc::new(Mutex::new(None)),
        fixed_clock(),
    )
    .await;

    // Assert
    assert_eq!(
        activity.summary().render(None),
        "turn-summary v1\n2 tools · 0s\nUsed\tRead ×2\n─ Worked 0s · 2 tools"
    );
}

#[tokio::test]
/// Verifies streamed output, including events drained while coalescing a
/// thought, is published once and cleared with the turn's live rows.
async fn test_consume_turn_events_streams_output_and_clears_it_with_the_turn() {
    // Arrange
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    for event in [
        TurnEvent::Output(OutputEvent::Started {
            id: 0,
            kind: OutputKind::ToolCall,
        }),
        TurnEvent::ThoughtDelta("thinking".to_string()),
        TurnEvent::Output(OutputEvent::Completed {
            id: 0,
            text: String::new(),
        }),
        TurnEvent::Output(OutputEvent::Started {
            id: 1,
            kind: OutputKind::Message,
        }),
        TurnEvent::Output(OutputEvent::Delta {
            id: 1,
            text: "Hello".to_string(),
        }),
        TurnEvent::Activity(ActivityEvent {
            status: ActivityStatus::Running,
            ..activity_event()
        }),
    ] {
        event_tx.send(event).expect("send");
    }
    drop(event_tx);

    // Act
    let activity = consume_turn_events(
        event_rx,
        app_event_tx.clone(),
        "session".into(),
        Arc::new(Mutex::new(None)),
        fixed_clock(),
    )
    .await;
    activity.clear_live(&app_event_tx, &"session".into());
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    let turns = events
        .iter()
        .filter_map(|event| match event {
            AppEvent::SessionTurnReady { session_id, turn } => Some((turn, session_id.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].1, "session");
    let snapshot = turns[0].0.snapshot();
    assert_eq!(snapshot.steps.len(), 2);
    assert_eq!(
        snapshot.steps[1]
            .narration
            .as_ref()
            .map(|narration| &*narration.text),
        Some("Hello")
    );
    assert_eq!(
        snapshot
            .steps
            .iter()
            .map(|step| step.rows.len())
            .sum::<usize>(),
        2
    );
    assert_eq!(
        events.last(),
        Some(&AppEvent::SessionTurnCleared {
            session_id: "session".into(),
        })
    );
}

#[tokio::test]
/// Verifies a turn removes its streamed output right after the saved answer,
/// which follows the turn summary, exactly once.
async fn test_run_channel_turn_clears_streamed_output_beside_the_saved_answer() {
    // Arrange
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .returning(|_session_id, _req, events| {
            Box::pin(async move {
                for event in [
                    TurnEvent::Output(OutputEvent::Started {
                        id: 0,
                        kind: OutputKind::Message,
                    }),
                    TurnEvent::Output(OutputEvent::Delta {
                        id: 0,
                        text: "done".to_string(),
                    }),
                    TurnEvent::Activity(activity_event()),
                ] {
                    events.send(event).expect("turn event");
                }

                Ok(successful_turn_result("done"))
            })
        });
    let (mut context, _db, _queued_messages, _directory) =
        queue_test_context(channel, VecDeque::new(), Status::InProgress).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    context.app_event_tx = app_event_tx;

    // Act
    let result = run_channel_turn(
        &context,
        auto_commit_run_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert!(result.is_ok(), "{result:?}");
    let is_cleared = |event: &AppEvent| matches!(event, AppEvent::SessionTurnCleared { .. });
    let cleared = events
        .iter()
        .position(is_cleared)
        .expect("live timeline cleared");
    assert!(matches!(
        events[cleared - 1],
        AppEvent::SessionUpdated { .. }
    ));
    assert_eq!(events.iter().filter(|event| is_cleared(event)).count(), 1);
    assert!(transcript_text(&context.transcript).contains("done"));
}

#[tokio::test]
/// Verifies a stopped or failed turn saves its summary right before its
/// error notice and removes its live timeline only after both are saved.
async fn test_run_channel_turn_saves_the_summary_before_a_failed_turn_notice() {
    for (stopped, notice) in [
        (true, "Session interrupted by user."),
        (false, "provider failed"),
    ] {
        // Arrange
        let mut channel = MockAgentChannel::new();
        channel
            .expect_run_turn()
            .once()
            .returning(move |_session_id, _req, events| {
                Box::pin(async move {
                    events
                        .send(TurnEvent::Activity(activity_event()))
                        .expect("turn event");

                    Err(if stopped {
                        AgentError::InterruptedByUser(notice.to_string())
                    } else {
                        AgentError::Backend(notice.to_string())
                    })
                })
            });
        let (mut context, db, _queued_messages, _directory) =
            queue_test_context(channel, VecDeque::new(), Status::InProgress).await;
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;

        // Act
        let result = run_channel_turn(
            &context,
            auto_commit_run_client(),
            default_turn_metadata(),
            AgentRequestKind::SessionStart,
            None,
            "test prompt".into(),
        )
        .await;
        let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();

        // Assert
        assert!(result.is_err(), "{result:?}");
        let messages = db
            .sessions()
            .load_session_messages(context.session_id.as_str())
            .await
            .expect("messages");
        let summary = messages
            .iter()
            .position(|message| message.kind == "activity_summary")
            .expect("summary saved");
        assert!(messages[summary].content.starts_with("turn-summary v1\n"));
        assert_eq!(messages[summary + 1].kind, "workflow_notice");
        assert!(messages[summary + 1].content.contains(notice), "{stopped}");
        let cleared = events
            .iter()
            .position(|event| matches!(event, AppEvent::SessionTurnCleared { .. }))
            .expect("live timeline cleared");
        let saved_before_clear = events[..cleared]
            .iter()
            .filter(|event| matches!(event, AppEvent::SessionUpdated { .. }))
            .count();
        assert!(saved_before_clear >= 2, "{stopped}: {events:?}");
    }
}

#[tokio::test]
/// Verifies compaction notices, including one that arrives inside a burst of
/// progress events, are saved once, in order, before the turn's answer.
async fn test_run_channel_turn_saves_compaction_notices_before_the_answer() {
    // Arrange
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .returning(|_session_id, _req, events| {
            Box::pin(async move {
                for event in [
                    TurnEvent::Notice(TurnNotice::ContextCompacted {
                        model: Some("qwen3.8-flash".to_string()),
                    }),
                    TurnEvent::ThoughtDelta("Thinking".to_string()),
                    TurnEvent::Notice(TurnNotice::ContextCompactionFailed {
                        reason: "invalid output".to_string(),
                    }),
                    TurnEvent::Notice(TurnNotice::ContextCompacted { model: None }),
                ] {
                    events.send(event).expect("turn event");
                }

                Ok(successful_turn_result("done"))
            })
        });
    let (context, db, _queued_messages, _directory) =
        queue_test_context(channel, VecDeque::new(), Status::InProgress).await;

    // Act
    let result = run_channel_turn(
        &context,
        auto_commit_run_client(),
        default_turn_metadata(),
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;

    // Assert
    assert!(result.is_ok(), "{result:?}");
    let messages = db
        .sessions()
        .load_session_messages(context.session_id.as_str())
        .await
        .expect("messages");
    let notices = messages
        .iter()
        .filter(|message| message.kind == "workflow_notice")
        .map(|message| message.content.trim().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        notices,
        [
            "[Context] Summarized earlier turns with `qwen3.8-flash` to fit the context window.",
            "[Context Warning] Could not summarize earlier turns (invalid output); the oldest no \
             longer fit and were dropped from the context.",
            "[Context] Summarized earlier turns to fit the context window.",
        ]
    );
    let position = |kind: &str| messages.iter().position(|message| message.kind == kind);
    assert!(position("workflow_notice") < position("assistant_answer"));
}

#[tokio::test]
async fn test_activity_is_published_before_the_turn_stream_closes() {
    // Arrange
    let (sender, receiver) = mpsc::unbounded_channel();
    let (app_sender, mut app_receiver) = mpsc::unbounded_channel();
    let consumer = tokio::spawn(consume_turn_events(
        receiver,
        app_sender,
        "session".into(),
        Arc::new(Mutex::new(None)),
        fixed_clock(),
    ));

    // Act
    sender
        .send(TurnEvent::Activity(ActivityEvent {
            status: ActivityStatus::Running,
            ..activity_event()
        }))
        .expect("send");
    let update = app_receiver.recv().await.expect("live update");

    // Assert
    let AppEvent::SessionTurnReady { session_id, turn } = update else {
        unreachable!("expected a live timeline");
    };
    assert_eq!(session_id.as_str(), "session");
    let running = turn.snapshot();
    assert!(!consumer.is_finished());

    // Act
    sender
        .send(TurnEvent::ThoughtDelta("thinking".into()))
        .expect("send");
    sender
        .send(TurnEvent::Activity(activity_event()))
        .expect("send");
    drop(sender);
    let activity = consumer.await.expect("consumer");
    let remaining = std::iter::from_fn(|| app_receiver.try_recv().ok()).collect::<Vec<_>>();
    let finished = turn.refresh().expect("changed timeline");

    // Assert
    assert_eq!(
        running
            .current
            .as_ref()
            .map(|current| current.text.as_str()),
        Some("Calling Read")
    );
    assert!(remaining.iter().any(|event| matches!(event,
        AppEvent::SessionProgressUpdated { progress_message: Some(progress), .. }
            if progress == "Calling Read"
    )));
    assert!(matches!(
        &finished.steps[0].rows[0],
        TimelineRow::Tool(row) if row.state == RowState::Succeeded
    ));
    assert_eq!(
        activity.summary().render(None),
        "turn-summary v1\n1 tool · 0s\nUsed\tRead ×1\n─ Worked 0s · 1 tool"
    );
}

#[tokio::test]
/// Verifies a Harness turn refreshes its project's Cargo cache for the main
/// checkout and worktree it resolved, while another agent's turn does not.
async fn test_run_channel_turn_refreshes_the_cargo_cache_for_harness_turns_only() {
    // Arrange
    let harness = AgentSelection::new(AgentKind::Harness, AgentModel::KimiK3);
    let other = default_turn_metadata().session_agent;

    // Act
    let (harness_result, harness_refreshes, folder, main_checkout) =
        run_turn_with_cargo_cache(harness).await;
    let (other_result, other_refreshes, _, _) = run_turn_with_cargo_cache(other).await;

    // Assert
    assert!(harness_result.is_ok(), "{harness_result:?}");
    assert!(other_result.is_ok(), "{other_result:?}");
    assert_eq!(harness_refreshes, [(main_checkout, folder)]);
    assert_eq!(other_refreshes, []);
}

/// Runs one successful turn of `session_agent` in a worktree of a resolved
/// main checkout with a scripted Cargo cache, and returns the turn result,
/// the cache refreshes it started, the worktree, and the main checkout.
async fn run_turn_with_cargo_cache(
    session_agent: AgentSelection,
) -> (
    Result<(), SessionError>,
    Vec<(PathBuf, PathBuf)>,
    PathBuf,
    PathBuf,
) {
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_in_progress_test_session(&db).await;
    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .once()
        .returning(|_session_id, _req, _events| {
            Box::pin(async { Ok(successful_turn_result("done")) })
        });
    let main_checkout = base_dir.path().join("main");
    let mut mock_git_client = mock_git_client_detecting_main_repo(main_checkout.clone());
    mock_git_client
        .expect_tracked_worktree_status()
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_ref_hash()
        .returning(|_, _| Box::pin(async { Ok("base-commit".to_string()) }));
    mock_git_client
        .expect_diff_stats()
        .returning(|_, _| Box::pin(async { Ok(DiffStats::default()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel();
    let mut cargo_cache = MockCargoCacheClient::new();
    cargo_cache
        .expect_refresh()
        .returning(move |main_checkout, worktree, _| {
            let _ = refresh_tx.send((main_checkout, worktree));

            Ok(())
        });
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        available_agent_kinds: Arc::from(AgentKind::ALL),
        graph_activity: GraphActivityRegistry::default(),
        harness_models: HarnessModels::default(),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        cargo_cache: CargoCacheWarmer::new(Arc::new(cargo_cache)),
        session_run: SessionRunClient::from_channel("sess1".to_string(), Arc::new(mock_channel)),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: empty_transcript(),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent,
        status: Arc::new(Mutex::new(Status::InProgress)),
    };
    let mut turn_metadata = default_turn_metadata();
    turn_metadata.session_agent = session_agent;

    let result = run_channel_turn(
        &context,
        auto_commit_run_client(),
        turn_metadata,
        AgentRequestKind::SessionStart,
        None,
        "test prompt".into(),
    )
    .await;
    // The refresh task owns the only sender once the context is gone.
    drop(context);
    let mut refreshes = Vec::new();
    while let Some(refresh) = refresh_rx.recv().await {
        refreshes.push(refresh);
    }

    (
        result,
        refreshes,
        base_dir.path().to_path_buf(),
        main_checkout,
    )
}
