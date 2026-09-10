use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ag_agent::{AgentRequestKind, MockAgentChannel, TurnResult};
use ag_protocol::AgentResponse;
use tempfile::tempdir;

use super::super::{AT_MENTION_INDEX_TTL, TurnAppliedState, remote_branch_name_from_upstream_ref};
use super::support::{
    TestClock, add_manual_session_with_status, create_mock_backend, new_test_app,
    new_test_app_with_git, new_test_app_with_git_and_db, test_session_manager,
    test_session_manager_with_clock, wait_for_status,
};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::file_entry::FileEntry;
use crate::domain::session::{SessionStats, Status};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn test_apply_turn_applied_state_clears_active_prompt_and_resolution_loader() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(temp_dir.path().to_path_buf()).await;
    add_manual_session_with_status(
        &mut app,
        temp_dir.path(),
        "session-id",
        "Prompt",
        Status::InProgress,
    );
    app.sessions
        .set_active_prompt_output("session-id", " › Prompt\n\n".to_string());
    app.sessions.sessions_mut()[0]
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Resolving 1 review comment...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::ReviewCommentResolution,
            turn_position: None,
        });

    // Act
    app.sessions.apply_turn_applied_state(
        "session-id",
        &TurnAppliedState {
            follow_up_tasks: Vec::new(),
            questions: Vec::new(),
            token_usage_delta: SessionStats::default(),
        },
    );

    // Assert
    assert!(
        !app.sessions
            .active_prompt_outputs()
            .contains_key("session-id")
    );
    assert!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
}

#[test]
fn test_set_and_get_at_mention_index_for_root_cache() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    let entries = vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }];

    // Act
    session_manager.set_at_mention_index_for_root(lookup_root.clone(), entries.clone());

    // Assert
    assert_eq!(
        session_manager
            .at_mention_index_for_root(&lookup_root)
            .expect("expected cached entries"),
        entries
    );
}

#[tokio::test]
/// Verifies that the first reply after a model switch replays the full
/// session transcript and subsequent replies omit the replay snapshot.
///
/// A completion channel (`done_tx`/`done_rx`) is used to signal from
/// inside the mock's async block so that `wait_for_status` always sees the
/// worker in `InProgress` and correctly polls until `Review`. Without this,
/// `wait_for_status` would return immediately because the initial status
/// is already `Review` before the worker runs.
async fn test_reply_with_backend_replays_history_once_after_model_switch() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;

    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let initial_output = " › Initial prompt\n\nmock-start\n".to_string();
    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.transcript = Some(crate::test_support::assistant_transcript(&initial_output));
        session.prompt = "Initial prompt".to_string();
        session.status = Status::Review;
    }
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str()) {
        if let Ok(mut transcript) = handles.transcript.lock() {
            *transcript = crate::test_support::assistant_transcript(&initial_output);
        }
        if let Ok(mut status) = handles.status.lock() {
            *status = Status::Review;
        }
    }

    // Persist the prompt so that `RefreshSessions` reloads from DB with the
    // correct value. `update_status(Review)` emits `RefreshSessions`, which
    // reloads sessions from DB; without persisting here, `session.prompt`
    // would be reset to `""` causing the second reply to use
    // `AgentRequestKind::SessionStart`.
    app.services
        .db()
        .sessions()
        .update_session_prompt(&session_id, "Initial prompt")
        .await
        .expect("failed to persist initial prompt");

    app.set_session_model(
        &session_id,
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
    )
    .await
    .expect("failed to switch model");

    // Shared state to capture replay transcript text from each turn request.
    let captured_replay_transcripts: Arc<Mutex<Vec<Option<String>>>> =
        Arc::new(Mutex::new(Vec::new()));

    // The done channel signals from inside the mock future so the test
    // can wait on each turn completing before calling `wait_for_status`.
    // This prevents `wait_for_status` from returning immediately when the
    // session is already in `Review` before the worker processes the turn.
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    // Register a MockAgentChannel that collects replay transcript values from
    // resume turns so they can be asserted synchronously after the test.
    let mut mock_channel = MockAgentChannel::new();
    let captured = Arc::clone(&captured_replay_transcripts);
    let done_capture = done_tx.clone();
    mock_channel.expect_run_turn().returning(move |_, req, _| {
        if matches!(req.request_kind, AgentRequestKind::SessionResume) {
            captured
                .lock()
                .expect("lock poisoned")
                .push(req.continuation.replay_transcript().map(str::to_string));
        }
        let done = done_capture.clone();
        Box::pin(async move {
            let _ = done.send(());
            Ok(TurnResult {
                assistant_message: AgentResponse::plain(""),
                context_reset: false,
                input_tokens: 0,
                output_tokens: 0,
                provider_conversation_id: None,
            })
        })
    });
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));
    app.sessions
        .worker_service
        .test_agent_channels
        .insert(session_id.clone().into(), Arc::new(mock_channel));

    // Act — first reply after model switch: history should be replayed.
    app.sessions
        .reply(&app.services, &session_id, "Switch reply")
        .await;
    done_rx.recv().await.expect("first turn completion signal");
    wait_for_status(&mut app, &session_id, Status::Review).await;

    // Act — second reply: no history replay expected.
    app.sessions
        .reply(&app.services, &session_id, "Second reply")
        .await;
    done_rx.recv().await.expect("second turn completion signal");
    wait_for_status(&mut app, &session_id, Status::Review).await;

    // Assert
    let outputs = captured_replay_transcripts
        .lock()
        .expect("lock poisoned")
        .clone();
    assert_eq!(outputs.len(), 2, "expected exactly two Resume turns");
    let first_replay_transcript = outputs[0]
        .as_deref()
        .expect("first reply should include replay transcript");
    assert!(
        first_replay_transcript.contains("Initial prompt"),
        "first reply should replay history containing 'Initial prompt'"
    );
    assert!(
        outputs[1].is_none(),
        "second reply should not replay history"
    );
}

