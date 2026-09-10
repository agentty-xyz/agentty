use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_agent::{AgentSelectionMetadata, MockAgentChannel};
use ag_git as git;
use ag_protocol::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use ag_session::session_branch;
use tempfile::tempdir;
use tokio::sync::Notify;

use super::super::{SessionCreationKind, SessionDefaults, SessionManager, session_folder};
use super::support::{
    add_manual_session, allow_detect_git_info, create_and_start_session, install_mock_git_client,
    new_test_app, new_test_app_with_db, new_test_app_with_git, new_test_app_with_git_and_db,
    session_replay_text, test_session_manager, wait_for_path_absent,
};
use crate::app::SessionState;
use crate::app::session::SessionLoadInput;
use crate::app::session::workflow::task::SessionTaskService;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::file_entry::FileEntry;
use crate::domain::selection::SelectionState;
use crate::domain::session::{SESSION_DATA_DIR, SessionHandles, SessionId, SessionRole, Status};
use crate::domain::setting::SettingName;
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::fs;

#[tokio::test]
async fn test_create_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            app.projects.active_project_id(),
            SettingName::DefaultSmartReasoningLevel,
            ReasoningLevel::Low.as_str(),
        )
        .await
        .expect("failed to set project reasoning level");

    // Act
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    // Assert — blank session
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, session_id);
    assert_eq!(app.sessions.sessions()[0].prompt, "");
    assert_eq!(app.sessions.sessions()[0].title, None);
    assert_eq!(app.sessions.sessions()[0].display_title(), "No title");
    assert!(!app.sessions.sessions()[0].is_draft_session());
    assert_eq!(app.sessions.sessions()[0].status, Status::Draft);
    assert_eq!(app.sessions.selected_session_index(), Some(0));
    assert_eq!(
        app.sessions.sessions()[0].agent.model(),
        AgentKind::Gemini.default_model()
    );

    // Check filesystem
    let session_dir = &app.sessions.sessions()[0].folder;
    let data_dir = session_dir.join(SESSION_DATA_DIR);
    assert!(session_dir.exists());
    assert!(data_dir.is_dir());

    // Check DB
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let activity_timestamps = app
        .services
        .db()
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");
    assert_eq!(db_sessions.len(), 1);
    assert_eq!(db_sessions[0].base_branch, "main");
    assert_eq!(
        db_sessions[0].model,
        AgentKind::Gemini.default_model().as_str()
    );
    assert!(!db_sessions[0].is_draft);
    assert_eq!(db_sessions[0].status, "Draft");
    assert_eq!(
        db_sessions[0].reasoning_level_override.as_deref(),
        Some(ReasoningLevel::Low.as_str())
    );
    assert_eq!(activity_timestamps.len(), 1);
}

#[tokio::test]
async fn test_create_session_propagates_project_reasoning_read_failure() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), database).await;
    sqlx::query!("DROP TABLE project_setting")
        .execute(&pool)
        .await
        .expect("failed to drop project settings table");

    // Act
    let result = app.create_session().await;

    // Assert
    assert!(result.is_err());
    assert!(app.sessions.sessions().is_empty());
}

#[tokio::test]
async fn test_create_session_keeps_default_smart_model_setting_when_session_model_changes() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let first_session_id = app
        .create_session()
        .await
        .expect("failed to create first session");
    app.set_session_model(
        &first_session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set session model");
    let active_project_id = app.active_project_id();
    let default_smart_model_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartModel)
        .await
        .expect("failed to load setting");
    let default_smart_agent_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartAgent)
        .await
        .expect("failed to load agent setting");

    // Act
    let second_session_id = app
        .create_session()
        .await
        .expect("failed to create second session");

    // Assert
    let second_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == second_session_id)
        .expect("missing second session");
    assert_eq!(
        second_session.agent.model(),
        AgentKind::Gemini.default_model()
    );
    assert_eq!(default_smart_model_setting, None);
    assert_eq!(default_smart_agent_setting, None);

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_second_session = db_sessions
        .iter()
        .find(|session| session.id == second_session_id)
        .expect("missing second session in db");
    assert_eq!(
        db_second_session.model,
        AgentKind::Gemini.default_model().as_str()
    );
}

