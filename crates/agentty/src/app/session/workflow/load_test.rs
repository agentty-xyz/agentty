use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use ag_git::{GitError, MockGitClient};

use super::super::session_folder;
use super::{
    SessionLoadInput, load_session_transcript, merge_loaded_session_status,
    migrate_active_sessions_off_retired_models, migrate_session_off_retired_model,
    parse_questions_json, parse_review_request, should_skip_missing_folder_session,
    sync_handle_transcript_with_loaded,
};
use crate::app::SessionManager;
use crate::domain::agent::AgentModel;
use crate::domain::permission::PermissionMode;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    DailyActivity, ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, Session,
    SessionDiffState, SessionDiffStats, SessionHandles, SessionId, SessionSize, Status,
};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot,
};
use crate::infra::clock::{Clock, RealClock};
use crate::infra::db::{
    AppRepositories, DbError, SessionListRow, SessionPreparationRow, SessionPreparationState,
    SessionReviewRequestRow,
};
use crate::infra::fs;

#[test]
fn test_workspace_preparation_shows_failures_and_clears_them_on_retry() {
    // Arrange
    let mut session = crate::test_support::session_fixture("preparing", Status::Draft);
    let mut preparation = SessionPreparationRow {
        error: Some("setup failed".to_string()),
        prompt: None,
        session_id: session.id.to_string(),
        start_ref: "main".to_string(),
        state: SessionPreparationState::Failed,
    };

    // Act
    SessionManager::apply_workspace_preparation(&mut session, Some(&preparation));

    // Assert
    let notice = session
        .transient_messages
        .get(TransientMessageSlot::WorkspacePreparation)
        .expect("setup failure remains visible");
    assert!(matches!(&notice.body, TransientMessageBody::Plain(text)
            if text.contains("setup failed") && text.contains("Press s to retry")));

    // Act
    preparation.state = SessionPreparationState::Preparing;
    SessionManager::apply_workspace_preparation(&mut session, Some(&preparation));

    // Assert
    let notice = session
        .transient_messages
        .get(TransientMessageSlot::WorkspacePreparation)
        .expect("preparation remains available to lifecycle actions");
    assert!(matches!(&notice.body, TransientMessageBody::Plain(text) if text.is_empty()));
    assert!(session.allows_cancel_action());
}

/// Clock fixture that supplies event-specific offsets for activity rows.
struct ActivityOffsetClock;

impl Clock for ActivityOffsetClock {
    fn local_utc_offset_seconds(&self, timestamp_seconds: i64) -> i64 {
        if timestamp_seconds < 86_400 {
            3_600
        } else {
            -3_600
        }
    }

    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

fn session_replay_text(session: &Session) -> String {
    session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default()
}

fn assistant_transcript(content: impl AsRef<str>) -> SessionTranscript {
    SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        content.as_ref(),
    )])
}

fn assistant_replay_text(content: impl AsRef<str>) -> String {
    assistant_transcript(content)
        .replay_text()
        .expect("assistant transcript should have replay text")
}

#[tokio::test]
async fn load_sessions_skips_invalid_permission_mode_without_hiding_valid_siblings() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("project should be created");
    for session_id in ["valid-mode", "invalid-mode"] {
        db.sessions()
            .insert_draft_session(session_id, "gpt-5.6-sol", "main", "Draft", project_id)
            .await
            .expect("session should be created");
    }
    sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'invalid-mode'")
        .execute(&pool)
        .await
        .expect("permission mode should be corrupted");
    let mock_fs_client = create_folder_lookup_mock(Vec::new());
    let mut handles = HashMap::new();

    // Act
    let (sessions, _, session_worktree_availability) =
        SessionManager::load_sessions_with_fs_client(
            SessionLoadInput {
                active_project_id: project_id,
                active_session_id: None,
                base: Path::new("/virtual/session-base"),
                clock: &RealClock,
                db: &db,
                fs_client: &mock_fs_client,
                working_dir: Path::new("/tmp/test"),
            },
            &mut handles,
        )
        .await;

    // Assert
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "valid-mode");
    assert_eq!(sessions[0].permission_mode, PermissionMode::AutoEdit);
    assert!(handles.contains_key("valid-mode"));
    assert!(!handles.contains_key("invalid-mode"));
    assert_eq!(
        session_worktree_availability.get("valid-mode"),
        Some(&false)
    );
    assert!(!session_worktree_availability.contains_key("invalid-mode"));
}

