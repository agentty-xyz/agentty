use std::future::Future;
use std::io;
use std::io::ErrorKind;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mockall::Sequence;
use mockall::predicate::eq;
use tokio::sync::mpsc;

use super::{
    EventSource, MockEventSource, TERMINAL_EVENT_DRAIN_BUDGET, process_event_with_key_handler,
    process_events, process_events_with_handler, process_events_with_scroll_handler,
    process_paste_event, spawn_event_reader, spawn_event_reader_with_source,
};
use crate::app::{App, AppEvent};
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::domain::session::{Session, SessionRole, SessionSize, SessionStats, Status};
use crate::domain::transient_message::TransientMessageStore;
use crate::infra::clock::Clock;
use crate::presentation::app_mode::{
    AppMode, ChatFocus, DiffFocus, DiffLineCommentAnchor, DiffLineCommentTarget, DiffLineComments,
    DiffLineSide, DiffPreview,
};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};
use crate::presentation::setting::SettingsAction;
use crate::runtime::{EventResult, FRAME_INTERVAL, PresentationState};

/// Continues a test cycle while asserting no terminal event was produced.
fn continue_without_terminal_event<'handler>(
    _app: &'handler mut App,
    _terminal: &'handler mut (),
    event: Option<Event>,
) -> Pin<Box<dyn Future<Output = io::Result<EventResult>> + 'handler>> {
    assert_eq!(
        event.into_iter().count(),
        0,
        "reader failures must not become events"
    );

    Box::pin(std::future::ready(Ok(EventResult::Continue)))
}

/// Continues after a key event without applying mode behavior.
fn continue_for_key_event<'handler>(
    _app: &'handler mut App,
    _terminal: &'handler mut (),
    _key: KeyEvent,
) -> Pin<Box<dyn Future<Output = io::Result<EventResult>> + 'handler>> {
    Box::pin(std::future::ready(Ok(EventResult::Continue)))
}

/// Verifies the production reader wiring honors a pre-requested shutdown
/// without polling the concrete terminal source.
#[test]
fn test_spawn_event_reader_exits_when_shutdown_is_already_requested() {
    // Arrange
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(true));

    // Act
    let join_result = spawn_event_reader(event_tx, shutdown).join();

    // Assert
    assert!(join_result.is_ok());
    assert!(event_rx.try_recv().is_err());
}

/// Verifies the event reader forwards one queued event before stopping on
/// a poll error.
#[tokio::test]
async fn test_spawn_event_reader_with_source_forwards_event_to_channel() {
    // Arrange
    let mut mock_source = MockEventSource::new();
    let mut sequence = Sequence::new();
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(true));
    mock_source
        .expect_read()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|| {
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            )))
        });
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(ErrorKind::BrokenPipe, "stop")));
    let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Act
    let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
    let received_event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
        .await
        .expect("timed out waiting for event")
        .expect("failed to receive event")
        .expect("event reader returned an error");
    join_handle
        .join()
        .expect("failed to join event reader thread");

    // Assert
    assert!(matches!(received_event, Event::Key(_)));
}

/// Verifies the reader exits cleanly when the event receiver is already
/// gone.
#[test]
fn test_spawn_event_reader_with_source_stops_when_receiver_is_dropped() {
    // Arrange
    let mut mock_source = MockEventSource::new();
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .returning(|_| Ok(true));
    mock_source.expect_read().times(1).returning(|| {
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )))
    });
    let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    drop(event_rx);
    let shutdown = Arc::new(AtomicBool::new(false));

    // Act
    let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
    let join_result = join_handle.join();

    // Assert
    assert!(join_result.is_ok());
}

/// Verifies an interrupted poll is retried before a later fatal failure is
/// forwarded.
#[test]
fn test_spawn_event_reader_with_source_retries_interrupted_poll() {
    // Arrange
    let mut mock_source = MockEventSource::new();
    let mut sequence = Sequence::new();
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(ErrorKind::Interrupted, "retry")));
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(ErrorKind::BrokenPipe, "stop")));
    mock_source.expect_read().times(0);
    let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Act
    let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
    let join_result = join_handle.join();
    let queued_error = event_rx
        .try_recv()
        .expect("fatal poll failure should be forwarded after retry")
        .expect_err("reader should forward the fatal poll error");

    // Assert
    assert!(join_result.is_ok());
    assert_eq!(queued_error.kind(), ErrorKind::BrokenPipe);
    assert_eq!(queued_error.to_string(), "stop");
    assert!(event_rx.try_recv().is_err());
}

