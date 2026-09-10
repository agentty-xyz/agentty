use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_git as git;
use tempfile::tempdir;
use tokio::sync::Barrier;

use super::super::{SessionManager, session_folder};
use super::support::{
    add_manual_session, install_mock_git_client, new_test_app, new_test_app_with_db,
    session_replay_text,
};
use crate::app::session::SessionLoadInput;
use crate::domain::agent::AgentModel;
use crate::domain::session::{
    DailyActivity, SESSION_DATA_DIR, SessionHandles, SessionId, Status,
    activity_day_key_with_offset,
};
use crate::infra::clock::{Clock, RealClock};
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, HelpContext,
};

#[tokio::test]
async fn test_load_sessions_aggregates_daily_activity() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .insert_session("gamma000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert gamma000");
    let seconds_per_day = 86_400_i64;
    let day_key_one = 10_i64;
    let day_key_two = 11_i64;

    db.sessions()
        .update_session_created_at("alpha000", day_key_one * seconds_per_day + 10)
        .await
        .expect("failed to update alpha000 created_at");
    db.sessions()
        .update_session_created_at("beta0000", day_key_one * seconds_per_day + 600)
        .await
        .expect("failed to update beta0000 created_at");
    db.sessions()
        .update_session_created_at("gamma000", day_key_two * seconds_per_day + 50)
        .await
        .expect("failed to update gamma000 created_at");
    db.activity()
        .clear_session_activity()
        .await
        .expect("failed to clear session activity");
    db.activity()
        .backfill_session_activity_from_sessions()
        .await
        .expect("failed to backfill session activity from session rows");
    let working_dir = PathBuf::from("/tmp/test");
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();
    let mut expected_activity_by_day: BTreeMap<i64, u32> = BTreeMap::new();
    for timestamp_seconds in [
        day_key_one * seconds_per_day + 10,
        day_key_one * seconds_per_day + 600,
        day_key_two * seconds_per_day + 50,
    ] {
        let day_key = activity_day_key_with_offset(
            timestamp_seconds,
            RealClock.local_utc_offset_seconds(timestamp_seconds),
        );
        let day_count = expected_activity_by_day.entry(day_key).or_insert(0);
        *day_count = day_count.saturating_add(1);
    }
    let expected_activity: Vec<DailyActivity> = expected_activity_by_day
        .into_iter()
        .map(|(day_key, session_count)| DailyActivity {
            day_key,
            session_count,
        })
        .collect();

    // Act
    let fs_client = fs::RealFsClient;
    let (sessions, stats_activity, _) = SessionManager::load_sessions_with_fs_client(
        SessionLoadInput {
            active_project_id: project_id,
            active_session_id: None,
            base: dir.path(),
            clock: &RealClock,
            db: &db,
            fs_client: &fs_client,
            working_dir: &working_dir,
        },
        &mut handles,
    )
    .await;

    // Assert
    assert_eq!(sessions.len(), 3);
    assert_eq!(stats_activity, expected_activity);
}

#[tokio::test]
async fn test_load_sessions_invalid_path() {
    // Arrange
    let path = PathBuf::from("/invalid/path/that/does/not/exist");

    // Act
    let app = new_test_app(path).await;

    // Assert
    assert!(app.sessions.sessions().is_empty());
}

#[tokio::test]
async fn test_load_existing_sessions() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("12345678", "claude-opus-4-6", "main", "Done", project_id)
        .await
        .expect("failed to insert");

    let session_dir = dir.path().join("12345678");
    let data_dir = session_dir.join(SESSION_DATA_DIR);
    std::fs::create_dir(&session_dir).expect("failed to create session dir");
    std::fs::create_dir(&data_dir).expect("failed to create data dir");
    db.sessions()
        .update_session_prompt("12345678", "Existing")
        .await
        .expect("failed to update prompt");
    db.sessions()
        .append_session_message(
            "12345678",
            crate::domain::session_message::SessionMessageKind::AssistantAnswer,
            "Output",
        )
        .await
        .expect("failed to append message");

    // Act
    let app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;

    // Assert
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, "12345678");
    assert_eq!(
        app.sessions.sessions()[0].agent.model(),
        AgentModel::ClaudeOpus5
    );
    assert_eq!(app.sessions.sessions()[0].prompt, "Existing");
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert_eq!(output, "Output\n\n");
    assert_eq!(app.sessions.selected_session_index(), Some(0));
}