#[test]
fn daily_activity_uses_clock_offset_for_each_timestamp() {
    // Arrange
    let timestamps = vec![86_399, 86_400, 86_399];
    let clock = ActivityOffsetClock;

    // Act
    let activity = SessionManager::daily_activity_from_timestamps(timestamps, &clock);
    let monotonic_time = clock.now_instant();
    let system_time = clock.now_system_time();

    // Assert
    assert!(monotonic_time <= Instant::now());
    assert_eq!(system_time, SystemTime::UNIX_EPOCH);
    assert_eq!(
        activity,
        vec![
            DailyActivity {
                day_key: 0,
                session_count: 1,
            },
            DailyActivity {
                day_key: 1,
                session_count: 2,
            },
        ]
    );
}

#[tokio::test]
async fn session_diff_stats_preserve_binary_presence_and_git_errors() {
    // Arrange
    let folder = PathBuf::from("/tmp/session");
    let existing_folder_client = create_folder_lookup_mock(vec![folder.clone()]);
    let missing_folder_client = create_folder_lookup_mock(Vec::new());
    let mut binary_diff_client = MockGitClient::new();
    binary_diff_client.expect_diff().times(1).returning(|_, _| {
        Box::pin(async {
            Ok("diff --git a/image.png b/image.png\nBinary files differ\n".to_string())
        })
    });
    let mut failing_diff_client = MockGitClient::new();
    failing_diff_client
        .expect_diff()
        .times(1)
        .returning(|_, _| {
            Box::pin(async { Err(GitError::OutputParse("diff failed".to_string())) })
        });

    // Act
    let binary_stats = SessionManager::session_diff_stats_for_folder(
        &existing_folder_client,
        &binary_diff_client,
        &folder,
        "main",
    )
    .await;
    let error_stats = SessionManager::session_diff_stats_for_folder(
        &existing_folder_client,
        &failing_diff_client,
        &folder,
        "main",
    )
    .await;
    let missing_folder_stats = SessionManager::session_diff_stats_for_folder(
        &missing_folder_client,
        &MockGitClient::new(),
        &folder,
        "main",
    )
    .await;

    // Assert
    assert_eq!(
        binary_stats,
        SessionDiffStats::Known {
            added_lines: 0,
            deleted_lines: 0,
            has_diff: true,
            session_size: SessionSize::Xs,
        }
    );
    assert_eq!(error_stats, SessionDiffStats::Unknown);
    assert_eq!(missing_folder_stats, SessionDiffStats::Unknown);
}

/// Returns a filesystem mock that reports the supplied directories as
/// existing and treats missing staged-draft metadata files as absent.
fn create_folder_lookup_mock(existing_folders: Vec<PathBuf>) -> fs::MockFsClient {
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_is_dir()
        .times(0..)
        .returning(move |path| existing_folders.contains(&path));
    mock_fs_client.expect_read_file().times(0..).returning(|_| {
        Box::pin(async {
            Err(fs::FsError::Io(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            )))
        })
    });

    mock_fs_client
}