#[tokio::test]
async fn test_create_session_persists_default_smart_model_setting_when_last_used_model_is_enabled()
{
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
    let active_project_id = app.active_project_id();
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            active_project_id,
            SettingName::LastUsedModelAsDefault,
            "true",
        )
        .await
        .expect("failed to upsert last-used-model setting");
    let first_session_id = app
        .create_session()
        .await
        .expect("failed to create first session");

    // Act
    app.set_session_model(
        &first_session_id,
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
    )
    .await
    .expect("failed to set session model");
    let default_smart_model_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartModel)
        .await
        .expect("failed to load setting");
    let default_smart_agent_setting = app
        .services
        .db()
        .settings()
        .get_project_setting(active_project_id, SettingName::DefaultSmartAgent)
        .await
        .expect("failed to load agent setting");
    drop(app);
    let mut restarted_app = new_test_app_with_git_and_db(dir.path(), db).await;
    let second_session_id = restarted_app
        .create_session()
        .await
        .expect("failed to create second session");

    // Assert
    assert_eq!(
        default_smart_model_setting,
        Some(AgentModel::Gpt56Sol.as_str().to_string())
    );
    assert_eq!(
        default_smart_agent_setting,
        Some(AgentKind::Codex.name().to_string())
    );
    let second_session = restarted_app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == second_session_id)
        .expect("missing second session");
    assert_eq!(second_session.agent.kind(), AgentKind::Codex);
    assert_eq!(second_session.agent.model(), AgentModel::Gpt56Sol);
}

#[tokio::test]
async fn test_create_session_reads_default_smart_model_and_speed_from_db_settings() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let active_project_id = app.active_project_id();
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            active_project_id,
            SettingName::DefaultSmartModel,
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to upsert default smart model setting");
    app.services
        .db()
        .settings()
        .upsert_project_setting(
            active_project_id,
            SettingName::DefaultSmartSpeedMode,
            SpeedMode::Fast.as_str(),
        )
        .await
        .expect("failed to upsert default smart speed setting");

    // Act
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    // Assert
    let created_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing created session");
    assert_eq!(created_session.agent.model(), AgentModel::ClaudeOpus5);
    assert_eq!(created_session.agent.kind(), AgentKind::Claude);
    assert_eq!(created_session.speed_mode, SpeedMode::Fast);
}

#[tokio::test]
async fn test_create_session_uses_default_smart_model_setting_and_most_recent_permission_mode() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project(&dir.path().to_string_lossy(), Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session("alpha0001", "gemini-3.8-flash", "main", "Done", project_id)
        .await
        .expect("failed to insert alpha0001");
    db.sessions()
        .insert_session(
            "beta00002",
            AgentModel::ClaudeHaiku4520251001.as_str(),
            "main",
            "Done",
            project_id,
        )
        .await
        .expect("failed to insert beta00002");
    db.settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultSmartModel,
            AgentModel::ClaudeHaiku4520251001.as_str(),
        )
        .await
        .expect("failed to upsert default smart model setting");
    db.sessions()
        .update_session_updated_at("alpha0001", 1_i64)
        .await
        .expect("failed to update alpha0001 timestamp");
    db.sessions()
        .update_session_updated_at("beta00002", 2_i64)
        .await
        .expect("failed to update beta00002 timestamp");
    for session_id in ["alpha0001", "beta00002"] {
        let session_dir = session_folder(dir.path(), session_id);
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create session data dir");
    }
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;

    // Act
    let created_session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    // Assert
    let created_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == created_session_id)
        .expect("missing created session");
    assert_eq!(
        created_session.agent.model(),
        AgentModel::ClaudeHaiku4520251001
    );
}

#[tokio::test]
async fn test_create_session_without_git() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;

    // Act
    let result = app.create_session().await;

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("Git branch is required")
    );
    assert!(app.sessions.sessions().is_empty());
}

#[tokio::test]
async fn test_create_session_with_git_no_actual_repo() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        Some("main".to_string()),
        db,
    )
    .await;
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(|_| Box::pin(async { None }));
    mock_git_client.expect_remove_worktree().never();
    mock_git_client.expect_delete_branch().never();
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let result = app.create_session().await;

    // Assert - should fail because git repo doesn't actually exist
    assert!(result.is_err());
    assert!(
        result
            .expect_err("should be error")
            .to_string()
            .contains("git repository root")
    );
    assert!(
        app.services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions")
            .is_empty()
    );
}