/// Verifies a false poll result skips reads before forwarding the next
/// fatal poll failure.
#[test]
fn test_spawn_event_reader_with_source_forwards_poll_error_after_empty_poll() {
    // Arrange
    let mut mock_source = MockEventSource::new();
    let mut sequence = Sequence::new();
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(false));
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(ErrorKind::BrokenPipe, "stop")));
    mock_source.expect_read().times(0);
    let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Act
    let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
    let join_result = join_handle.join();
    let queued_error = event_rx
        .try_recv()
        .expect("poll failure should be forwarded")
        .expect_err("reader should forward the poll error");

    // Assert
    assert!(join_result.is_ok());
    assert_eq!(queued_error.kind(), ErrorKind::BrokenPipe);
    assert_eq!(queued_error.to_string(), "stop");
}

/// Verifies an interrupted read is retried from polling instead of being
/// forwarded to the runtime.
#[test]
fn test_spawn_event_reader_with_source_retries_interrupted_read() {
    // Arrange
    let mut mock_source = MockEventSource::new();
    let mut sequence = Sequence::new();
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(true));
    mock_source
        .expect_read()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|| Err(io::Error::new(ErrorKind::Interrupted, "retry")));
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(ErrorKind::BrokenPipe, "stop")));
    let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Act
    let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
    let join_result = join_handle.join();
    let queued_error = event_rx
        .try_recv()
        .expect("fatal poll failure should be forwarded after read retry")
        .expect_err("reader should forward the fatal poll error");

    // Assert
    assert!(join_result.is_ok());
    assert_eq!(queued_error.kind(), ErrorKind::BrokenPipe);
    assert_eq!(queued_error.to_string(), "stop");
    assert!(event_rx.try_recv().is_err());
}

/// Verifies terminal read failures are forwarded before the reader exits.
#[test]
fn test_spawn_event_reader_with_source_forwards_read_error() {
    // Arrange
    let mut mock_source = MockEventSource::new();
    mock_source
        .expect_poll()
        .with(eq(FRAME_INTERVAL))
        .once()
        .returning(|_| Ok(true));
    mock_source
        .expect_read()
        .once()
        .returning(|| Err(io::Error::new(ErrorKind::BrokenPipe, "read failed")));
    let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Act
    let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
    let join_result = join_handle.join();
    let queued_error = event_rx
        .try_recv()
        .expect("read failure should be forwarded")
        .expect_err("reader should forward the read error");

    // Assert
    assert!(join_result.is_ok());
    assert_eq!(queued_error.kind(), ErrorKind::BrokenPipe);
    assert_eq!(queued_error.to_string(), "read failed");
}

/// Verifies a pre-set shutdown flag exits the reader without touching the
/// event source.
#[test]
fn test_spawn_event_reader_with_source_exits_when_shutdown_is_already_requested() {
    // Arrange
    let mut mock_source = MockEventSource::new();
    mock_source.expect_poll().times(0);
    mock_source.expect_read().times(0);
    let event_source: Arc<dyn EventSource> = Arc::new(mock_source);
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let shutdown = Arc::new(AtomicBool::new(true));

    // Act
    let join_handle = spawn_event_reader_with_source(event_source, event_tx, shutdown);
    let join_result = join_handle.join();

    // Assert
    assert!(join_result.is_ok());
}

/// Verifies pasted text is routed into prompt input without invoking the
/// key handler.
#[tokio::test]
async fn test_process_event_with_key_handler_pastes_into_prompt_mode() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = "session-1".to_string();
    app.sessions.push_session(Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: std::env::temp_dir(),
        follow_up_tasks: Vec::new(),
        id: session_id.clone().into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            crate::domain::agent::AgentKind::Antigravity.default_model(),
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: "project".to_string(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: crate::domain::agent::SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::Draft,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    });
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::default(),
        input: InputState::default(),
        scroll_offset: None,
        session_id: session_id.into(),
        slash_state: PromptSlashState::default(),
    };
    let mut terminal = ();

    // Act
    let result = process_event_with_key_handler(
        &mut app,
        &mut terminal,
        Some(Event::Paste("    line 1\r\n        line 2".to_string())),
        |_, (), _| Box::pin(async { Err(io::Error::other("unexpected key-handler call")) }),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(
        matches!(&app.mode, AppMode::Prompt { input, .. } if input.text() == "    line 1\n        line 2")
    );
}