/// Ensures reload keeps live handle output and active status when
/// persisted row data is stale.
#[tokio::test]
async fn test_load_sessions_preserves_live_handle_output_and_status() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");

    let session_id = "test-session";
    db.sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.sessions()
        .append_session_message(session_id, SessionMessageKind::AssistantAnswer, "DB Output")
        .await
        .expect("failed to append persisted message");
    db.sessions()
        .mark_session_diff_unknown(session_id)
        .await
        .expect("failed to mark session diff unknown");

    let base_path = Path::new("/virtual/session-base");
    let session_dir = session_folder(base_path, session_id);
    let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
    let live_output = "Live Output".to_string();
    let live_status = Status::Review;
    handles.insert(
        session_id.to_string().into(),
        SessionHandles::new_with_transcript(live_status, assistant_transcript(&live_output)),
    );

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(
        session_replay_text(session),
        assistant_replay_text(&live_output)
    );
    assert_eq!(session.status, live_status);
    assert_eq!(session.stats.diff_state, SessionDiffState::Unknown);

    let handle = handles
        .get(session_id)
        .expect("missing existing runtime handle");
    let handle_output = handle
        .transcript
        .lock()
        .expect("failed to lock handle transcript")
        .replay_text()
        .unwrap_or_default();
    let handle_status = *handle.status.lock().expect("failed to lock handle status");
    assert_eq!(handle_output, assistant_replay_text(&live_output));
    assert_eq!(handle_status, live_status);
}

/// Ensures project-scoped reloads reconstruct queued workflow rows from
/// the live session handles that still own their worker commands.
#[tokio::test]
async fn test_load_sessions_restores_queued_actions_from_live_handles() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let session_id = SessionId::from("queued-session");
    db.sessions()
        .insert_session(
            &session_id,
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let base_path = Path::new("/virtual/session-base");
    let mock_fs_client = create_folder_lookup_mock(vec![session_folder(base_path, &session_id)]);
    let handles = SessionHandles::new(Status::InProgress);
    handles.upsert_queued_action(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(
            3,
            "sync after this turn".to_string(),
        )),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::SyncQueue,
        turn_position: Some(0),
    });
    let mut handles_by_session = HashMap::from([(session_id.clone(), handles)]);

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: Some(&session_id),
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles_by_session,
    )
    .await;

    // Assert
    let queued_action = sessions[0]
        .transient_messages
        .get(TransientMessageSlot::SyncQueue)
        .expect("queued sync should be restored");
    assert!(matches!(
        &queued_action.body,
        TransientMessageBody::Queued(action)
            if action.order == 3 && action.text == "sync after this turn"
    ));
}

/// Ensures reload caches worktree availability alongside loaded session
/// rows.
#[tokio::test]
async fn test_load_sessions_reports_worktree_availability() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let session_with_worktree_id = "worktree-available";
    let session_without_worktree_id = "draft-missing";
    db.sessions()
        .insert_session(
            session_with_worktree_id,
            "gemini-3.8-flash",
            "main",
            "Draft",
            project_id,
        )
        .await
        .expect("failed to insert session with worktree");
    db.sessions()
        .insert_draft_session(
            session_without_worktree_id,
            "gemini-3.8-flash",
            "main",
            "Draft",
            project_id,
        )
        .await
        .expect("failed to insert draft session");

    let base_path = Path::new("/virtual/session-base");
    let mock_fs_client =
        create_folder_lookup_mock(vec![session_folder(base_path, session_with_worktree_id)]);
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

    // Act
    let (_, _, session_worktree_availability) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    assert_eq!(
        session_worktree_availability.get(session_with_worktree_id),
        Some(&true)
    );
    assert_eq!(
        session_worktree_availability.get(session_without_worktree_id),
        Some(&false)
    );
}

/// Ensures reload reads persisted detail for active sessions.
#[tokio::test]
async fn test_load_sessions_reads_persisted_detail_for_active_session() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");

    let session_id = "test-session";
    db.sessions()
        .insert_session(session_id, "gemini-3.8-flash", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_prompt(session_id, "persisted prompt")
        .await
        .expect("failed to update session prompt");
    db.sessions()
        .update_session_questions(
            session_id,
            r#"[{"text":"persisted question?","options":["Yes"]}]"#,
        )
        .await
        .expect("failed to update session questions");
    db.sessions()
        .append_session_message(
            session_id,
            SessionMessageKind::AssistantAnswer,
            "persisted output",
        )
        .await
        .expect("failed to append session message");

    let base_path = Path::new("/virtual/session-base");
    let session_dir = session_folder(base_path, session_id);
    let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
    handles.insert(
        session_id.to_string().into(),
        SessionHandles::new_with_transcript(Status::Review, assistant_transcript("Live Output")),
    );

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: Some(session_id),
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(
        session_replay_text(session),
        assistant_replay_text("Live Output")
    );
    assert_eq!(session.prompt, "persisted prompt");
    assert_eq!(
        session.questions,
        vec![QuestionItem {
            options: vec!["Yes".to_string()],
            text: "persisted question?".to_string(),
        }]
    );
}