#[tokio::test]
async fn test_create_session_cleans_up_on_error() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        Some("main".to_string()),
        db,
    )
    .await;
    let repo_root = dir.path().to_path_buf();
    let mut mock_git_client = git::MockGitClient::new();
    allow_detect_git_info(&mut mock_git_client);
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock_git_client
        .expect_create_worktree()
        .times(1)
        .returning(|_, _, _, _| {
            Box::pin(async {
                Err(git::GitError::OutputParse(
                    "mock create_worktree failed".to_string(),
                ))
            })
        });
    mock_git_client.expect_remove_worktree().never();
    mock_git_client.expect_delete_branch().never();
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let result = app.create_session().await;

    // Assert - session should not be created
    assert!(result.is_err());
    assert_eq!(app.sessions.sessions().len(), 0);
    assert!(
        app.services
            .db()
            .sessions()
            .load_sessions()
            .await
            .expect("sessions")
            .is_empty()
    );

    // Verify no session folder was left behind
    let entries = std::fs::read_dir(dir.path())
        .expect("failed to read dir")
        .count();
    assert_eq!(entries, 0, "Session folder should be cleaned up on error");
}

#[tokio::test]
async fn test_create_session_scoped_to_project() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let project_id = app.active_project_id();

    // Act
    app.create_session()
        .await
        .expect("failed to create session");

    // Assert — session belongs to the active project
    let sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].project_id, Some(project_id));
}

#[tokio::test]
async fn test_delete_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "A").await;
    let session_id = app.sessions.sessions()[0].id.clone();
    let session_folder = app.sessions.sessions()[0].folder.clone();
    app.sessions.set_at_mention_index_for_root(
        session_folder.clone(),
        vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }],
    );
    let session_update_versions = app.services.session_update_versions();
    SessionTaskService::remove_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );
    let initial_version = SessionTaskService::next_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );
    assert_eq!(initial_version, 1);

    // Act
    app.delete_selected_session().await;
    let reset_version = SessionTaskService::next_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );

    // Assert
    assert!(app.sessions.sessions().is_empty());
    assert_eq!(app.sessions.selected_session_index(), None);
    assert!(!session_folder.exists());
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert!(db_sessions.is_empty());
    assert!(
        app.sessions
            .at_mention_index_for_root(&session_folder)
            .is_none()
    );
    assert_eq!(reset_version, 1);

    SessionTaskService::remove_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );
}

#[tokio::test]
async fn test_delete_session_without_git() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app(dir.path().to_path_buf()).await;
    add_manual_session(&mut app, dir.path(), "manual01", "Test");

    // Act
    app.delete_selected_session().await;

    // Assert
    assert_eq!(app.sessions.sessions().len(), 0);
}

#[tokio::test]
async fn test_load_sessions_keeps_daily_activity_after_session_deletion() {
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
    db.activity()
        .insert_session_creation_activity_at("alpha000", 10)
        .await
        .expect("failed to persist first activity event");
    db.activity()
        .insert_session_creation_activity_at("beta0000", 20)
        .await
        .expect("failed to persist second activity event");
    db.sessions()
        .delete_session("alpha000")
        .await
        .expect("failed to delete alpha000");
    let working_dir = PathBuf::from("/tmp/test");
    let mut handles: HashMap<SessionId, SessionHandles> = HashMap::new();

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
    assert_eq!(sessions.len(), 1);
    let total_activity_count: u32 = stats_activity
        .iter()
        .map(|daily_activity| daily_activity.session_count)
        .sum();
    assert_eq!(total_activity_count, 2);
}