#[tokio::test]
async fn test_refresh_session_branch_names_runs_detection_concurrently() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session(&mut app, dir.path(), "alpha001", "First");
    add_manual_session(&mut app, dir.path(), "bravo002", "Second");
    let barrier = Arc::new(Barrier::new(2));
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .times(2)
        .returning({
            let barrier = Arc::clone(&barrier);
            move |_| {
                let barrier = Arc::clone(&barrier);

                Box::pin(async move {
                    barrier.wait().await;

                    None
                })
            }
        });
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let refresh_result = tokio::time::timeout(
        Duration::from_millis(100),
        app.sessions.refresh_session_branch_names(),
    )
    .await;

    // Assert
    assert!(
        refresh_result.is_ok(),
        "branch refresh should complete without serially blocking"
    );
    assert_eq!(
        app.sessions.session_branch_name("alpha001"),
        Some("wt/alpha001")
    );
    assert_eq!(
        app.sessions.session_branch_name("bravo002"),
        Some("wt/bravo002")
    );
}

#[tokio::test]
async fn test_load_done_session_without_folder_kept() {
    // Arrange — DB has a terminal row but no matching folder on disk
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("missing01", "gemini-3.8-flash", "main", "Done", project_id)
        .await
        .expect("failed to insert");

    // Act
    let app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;

    // Assert — terminal session is kept even after folder cleanup
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, "missing01");
    assert_eq!(app.sessions.sessions()[0].status, Status::Done);
}

#[tokio::test]
async fn test_refresh_sessions_if_needed_reloads_and_preserves_selection() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "alpha000",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .update_session_updated_at("alpha000", 1)
        .await
        .expect("failed to set alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2)
        .await
        .expect("failed to set beta0000 timestamp");
    for session_id in ["alpha000", "beta0000"] {
        let session_dir = session_folder(dir.path(), session_id);
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
    }
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;
    app.sessions.select_session_index(Some(1));

    // Act
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("alpha000", "Done", 0)
        .await
        .expect("failed to update session status");
    app.refresh_sessions_now().await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].id, "alpha000");
    let selected_index = app
        .sessions
        .selected_session_index()
        .expect("missing selection");
    assert_eq!(app.sessions.sessions()[selected_index].id, "alpha000");
}

#[tokio::test]
async fn test_refresh_sessions_loads_diff_help_detail_when_another_session_is_selected() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha000", "gemini-3.8-flash", "main", "Review", project_id)
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .update_session_prompt("alpha000", "Alpha prompt")
        .await
        .expect("failed to set alpha000 prompt");
    db.sessions()
        .update_session_prompt("beta0000", "Beta prompt")
        .await
        .expect("failed to set beta0000 prompt");
    db.sessions()
        .update_session_updated_at("alpha000", 1)
        .await
        .expect("failed to set alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2)
        .await
        .expect("failed to set beta0000 timestamp");
    for session_id in ["alpha000", "beta0000"] {
        let session_dir = session_folder(dir.path(), session_id);
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
    }
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;
    let help_session_id = SessionId::from("alpha000");
    let selected_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == "beta0000")
        .expect("beta0000 should be loaded");
    app.sessions.select_session_index(Some(selected_index));
    app.mode = AppMode::Help {
        context: HelpContext::Diff {
            can_comment: true,
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            session_id: help_session_id.clone(),
            scroll_offset: 0,
        },
        scroll_offset: 0,
    };

    // Act
    app.refresh_sessions_now().await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Help {
            context: HelpContext::Diff { ref session_id, .. },
            ..
        } if session_id == &help_session_id
    ));
    assert_eq!(
        app.sessions
            .selected_session()
            .map(|session| session.id.as_str()),
        Some("beta0000")
    );
    assert_eq!(
        app.sessions
            .session_for_id(&help_session_id)
            .map(|session| session.prompt.as_str()),
        Some("Alpha prompt")
    );
    assert_eq!(
        app.sessions
            .session_for_id("beta0000")
            .map(|session| session.prompt.as_str()),
        Some("")
    );
}

#[tokio::test]
async fn test_load_in_progress_session_without_folder_skipped() {
    // Arrange — DB has a non-terminal row but no matching folder on disk
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "missing02",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert");

    // Act
    let app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;

    // Assert — non-terminal session is skipped because folder doesn't exist
    assert!(app.sessions.sessions().is_empty());
}