/// Ensures inactive session refresh skips transcript-scale fields.
#[tokio::test]
async fn test_load_sessions_defers_persisted_detail_for_inactive_session() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");

    let session_id = "inactive-session";
    db.sessions()
        .insert_session(session_id, "gemini-3.8-flash", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_prompt(session_id, "large prompt")
        .await
        .expect("failed to update prompt");
    db.sessions()
        .update_session_questions(session_id, r#"["Need detail?"]"#)
        .await
        .expect("failed to update questions");
    db.sessions()
        .append_session_message(
            session_id,
            SessionMessageKind::AssistantAnswer,
            "large output",
        )
        .await
        .expect("failed to append message");

    let base_path = Path::new("/virtual/session-base");
    let session_dir = session_folder(base_path, session_id);
    let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(session_replay_text(session), "");
    assert_eq!(session.prompt, "");
    assert_eq!(session.questions, [] as [ag_protocol::QuestionItem; 0]);
    let handle = handles.get(session_id).expect("missing runtime handle");
    let handle_output = handle
        .transcript
        .lock()
        .expect("failed to lock transcript")
        .replay_text();
    assert_eq!(handle_output, None);
}

/// Ensures active reload hydrates an existing empty handle from persisted
/// transcript detail.
#[tokio::test]
async fn test_load_sessions_hydrates_empty_handle_for_active_session() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");

    let session_id = "active-session";
    db.sessions()
        .insert_session(session_id, "gemini-3.8-flash", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    db.sessions()
        .append_session_message(
            session_id,
            SessionMessageKind::AssistantAnswer,
            "persisted output",
        )
        .await
        .expect("failed to append message");

    let base_path = Path::new("/virtual/session-base");
    let session_dir = session_folder(base_path, session_id);
    let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
    handles.insert(
        session_id.to_string().into(),
        SessionHandles::new_unloaded(Status::Review),
    );

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: Some(session_id),
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(
        session_replay_text(session),
        assistant_replay_text("persisted output")
    );

    let handle = handles.get(session_id).expect("missing runtime handle");
    let handle_output = handle
        .transcript
        .lock()
        .expect("failed to lock transcript")
        .replay_text()
        .unwrap_or_default();
    assert_eq!(handle_output, assistant_replay_text("persisted output"));
}

/// Ensures transcript loading returns database failures instead of
/// converting them into empty transcript text.
#[tokio::test]
async fn test_load_session_transcript_returns_query_errors() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query!("DROP TABLE session_message")
        .execute(&pool)
        .await
        .expect("failed to drop session_message table");

    // Act
    let error = load_session_transcript(&db, "missing-session")
        .await
        .expect_err("transcript load should fail");

    // Assert
    assert!(matches!(error, DbError::Query(_)));
}

/// Ensures terminal persisted statuses replace stale active handle status
/// during reload.
#[tokio::test]
async fn test_load_sessions_terminal_db_status_overrides_handle_status() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");

    let session_id = "test-session";
    db.sessions()
        .insert_session(session_id, "gemini-3.8-flash", "main", "Done", project_id)
        .await
        .expect("failed to insert session");

    let base_path = Path::new("/virtual/session-base");
    let session_dir = session_folder(base_path, session_id);
    let mock_fs_client = create_folder_lookup_mock(vec![session_dir]);

    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
    handles.insert(
        session_id.to_string().into(),
        SessionHandles::new_with_transcript(Status::Review, assistant_transcript("output")),
    );

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(session.status, Status::Done);

    let handle = handles
        .get(session_id)
        .expect("missing existing runtime handle");
    let handle_status = *handle.status.lock().expect("failed to lock handle status");
    assert_eq!(handle_status, Status::Done);
}