#[tokio::test]
async fn test_create_stacked_draft_session_persists_parent_and_base_branch() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let expected_base_branch = session_branch(&parent_session_id);

    // Act
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");

    // Assert
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    assert!(child_session.is_draft_session());
    assert_eq!(child_session.status, Status::Draft);
    assert_eq!(
        child_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(child_session.base_branch, expected_base_branch);
    assert!(!child_session.folder.exists());

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_child_session = db_sessions
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing persisted child session");
    assert_eq!(
        db_child_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(db_child_session.base_branch, expected_base_branch);
}

#[tokio::test]
async fn test_create_stacked_draft_session_chains_five_drafts_and_rejects_sixth() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let root_session_id = app.create_session().await.expect("failed to create root");
    let mut parent_session_id = root_session_id;
    for _ in 1..=5 {
        parent_session_id = app
            .create_stacked_draft_session(&parent_session_id)
            .await
            .expect("failed to create nested stack level");
    }

    // Act
    let result = app.create_stacked_draft_session(&parent_session_id).await;

    // Assert
    let error = result.expect_err("sixth stack level should fail");
    assert!(error.to_string().contains("five-level stack limit"));
    assert_eq!(app.sessions.sessions().len(), 6);
}

#[tokio::test]
async fn test_esc_deletes_blank_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let session_index = app
        .session_index_for_id(&session_id)
        .expect("missing session index");
    let session_folder = app.sessions.sessions()[session_index].folder.clone();
    assert!(session_folder.exists());

    // Act — simulate Esc: delete the blank session
    app.delete_selected_session().await;

    // Assert
    assert!(app.sessions.sessions().is_empty());
    assert!(!session_folder.exists());
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert!(db_sessions.is_empty());
}

#[tokio::test]
async fn test_clear_title_generation_task_if_matches_removes_matching_generation() {
    // Arrange
    let state = SessionState::new(
        HashMap::new(),
        Vec::new(),
        SelectionState::default(),
        Arc::new(RealClock),
        1,
        0,
    );
    let mut session_manager = SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    );
    let session_id = "session-id".to_string();
    let task = tokio::spawn(async {});
    session_manager.replace_title_generation_task(&session_id, 2, task);

    // Act
    session_manager.clear_title_generation_task_if_matches(&session_id, 2);

    // Assert
    assert!(
        session_manager
            .workflow_state
            .title_generation_tasks
            .is_empty()
    );
}

#[tokio::test]
/// Ensures canceling an unstarted draft session persists `Canceled`
/// status without requiring a materialized worktree.
async fn test_cancel_draft_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    let session_folder = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session")
        .folder
        .clone();

    // Act
    app.sessions
        .cancel_session(&app.services, &session_id)
        .await
        .expect("failed to cancel draft session");

    // Assert
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session");
    assert_eq!(session.status, Status::Canceled);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let db_session = db_sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing persisted session");
    assert_eq!(db_session.status, "Canceled");
    wait_for_path_absent(&session_folder).await;
}

#[test]
fn test_remove_at_mention_index_for_root_drops_cached_entries() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let lookup_root = temp_dir.path().to_path_buf();
    let entries = vec![FileEntry {
        is_dir: true,
        path: "src".to_string(),
    }];
    session_manager.set_at_mention_index_for_root(lookup_root.clone(), entries);

    // Act
    session_manager.remove_at_mention_index_for_root(&lookup_root);

    // Assert
    assert!(
        session_manager
            .at_mention_index_for_root(&lookup_root)
            .is_none()
    );
}

#[tokio::test]
async fn test_start_staged_session_launches_bundle_and_clears_staged_drafts() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    app.stage_draft_message(&session_id, "Second draft")
        .await
        .expect("failed to stage second draft");

    // Act
    app.start_staged_session(&session_id)
        .await
        .expect("failed to start staged session");

    crate::test_support::finish_session_creation_tasks(&mut app).await;

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].prompt,
        "First draft\n\nSecond draft"
    );
    assert_eq!(
        app.sessions.sessions()[0].draft_attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
    assert!(app.sessions.sessions()[0].folder.exists());
    assert!(
        app.sessions.sessions()[0]
            .folder
            .join(SESSION_DATA_DIR)
            .is_dir()
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(db_sessions[0].prompt, "First draft\n\nSecond draft");
}

#[tokio::test]
async fn test_start_staged_session_clears_draft_flag() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");

    // Act
    app.start_staged_session(&session_id)
        .await
        .expect("failed to start staged session");

    crate::test_support::finish_session_creation_tasks(&mut app).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            app.process_pending_app_events().await;
            if !app
                .sessions
                .session_for_id(&session_id)
                .expect("session")
                .is_draft_session()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker accepts the draft handoff");

    // Assert
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing started session");
    assert!(!session.is_draft);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert!(!db_sessions[0].is_draft);
}