#[tokio::test]
async fn test_retain_active_prompt_outputs_keeps_only_active_sessions() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(temp_dir.path().to_path_buf()).await;
    add_manual_session_with_status(
        &mut app,
        temp_dir.path(),
        "active-session",
        "Prompt",
        Status::InProgress,
    );
    add_manual_session_with_status(
        &mut app,
        temp_dir.path(),
        "review-session",
        "Prompt",
        Status::Review,
    );
    app.sessions
        .set_active_prompt_output("active-session", " › Active\n\n".to_string());
    app.sessions
        .set_active_prompt_output("review-session", " › Review\n\n".to_string());

    // Act
    app.sessions.retain_active_prompt_outputs();

    // Assert
    assert!(
        app.sessions
            .active_prompt_outputs()
            .contains_key("active-session")
    );
    assert!(
        !app.sessions
            .active_prompt_outputs()
            .contains_key("review-session")
    );
}

#[test]
fn test_retain_active_prompt_outputs_prunes_expired_at_mention_indexes() {
    // Arrange
    let initial_instant = Instant::now();
    let initial_system_time = SystemTime::UNIX_EPOCH + Duration::from_mins(1);
    let clock = Arc::new(TestClock::new(initial_instant, initial_system_time));
    let mut session_manager = test_session_manager_with_clock("session-id", None, clock.clone());
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    session_manager.set_at_mention_index_for_root(
        lookup_root.clone(),
        vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }],
    );
    clock.advance(AT_MENTION_INDEX_TTL + Duration::from_secs(1));

    // Act
    session_manager.retain_active_prompt_outputs();

    // Assert
    assert!(
        session_manager
            .at_mention_index_for_root(&lookup_root)
            .is_none()
    );
}

#[tokio::test]
async fn test_reply_first_message_uses_full_prompt_text_as_title() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let prompt = "Line one\nLine two with more words for title text";
    let backend = create_mock_backend();

    // Act
    app.sessions
        .reply_with_backend(
            &app.services,
            &session_id,
            prompt,
            Arc::new(backend),
            AgentModel::Gemini38Flash,
        )
        .await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].title, Some(prompt.to_string()));
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(db_sessions[0].title, Some(prompt.to_string()));
}

#[test]
fn test_remote_branch_name_returns_input_for_bare_ref() {
    // Arrange / Act
    let branch = remote_branch_name_from_upstream_ref("no-slash");

    // Assert
    assert_eq!(branch, "no-slash");
}

#[test]
fn test_at_mention_index_for_root_invalidates_after_ttl_expires() {
    // Arrange
    let initial_instant = Instant::now();
    let initial_system_time = SystemTime::UNIX_EPOCH + Duration::from_mins(1);
    let clock = Arc::new(TestClock::new(initial_instant, initial_system_time));
    let mut session_manager = test_session_manager_with_clock("session-id", None, clock.clone());
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    let entries = vec![FileEntry {
        is_dir: true,
        path: "src".to_string(),
    }];
    session_manager.set_at_mention_index_for_root(lookup_root.clone(), entries);
    clock.advance(AT_MENTION_INDEX_TTL + Duration::from_secs(1));

    // Act
    let cached_entries = session_manager.at_mention_index_for_root(&lookup_root);

    // Assert
    assert!(cached_entries.is_none());
}