/// Ensures still-active sessions on a retired model are switched to the
/// replacement model both in memory and in the database.
#[tokio::test]
async fn test_load_sessions_switches_active_session_off_retired_model() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let session_id = "retired-active-session";
    db.sessions()
        .insert_session(session_id, "gemini-3.1-pro", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let base_path = Path::new("/virtual/session-base");
    let mock_fs_client = create_folder_lookup_mock(vec![session_folder(base_path, session_id)]);
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(session.agent.model(), AgentModel::Gemini31Pro);
    let row = db
        .sessions()
        .load_session(session_id)
        .await
        .expect("failed to load session row")
        .expect("missing session row");
    assert_eq!(row.model, "gemini-3.1-pro-preview");
    assert_eq!(row.agent, "antigravity");
}

/// Ensures automatic model migration does not make an old session appear
/// recently active.
#[tokio::test]
async fn test_migrate_session_preserves_updated_at() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let session_id = "retired-timestamp-session";
    db.sessions()
        .insert_session(session_id, "claude-opus-4-6", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    sqlx::query(
        r"
UPDATE session
SET updated_at = ?
WHERE id = ?
",
    )
    .bind(123_i64)
    .bind(session_id)
    .execute(&pool)
    .await
    .expect("failed to set historical timestamp");

    // Act
    migrate_session_off_retired_model(&db, session_id, "claude", "claude-opus-4-6", Status::Review)
        .await;

    // Assert
    let row = db
        .sessions()
        .load_session(session_id)
        .await
        .expect("failed to load migrated session")
        .expect("missing migrated session");
    assert_eq!(row.agent, "claude");
    assert_eq!(row.model, "claude-opus-5");
    assert_eq!(row.updated_at, 123);
}

/// Ensures startup migration covers active sessions outside the currently
/// loaded project while retaining terminal-session history.
#[tokio::test]
async fn test_migrate_active_sessions_off_retired_models_covers_inactive_projects() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let active_project_id = db
        .projects()
        .upsert_project("/tmp/active", None)
        .await
        .expect("failed to upsert active project");
    let inactive_project_id = db
        .projects()
        .upsert_project("/tmp/inactive", None)
        .await
        .expect("failed to upsert inactive project");
    db.sessions()
        .insert_session(
            "active-project-session",
            "claude-opus-4-6",
            "main",
            "Review",
            active_project_id,
        )
        .await
        .expect("failed to insert active-project session");
    db.sessions()
        .insert_session(
            "inactive-project-session",
            "gemini-3.5-flash",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("failed to insert inactive-project session");
    db.sessions()
        .insert_session(
            "inactive-project-finished",
            "gemini-3.5-flash",
            "main",
            "Done",
            inactive_project_id,
        )
        .await
        .expect("failed to insert finished inactive-project session");

    // Act
    migrate_active_sessions_off_retired_models(&db).await;

    // Assert
    let active_project_row = db
        .sessions()
        .load_session("active-project-session")
        .await
        .expect("failed to load active-project session")
        .expect("missing active-project session");
    let inactive_project_row = db
        .sessions()
        .load_session("inactive-project-session")
        .await
        .expect("failed to load inactive-project session")
        .expect("missing inactive-project session");
    let finished_row = db
        .sessions()
        .load_session("inactive-project-finished")
        .await
        .expect("failed to load finished inactive-project session")
        .expect("missing finished inactive-project session");
    assert_eq!(active_project_row.model, "claude-opus-5");
    assert_eq!(active_project_row.agent, "claude");
    assert_eq!(inactive_project_row.model, "gemini-3.5-flash-lite");
    assert_eq!(inactive_project_row.agent, "antigravity");
    assert_eq!(finished_row.model, "gemini-3.5-flash");
}