#[tokio::test]
async fn test_start_staged_session_succeeds_when_clearing_draft_flag_fails() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let mut app = new_test_app_with_git_and_db(dir.path(), db).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    app.services
        .db()
        .sessions()
        .insert_session_preparation(&session_id, "main")
        .await
        .expect("prepare");
    SessionManager::prepare_reserved_session(&app.services, &session_id)
        .await
        .expect("ready workspace");
    sqlx::query!(
        r"
CREATE TRIGGER fail_clear_draft_flag
BEFORE UPDATE OF is_draft ON session
WHEN OLD.is_draft = 1 AND NEW.is_draft = 0
BEGIN
    SELECT RAISE(ABORT, 'draft cleanup failed');
END
"
    )
    .execute(&pool)
    .await
    .expect("failed to install draft cleanup failure trigger");

    // Act
    let result = app.start_staged_session(&session_id).await;

    crate::test_support::finish_session_creation_tasks(&mut app).await;

    // Assert
    assert!(result.is_ok());
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing started session");
    assert!(session.is_draft);
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions");
    assert!(db_sessions[0].is_draft);
}

#[tokio::test]
async fn test_start_staged_session_launches_stacked_draft_child() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    app.stage_draft_message(&child_session_id, "Stacked draft")
        .await
        .expect("failed to stage stacked draft message");

    let started = Arc::new(Notify::new());
    let started_for_agent = Arc::clone(&started);
    let mut channel = MockAgentChannel::new();
    channel
        .expect_run_turn()
        .once()
        .returning(move |_, request, _| {
            assert_eq!(request.prompt.text, "Stacked draft");
            started_for_agent.notify_one();

            Box::pin(std::future::pending())
        });
    app.sessions
        .worker_service
        .test_agent_channels
        .insert(child_session_id.clone().into(), Arc::new(channel));

    // Act
    app.start_staged_session(&child_session_id)
        .await
        .expect("failed to start stacked draft");
    app.sessions.sync_from_handles();

    crate::test_support::finish_session_creation_tasks(&mut app).await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("stacked first turn must reach the worker");
    app.sessions.sync_from_handles();

    // Assert
    let child_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == child_session_id)
        .expect("missing child session");
    assert_eq!(child_session.status, Status::InProgress);
    assert!(child_session.folder.exists());
    assert_eq!(
        child_session.parent_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
}

#[tokio::test]
async fn test_stage_draft_message_persists_bundle_without_starting_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    let session_update_versions = app.services.session_update_versions();
    SessionTaskService::remove_session_update_version(
        &session_update_versions,
        session_id.as_str(),
    );

    // Act
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    app.stage_draft_message(&session_id, "Second draft")
        .await
        .expect("failed to stage second draft");
    let session_update_version = {
        let session_update_versions = session_update_versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *session_update_versions
            .get(session_id.as_str())
            .unwrap_or(&0)
    };

    // Assert
    assert_eq!(app.sessions.sessions()[0].status, Status::Draft);
    assert_eq!(
        app.sessions.sessions()[0].prompt,
        "First draft\n\nSecond draft"
    );
    assert_eq!(
        app.sessions.sessions()[0].title,
        Some("First draft".to_string())
    );
    assert_eq!(
        app.sessions.sessions()[0].draft_attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(db_sessions[0].prompt, "First draft\n\nSecond draft");
    assert_eq!(db_sessions[0].title, Some("First draft".to_string()));
    assert_eq!(session_update_version, 2);
}

#[tokio::test]
async fn test_stage_draft_message_keeps_generated_title_until_replacement_finishes() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(&session_id, "First draft")
        .await
        .expect("failed to stage first draft");
    app.services
        .db()
        .sessions()
        .update_session_title(&session_id, "Generated draft title")
        .await
        .expect("failed to persist generated draft title");
    app.sessions.sessions_mut()[0].title = Some("Generated draft title".to_string());

    // Act
    app.stage_draft_message(&session_id, "Second draft")
        .await
        .expect("failed to stage second draft");

    // Assert
    assert_eq!(
        app.sessions.sessions()[0].prompt,
        "First draft\n\nSecond draft"
    );
    assert_eq!(
        app.sessions.sessions()[0].title,
        Some("Generated draft title".to_string())
    );
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert_eq!(
        db_sessions[0].title,
        Some("Generated draft title".to_string())
    );
}