/// Verifies pasted text updates question input in free-text mode.
#[tokio::test]
async fn test_process_event_with_key_handler_pastes_into_question_free_text_mode() {
    // Arrange — paste only works in free-text mode (`selected_option_index`
    // is `None`).
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        scroll_offset: None,
        questions: vec![QuestionItem {
            options: vec!["yes".to_string()],
            text: "Is this enough?".to_string(),
        }],
        responses: Vec::new(),
        selected_option_index: None,
        session_id: "session-1".into(),
    };
    let mut terminal = ();

    // Act
    let result = process_event_with_key_handler(
        &mut app,
        &mut terminal,
        Some(Event::Paste("custom\ranswer".to_string())),
        |_, (), _| Box::pin(async { Err(io::Error::other("unexpected key-handler call")) }),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(
        &app.mode,
        AppMode::Question {
            input,
            selected_option_index: None,
            ..
        } if input.text() == "custom\nanswer"
    ));
}

#[tokio::test]
async fn test_process_event_with_key_handler_pastes_into_inline_diff_comment() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut line_comments = DiffLineComments::default();
    line_comments.start_editing_target(DiffLineCommentTarget::single(DiffLineCommentAnchor {
        content: "review();".to_string(),
        line: 1,
        path: "src/main.rs".to_string(),
        side: DiffLineSide::New,
    }));
    app.mode = AppMode::Diff {
        diff: "diff --git a/src/main.rs b/src/main.rs\n+review();\n".to_string(),
        file_explorer_selected_index: 1,
        focus: DiffFocus::Content,
        line_comments,
        preview: DiffPreview::default(),
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session_id: "session-1".into(),
    };
    let mut terminal = ();

    // Act
    let result = process_event_with_key_handler(
        &mut app,
        &mut terminal,
        Some(Event::Paste("first line\r\nsecond line".to_string())),
        continue_for_key_event,
    )
    .await;
    let key_result = process_event_with_key_handler(
        &mut app,
        &mut terminal,
        Some(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))),
        continue_for_key_event,
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(key_result, Ok(EventResult::Continue)));
    assert!(matches!(
        &app.mode,
        AppMode::Diff { line_comments, .. }
            if line_comments.comments[0].input.text() == "first line\nsecond line"
    ));
}

#[tokio::test]
async fn test_process_paste_event_updates_publish_branch_input() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::PublishBranchInput {
        default_branch_name: "wt/session".to_string(),
        input: InputState::default(),
        locked_upstream_ref: None,
        publish_branch_action: crate::domain::session::PublishBranchAction::Push,
        restore_view: crate::presentation::app_mode::ConfirmationViewMode {
            scroll_offset: None,
            session_id: "session-1".into(),
        },
    };

    // Act
    process_paste_event(&mut app, "review/shared-input\r\nignored").await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::PublishBranchInput { input, .. }
            if input.text() == "review/shared-input"
    ));
}

#[tokio::test]
async fn test_process_paste_event_updates_launch_configuration_input() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.tabs.set(crate::app::Tab::Settings);
    for _ in 0..7 {
        let view = app.settings.view();
        let _ = app.settings_presentation.apply(&view, SettingsAction::Next);
    }
    let view = app.settings.view();
    let _ = app
        .settings_presentation
        .apply(&view, SettingsAction::Activate);
    let view = app.settings.view();
    let _ = app
        .settings_presentation
        .apply(&view, SettingsAction::StartAddingLaunchConfiguration);

    // Act
    process_paste_event(&mut app, "cargo nextest run\r\nignored").await;

    // Assert
    let editor = app
        .settings_presentation
        .snapshot(&app.settings.view())
        .launch_configuration_list_editor
        .expect("launch-configuration editor should be open");
    assert!(matches!(
        editor.input,
        Some(ref input) if input.text() == "cargo nextest run"
    ));
}

/// Verifies non-key terminal events are ignored by the runtime handler.
#[tokio::test]
async fn test_process_event_with_key_handler_ignores_resize_events() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let original_mode = AppMode::List;
    app.mode = original_mode;
    let mut terminal = ();

    // Act
    let result = process_event_with_key_handler(
        &mut app,
        &mut terminal,
        Some(Event::Resize(120, 40)),
        |_, (), _| Box::pin(async { Err(io::Error::other("unexpected key-handler call")) }),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(&app.mode, AppMode::List));
}

/// Verifies key release events are ignored even when keyboard enhancement
/// flags make them visible to the runtime.
#[tokio::test]
async fn test_process_event_with_key_handler_ignores_key_release_events() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut terminal = ();

    // Act
    let result = process_event_with_key_handler(
        &mut app,
        &mut terminal,
        Some(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::ALT,
            KeyEventKind::Release,
        ))),
        |_, (), _| Box::pin(async { Err(io::Error::other("unexpected key-handler call")) }),
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
}