/// Ensures a terminal transition after the migration read wins the race
/// and preserves the retired model id as history.
#[tokio::test]
async fn test_migrate_session_preserves_retired_model_after_terminal_transition() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let race_cases = [
        ("race-merged", "Merged"),
        ("race-done", "Done"),
        ("race-canceled", "Canceled"),
    ];
    for (session_id, _) in race_cases {
        db.sessions()
            .insert_session(session_id, "claude-opus-4-6", "main", "Review", project_id)
            .await
            .expect("failed to insert active session");
    }
    let stale_rows = db
        .sessions()
        .load_active_session_agent_models()
        .await
        .expect("failed to load active sessions");
    for (session_id, terminal_status) in race_cases {
        db.sessions()
            .update_session_status_with_timing_at(session_id, terminal_status, 1)
            .await
            .expect("failed to persist terminal transition");
    }

    // Act
    for row in stale_rows {
        let stale_status = row
            .status
            .parse::<Status>()
            .expect("active status should parse");
        migrate_session_off_retired_model(&db, &row.id, &row.agent, &row.model, stale_status).await;
    }

    // Assert
    for (session_id, terminal_status) in race_cases {
        let row = db
            .sessions()
            .load_session(session_id)
            .await
            .expect("failed to load transitioned session")
            .expect("missing transitioned session");
        assert_eq!(row.status, terminal_status);
        assert_eq!(row.agent, "claude");
        assert_eq!(row.model, "claude-opus-4-6");
    }
}

/// Ensures startup remains usable when active-session migration cannot
/// query a degraded database.
#[tokio::test]
async fn test_migrate_active_sessions_off_retired_models_ignores_query_failures() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    sqlx::query("DROP TABLE session")
        .execute(&pool)
        .await
        .expect("session table should be dropped");

    // Act
    migrate_active_sessions_off_retired_models(&db).await;

    // Assert
    assert!(
        db.sessions()
            .load_active_session_agent_models()
            .await
            .is_err()
    );
}

/// Ensures finished sessions keep their retired model id in the database
/// as history while loading with the replacement model in memory.
#[tokio::test]
async fn test_load_sessions_keeps_retired_model_in_db_for_finished_session() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let session_id = "retired-finished-session";
    db.sessions()
        .insert_session(session_id, "claude-opus-4-6", "main", "Done", project_id)
        .await
        .expect("failed to insert session");
    let base_path = Path::new("/virtual/session-base");
    let mock_fs_client = create_folder_lookup_mock(Vec::new());
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(session.agent.model(), AgentModel::ClaudeOpus5);
    let row = db
        .sessions()
        .load_session(session_id)
        .await
        .expect("failed to load session row")
        .expect("missing session row");
    assert_eq!(row.model, "claude-opus-4-6");
}

/// Ensures persisted review-request metadata is mapped onto loaded session
/// snapshots.
#[tokio::test]
async fn test_load_sessions_maps_review_request_metadata() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let review_request = ReviewRequest {
        last_refreshed_at: 999,
        summary: ReviewRequestSummary {
            display_id: "#17".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Closed,
            status_summary: Some("closed by maintainer".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/team/project/pull/17".to_string(),
        },
    };

    let session_id = "test-session";
    db.sessions()
        .insert_session(session_id, "gemini-3.8-flash", "main", "Done", project_id)
        .await
        .expect("failed to insert session");
    db.reviews()
        .update_session_review_request(session_id, Some(review_request.clone()))
        .await
        .expect("failed to persist review request metadata");

    let base_path = Path::new("/virtual/session-base");
    let mock_fs_client = create_folder_lookup_mock(Vec::new());
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

    // Act
    let (sessions, _, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: base_path,
            clock: &RealClock,
            db: &db,
            fs_client: &mock_fs_client,
            working_dir: Path::new("/tmp/test"),
        },
        &mut handles,
    )
    .await;

    // Assert
    let session = sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing reloaded session");
    assert_eq!(session.review_request, Some(review_request));
}

#[test]
/// Verifies read-only and terminal DB statuses override stale in-memory
/// handle statuses.
fn merge_loaded_session_status_prefers_read_only_and_terminal_status_from_db() {
    // Arrange
    let status_from_handle = Status::Draft;

    // Act
    let merged_status = merge_loaded_session_status(Status::Merged, status_from_handle);
    let done_status = merge_loaded_session_status(Status::Done, status_from_handle);

    // Assert
    assert_eq!(merged_status, Status::Merged);
    assert_eq!(done_status, Status::Done);
}