#[tokio::test]
async fn test_create_draft_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;

    // Act
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");

    // Assert
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].id, session_id);
    assert!(app.sessions.sessions()[0].is_draft_session());
    assert_eq!(app.sessions.sessions()[0].status, Status::Draft);
    assert!(!app.sessions.sessions()[0].folder.exists());

    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    assert!(db_sessions[0].is_draft);
}

#[test]
fn orchestration_research_creation_uses_managed_read_only_role_and_task_link() {
    // Arrange
    let creation_kind = SessionCreationKind::OrchestrationResearch { task_id: 42 };

    // Act
    let role = creation_kind.role();
    let task_id = creation_kind.orchestration_task_id();

    // Assert
    assert_eq!(role, SessionRole::OrchestrationResearcher);
    assert_eq!(task_id, Some(42));
}

#[tokio::test]
async fn test_delete_last_session_update_selection() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "1").await;
    create_and_start_session(&mut app, "2").await;

    // Act & Assert — delete last item
    app.sessions.select_session_index(Some(1));
    app.delete_selected_session().await;
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.selected_session_index(), Some(0));

    // Act & Assert — delete remaining item
    app.delete_selected_session().await;
    assert!(app.sessions.sessions().is_empty());
    assert_eq!(app.sessions.selected_session_index(), None);
}

#[tokio::test]
async fn test_start_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    // Act
    app.start_session(&session_id, "Hello".to_string())
        .await
        .expect("failed to start session");

    // Assert
    assert_eq!(app.sessions.sessions()[0].prompt, "Hello");
    assert_eq!(app.sessions.sessions()[0].title, Some("Hello".to_string()));
    app.sessions.sync_from_handles();
    let output = session_replay_text(&app.sessions.sessions()[0]);
    assert!(output.contains("Hello"));
    let db_sessions = app
        .services
        .db()
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load");
    let activity_timestamps = app
        .services
        .db()
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("failed to load session activity timestamps");
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(db_sessions[0].id.as_str())
        .await
        .expect("failed to load session messages");
    assert_eq!(db_sessions[0].prompt, "Hello");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].kind,
        crate::domain::session_message::SessionMessageKind::UserPrompt.as_str()
    );
    assert_eq!(messages[0].content, "Hello");
    assert_eq!(activity_timestamps.len(), 1);
}

#[tokio::test]
async fn test_start_session_uses_full_prompt_text_as_title() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let prompt = "First line\nSecond line is intentionally long to avoid truncation.";

    // Act
    app.start_session(&session_id, prompt.to_string())
        .await
        .expect("failed to start session");

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

#[tokio::test]
async fn test_delete_selected_draft_session_removes_staged_draft_metadata() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = app
        .create_draft_session()
        .await
        .expect("failed to create draft session");
    app.stage_draft_message(
        &session_id,
        TurnPrompt {
            attachments: vec![TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: dir.path().join("draft-image.png"),
            }],
            text: "First draft".to_string(),
            text_source: TurnPromptTextSource::UserPrompt,
        },
    )
    .await
    .expect("failed to stage first draft");
    let staged_draft_root = app.services.base_path().join(&session_id);
    assert!(staged_draft_root.exists());

    // Act
    app.delete_selected_session().await;

    // Assert
    assert!(!staged_draft_root.exists());
}

#[tokio::test]
async fn test_delete_selected_session_edge_cases() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "1").await;
    create_and_start_session(&mut app, "2").await;

    // Act & Assert — index out of bounds
    app.sessions.select_session_index(Some(99));
    app.delete_selected_session().await;
    assert_eq!(app.sessions.sessions().len(), 2);

    // Act & Assert — None selected
    app.sessions.select_session_index(None);
    app.delete_selected_session().await;
    assert_eq!(app.sessions.sessions().len(), 2);
}