/// Verifies handler errors terminate the outer event-processing cycle.
#[tokio::test]
async fn test_process_events_with_handler_returns_handler_error() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut terminal = ();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    event_tx
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))))
        .expect("failed to queue event");
    let mut tick = tokio::time::interval(Duration::from_mins(1));

    // Act
    let result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        |_, (), _| Box::pin(async { Err(io::Error::other("handler failed")) }),
    )
    .await;

    // Assert
    assert!(result.is_err());
    let error = result
        .err()
        .expect("handler error should exit the event loop");
    assert_eq!(error.to_string(), "handler failed");
}

/// Verifies a terminal reader failure exits the event cycle without being
/// converted into a terminal event.
#[tokio::test]
async fn test_process_events_with_handler_returns_reader_error() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut terminal = ();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    let initial_result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        continue_without_terminal_event,
    )
    .await;
    event_tx
        .send(Err(io::Error::new(ErrorKind::BrokenPipe, "reader failed")))
        .expect("failed to queue reader error");

    // Act
    let result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        continue_without_terminal_event,
    )
    .await;

    // Assert
    assert!(matches!(initial_result, Ok(EventResult::Continue)));
    let error = result
        .err()
        .expect("reader failure should exit the event cycle");
    assert_eq!(error.kind(), ErrorKind::BrokenPipe);
    assert_eq!(error.to_string(), "reader failed");
}

/// Verifies a closed terminal channel exits instead of spinning through
/// no-input cycles.
#[tokio::test]
async fn test_process_events_with_handler_returns_error_when_reader_channel_closes() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut terminal = ();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<io::Result<Event>>();
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    let initial_result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        continue_without_terminal_event,
    )
    .await;
    drop(event_tx);

    // Act
    let result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        continue_without_terminal_event,
    )
    .await;

    // Assert
    assert!(matches!(initial_result, Ok(EventResult::Continue)));
    let error = result
        .err()
        .expect("closed reader channel should exit the event cycle");
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    assert_eq!(error.to_string(), "terminal event reader stopped");
}

#[tokio::test]
async fn test_process_events_with_handler_drives_session_runtime_commands() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut terminal = ();
    let (_event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    tick.tick().await;
    let _session_runtime_consumer = app.sessions.foreground_consumer();
    let service = app.session_service();
    let lookup = tokio::spawn(async move {
        service
            .get_session(&ag_session::SessionId::from("missing"))
            .await
    });

    // Act
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let result = process_events_with_handler(
                &crate::test_support::FixedClock::unix_epoch(),
                &mut app,
                &mut terminal,
                &mut event_rx,
                &mut tick,
                |_, (), event| {
                    assert!(event.is_none());

                    Box::pin(async { Ok(EventResult::Continue) })
                },
            )
            .await;
            assert!(matches!(&result, Ok(EventResult::Continue)));

            tokio::task::yield_now().await;
            if lookup.is_finished() {
                break result;
            }
        }
    })
    .await
    .expect("runtime should process the session command");
    let session = lookup
        .await
        .expect("lookup task should finish")
        .expect("lookup should succeed");

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert_eq!(session, None);
}

#[tokio::test]
async fn test_process_events_with_handler_drives_app_runtime_events() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.services.emit_app_event(AppEvent::RefreshSessions);
    let mut terminal = ();
    let (_event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    tick.tick().await;

    // Act
    let result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        |_, (), event| {
            assert!(event.is_none());

            Box::pin(async { Ok(EventResult::Continue) })
        },
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
}

/// Verifies queued terminal events beyond the foreground budget stay in
/// the channel for the next render cycle.
#[tokio::test]
async fn test_process_events_with_handler_keeps_events_over_budget_queued() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut terminal = ();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    for _ in 0..(TERMINAL_EVENT_DRAIN_BUDGET + 2) {
        event_tx
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            ))))
            .expect("failed to queue event");
    }
    let handled_events = Arc::new(AtomicUsize::new(0));
    let mut tick = tokio::time::interval(Duration::from_mins(1));

    // Act
    let result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        {
            let handled_events = Arc::clone(&handled_events);

            move |_, (), event| {
                if event.is_some() {
                    handled_events.fetch_add(1, Ordering::Relaxed);
                }

                Box::pin(async { Ok(EventResult::Continue) })
            }
        },
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert_eq!(
        handled_events.load(Ordering::Relaxed),
        TERMINAL_EVENT_DRAIN_BUDGET
    );
    assert_eq!(event_rx.len(), 2);
}