#[test]
/// Verifies non-terminal DB statuses do not overwrite in-memory status.
fn merge_loaded_session_status_prefers_handle_for_non_terminal_db_status() {
    // Arrange
    let status_from_db = Status::Review;
    let status_from_handle = Status::InProgress;

    // Act
    let merged_status = merge_loaded_session_status(status_from_db, status_from_handle);

    // Assert
    assert_eq!(merged_status, Status::InProgress);
}

#[test]
/// Verifies loaded message rows do not replace an existing live
/// transcript snapshot.
fn sync_handle_transcript_with_loaded_keeps_existing_live_transcript() {
    // Arrange
    let live_transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "prompt"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "answer"),
    ]);
    let handles = SessionHandles::new_with_transcript(Status::Review, live_transcript.clone());
    let loaded_transcript = assistant_transcript("loaded answer");

    // Act
    let transcript = sync_handle_transcript_with_loaded(&handles, Some(&loaded_transcript));

    // Assert
    assert_eq!(transcript, Some(live_transcript.clone()));
    assert_eq!(
        handles.transcript.lock().ok().as_deref(),
        Some(&live_transcript)
    );
}

#[test]
/// Verifies persisted history merges with a partial workflow notice
/// appended before a lazy transcript was hydrated.
fn sync_handle_transcript_with_loaded_merges_partial_unloaded_transcript() {
    // Arrange
    let handles = SessionHandles::new_unloaded(Status::Review);
    handles
        .transcript
        .lock()
        .expect("transcript lock should not be poisoned")
        .clone_from(&SessionTranscript::new(vec![SessionMessage::new(
            2,
            SessionMessageKind::WorkflowNotice,
            "\n[Sync] Successfully synced onto main\n",
        )]));
    let loaded_transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
    ]);
    let expected_transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
        SessionMessage::new(
            2,
            SessionMessageKind::WorkflowNotice,
            "\n[Sync] Successfully synced onto main\n",
        ),
    ]);

    // Act
    let transcript = sync_handle_transcript_with_loaded(&handles, Some(&loaded_transcript));

    // Assert
    assert_eq!(transcript, Some(expected_transcript.clone()));
    assert_eq!(
        handles.transcript.lock().ok().as_deref(),
        Some(&expected_transcript)
    );
}

#[test]
/// Verifies hydration deduplicates a persisted live append while retaining
/// an unpersisted message whose temporary position conflicts.
fn sync_handle_transcript_with_loaded_merges_matching_and_conflicting_messages() {
    // Arrange
    let handles = SessionHandles::new_unloaded(Status::Review);
    let persisted_notice = SessionMessage::new(
        2,
        SessionMessageKind::WorkflowNotice,
        "\n[Sync] Successfully synced onto main\n",
    );
    handles
        .transcript
        .lock()
        .expect("transcript lock should not be poisoned")
        .clone_from(&SessionTranscript::new(vec![
            SessionMessage::new(
                0,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync Error] persistence failed\n",
            ),
            persisted_notice.clone(),
        ]));
    let loaded_transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
        persisted_notice.clone(),
    ]);

    // Act
    let transcript = sync_handle_transcript_with_loaded(&handles, Some(&loaded_transcript))
        .expect("merged transcript should be available");

    // Assert
    assert_eq!(
        transcript.messages(),
        &[
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "original prompt"),
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "original answer"),
            persisted_notice,
            SessionMessage::new(
                3,
                SessionMessageKind::WorkflowNotice,
                "\n[Sync Error] persistence failed\n"
            ),
        ]
    );
}

#[test]
/// Verifies missing-folder rows stay visible while merge cleanup has
/// removed the worktree before `Done` persistence finishes.
fn should_skip_missing_folder_session_keeps_live_merging_session() {
    // Arrange
    let has_session_folder = false;
    let persisted_status = Status::Merging;
    let live_handle_status = Some(Status::Merging);

    // Act
    let should_skip = should_skip_missing_folder_session(
        has_session_folder,
        false,
        persisted_status,
        live_handle_status,
    );

    // Assert
    assert!(!should_skip);
}

#[test]
/// Verifies missing-folder rows stay visible when either persistence or
/// live state has already recorded a remote merge.
fn should_skip_missing_folder_session_keeps_merged_session() {
    // Arrange, Act
    let persisted_merged_should_skip =
        should_skip_missing_folder_session(false, false, Status::Merged, Some(Status::Review));
    let live_merged_should_skip =
        should_skip_missing_folder_session(false, false, Status::Review, Some(Status::Merged));

    // Assert
    assert!(!persisted_merged_should_skip);
    assert!(!live_merged_should_skip);
}

#[test]
/// Verifies missing-folder non-terminal rows are still filtered when no
/// merge-cleanup transition is active.
fn should_skip_missing_folder_session_skips_orphaned_active_session() {
    // Arrange
    let has_session_folder = false;
    let persisted_status = Status::Review;
    let live_handle_status = None;

    // Act
    let should_skip = should_skip_missing_folder_session(
        has_session_folder,
        false,
        persisted_status,
        live_handle_status,
    );

    // Assert
    assert!(should_skip);
}

#[test]
/// Verifies missing-folder draft sessions stay visible before their
/// deferred worktree is created.
fn should_skip_missing_folder_session_keeps_new_draft_session() {
    // Arrange
    let has_session_folder = false;
    let persisted_status = Status::Draft;
    let live_handle_status = None;

    // Act
    let should_skip = should_skip_missing_folder_session(
        has_session_folder,
        true,
        persisted_status,
        live_handle_status,
    );

    // Assert
    assert!(!should_skip);
}

#[test]
/// Verifies invalid review-request rows are ignored during session load.
fn parse_review_request_returns_none_for_invalid_row() {
    // Arrange
    let row = SessionListRow {
        added_lines: 0,
        agent: "codex".to_string(),
        base_branch: "main".to_string(),
        created_at: 0,
        deleted_lines: 0,
        has_diff: Some(false),
        id: "session-a".to_string(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        input_tokens: 0,
        is_draft: false,
        model: "gpt-5.6-sol".to_string(),
        output_tokens: 0,
        parent_session_id: None,
        permission_mode: "auto_edit".to_string(),
        personality_id: None,
        project_id: Some(1),
        reasoning_level_override: None,
        response_style: "balanced".to_string(),
        published_upstream_ref: None,
        review_request: Some(SessionReviewRequestRow {
            display_id: "#42".to_string(),
            forge_kind: "UnknownForge".to_string(),
            last_refreshed_at: 0,
            source_branch: "feature/forge".to_string(),
            state: "Open".to_string(),
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        }),
        role: None,
        size: "XS".to_string(),
        speed_mode: "normal".to_string(),
        status: "Review".to_string(),
        title: None,
        updated_at: 0,
    };

    // Act
    let review_request = parse_review_request(&row);

    // Assert
    assert_eq!(review_request, None);
}

#[test]
fn test_parse_questions_json_new_format() {
    // Arrange
    let json = r#"[{"text":"Pick one?","options":["A","B"]}]"#;

    // Act
    let result = parse_questions_json(json);

    // Assert
    let items = result.expect("expected Some");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text, "Pick one?");
    assert_eq!(items[0].options, vec!["A", "B"]);
}

#[test]
fn test_parse_questions_json_legacy_format() {
    // Arrange
    let json = r#"["Need target?","Need tests?"]"#;

    // Act
    let result = parse_questions_json(json);

    // Assert
    let items = result.expect("expected Some");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].text, "Need target?");
    assert_eq!(items[0].options, [] as [std::string::String; 0]);
    assert_eq!(items[1].text, "Need tests?");
    assert_eq!(items[1].options, [] as [std::string::String; 0]);
}

#[test]
fn test_parse_questions_json_empty_string_returns_none() {
    // Arrange / Act
    let result = parse_questions_json("");

    // Assert
    assert!(result.is_none());
}

#[test]
fn test_parse_questions_json_invalid_json_returns_none() {
    // Arrange / Act
    let result = parse_questions_json("{not valid json");

    // Assert
    assert!(result.is_none());
}