/// Verifies channel closure noticed while draining queued input exits the
/// event cycle after handling the final event.
#[tokio::test]
async fn test_process_events_with_handler_returns_error_when_channel_closes_during_drain() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut terminal = ();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    event_tx
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))))
        .expect("failed to queue event");
    drop(event_tx);
    let handled_events = Arc::new(AtomicUsize::new(0));
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    tick.tick().await;

    // Act
    let result = process_events_with_handler(
        &crate::test_support::FixedClock::unix_epoch(),
        &mut app,
        &mut terminal,
        &mut event_rx,
        &mut tick,
        {
            let handled_events = Arc::clone(&handled_events);

            move |_, (), event| {
                if event.is_some() {
                    handled_events.fetch_add(1, Ordering::Relaxed);
                }

                Box::pin(async { Ok(EventResult::Continue) })
            }
        },
    )
    .await;

    // Assert
    let error = result
        .err()
        .expect("closed reader channel should exit during drain");
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    assert_eq!(handled_events.load(Ordering::Relaxed), 1);
}

struct BatchClock {
    elapsed_millis: AtomicUsize,
    start: std::time::Instant,
}

impl Clock for BatchClock {
    fn now_instant(&self) -> std::time::Instant {
        self.start + Duration::from_millis(self.elapsed_millis.load(Ordering::Relaxed) as u64)
    }

    fn now_system_time(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH
    }
}

#[tokio::test]
async fn test_event_batch_yields_when_elapsed_budget_is_reached() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let clock = BatchClock {
        elapsed_millis: AtomicUsize::new(0),
        start: std::time::Instant::now(),
    };
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    for _ in 0..3 {
        event_tx
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
            ))))
            .expect("queue key");
    }
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    tick.tick().await;
    let mut handled = 0;

    // Act
    let result = process_events_with_handler(
        &clock,
        &mut app,
        &mut (),
        &mut event_rx,
        &mut tick,
        |_, (), event| {
            if event.is_some() {
                handled += 1;
                clock.elapsed_millis.store(8, Ordering::Relaxed);
            }

            Box::pin(async { Ok(EventResult::Continue) })
        },
    )
    .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert_eq!(handled, 1);
    assert_eq!(event_rx.len(), 2);
    assert_eq!(clock.now_system_time(), std::time::SystemTime::UNIX_EPOCH);
}

#[tokio::test]
async fn test_production_event_batch_scrolls_then_leaves_mermaid_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session = crate::test_support::SessionFixtureBuilder::new()
        .status(Status::Review)
        .transcript("```mermaid\ngraph TD\nA --> B\n```\n".repeat(20))
        .build();
    app.mode = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: Some(0),
    };
    app.sessions.push_session(session);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("terminal");
    let presentation = Rc::new(PresentationState::default());
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    for key in ['j', 'j', 'k'] {
        event_tx
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::NONE,
            ))))
            .expect("queue scroll");
    }
    event_tx
        .send(Ok(Event::Resize(80, 24)))
        .expect("queue resize");
    event_tx
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        ))))
        .expect("queue navigation");
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    tick.tick().await;

    // Act
    while !event_rx.is_empty() {
        process_events(
            &mut app,
            Rc::clone(&presentation),
            &mut terminal,
            &mut event_rx,
            &mut tick,
        )
        .await
        .expect("dispatch input");
    }

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(app.needs_redraw());
}

#[tokio::test]
async fn test_event_batch_propagates_scroll_measurement_error() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session = crate::test_support::SessionFixtureBuilder::new()
        .status(Status::Review)
        .build();
    app.mode = AppMode::View {
        session_id: session.id.clone(),
        scroll_offset: Some(3),
    };
    app.sessions.push_session(session);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("terminal");
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
    event_tx.send(Event::Key(key)).expect("queue scroll");
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    tick.tick().await;

    // Act
    let result = process_events_with_scroll_handler(
        &mut app,
        Rc::new(PresentationState::default()),
        &mut terminal,
        &mut event_rx,
        &mut tick,
        |_, _, _, _, received_key| {
            assert_eq!(received_key, key);

            Err(io::Error::new(
                ErrorKind::BrokenPipe,
                "terminal size unavailable",
            ))
        },
    )
    .await;

    // Assert
    let error = result.err().expect("scroll failure must reach the caller");
    assert_eq!(error.kind(), ErrorKind::BrokenPipe);
    assert_eq!(error.to_string(), "terminal size unavailable");
    assert!(event_rx.is_empty());
    assert!(matches!(
        app.mode,
        AppMode::View {
            scroll_offset: Some(3),
            ..
        }
    ));
}
