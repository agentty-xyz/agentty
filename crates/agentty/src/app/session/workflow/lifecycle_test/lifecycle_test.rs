use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ag_forge as forge;
use ag_git as git;
use serde_json::Value;
use tracing::instrument::WithSubscriber;

use super::support::{
    create_passthrough_mock_fs_client, database_with_session, database_with_session_and_pool,
    load_persisted_session_row, orchestration_task_ids, rollback_git_client,
    session_manager_with_one_session, test_services, test_services_with_fs_client, test_session,
};
use crate::app::session::{SessionCreationKind, SessionCreationSettings, SessionError};
use crate::app::{ProjectManager, SessionManager};
use crate::domain::agent::{AgentSelection, AgentSelectionMetadata, ResponseStyle};
use crate::domain::session::{SessionHandles, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::setting::SettingName;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::test_support::telemetry::capture_events;
use crate::test_support::{FixedClock, TestSubscriber};

#[tokio::test]
async fn draft_creation_uses_the_active_project_and_requires_a_git_branch() {
    for branch in [None, Some("active-branch")] {
        // Arrange
        let source = test_session("", Status::Draft, None, "");
        let database = database_with_session(&source).await;
        let project_id = database
            .sessions()
            .load_session_project_id(&source.id)
            .await
            .expect("project lookup")
            .expect("project");
        let projects = ProjectManager::new(
            project_id,
            "active project".into(),
            branch.map(ToString::to_string),
            None,
            Vec::new(),
            PathBuf::from("."),
        );
        let mut git = git::MockGitClient::new();
        git.expect_create_worktree().never();
        let services = test_services(
            &database,
            Arc::new(git),
            Arc::new(forge::MockReviewRequestClient::new()),
        );
        let mut manager = session_manager_with_one_session(source);

        // Act
        let result = manager.create_draft_session(&projects, &services).await;

        // Assert
        if let Some(branch) = branch {
            let id = result.expect("draft creation");
            let draft = database
                .sessions()
                .load_session(&id)
                .await
                .expect("persisted draft")
                .expect("draft exists");
            assert_eq!(draft.project_id, Some(project_id));
            assert_eq!(draft.base_branch, branch);
            assert!(draft.is_draft);
            assert_eq!(draft.status, "Draft");
        } else {
            assert!(matches!(result, Err(SessionError::Workflow(ref message))
                if message == "Git branch is required to create a session"));
            assert_eq!(
                database
                    .sessions()
                    .load_sessions_metadata()
                    .await
                    .expect("session count")
                    .0,
                1
            );
        }
    }
}

#[tokio::test]
async fn successful_reservations_report_creation_types() {
    // Arrange
    let source = test_session("private prompt", Status::Review, Some("Source"), "history");
    let source_id = source.id.clone();
    let database = database_with_session(&source).await;
    let project_id = database
        .sessions()
        .load_session_project_id(&source_id)
        .await
        .expect("project lookup")
        .expect("source project");
    let task_ids = orchestration_task_ids(&database, &source_id).await;
    let mut git = git::MockGitClient::new();
    git.expect_find_git_repo_root()
        .returning(|path| Box::pin(async move { Some(path) }));
    git.expect_ref_hash()
        .returning(|_, _| Box::pin(async { Ok("frozen-source-commit".to_string()) }));
    let mut services = test_services_with_fs_client(
        &database,
        Arc::new(RealClock),
        Arc::new(create_passthrough_mock_fs_client()),
        Arc::new(git),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let settings = SessionCreationSettings {
        agent: source.agent,
        permission_mode: source.permission_mode,
        personality_id: None,
        reasoning_level: source.reasoning_level_override.unwrap_or_default(),
        response_style: source.response_style,
        role: source.role,
        speed_mode: source.speed_mode,
    };
    let mut manager = session_manager_with_one_session(source);
    let (analytics, receiver) = capture_events(7);
    services.set_analytics(Some(analytics));

    // Act
    for parent in [None, Some(source_id.as_str())] {
        manager
            .create_draft_session_for_project_with_parent(
                &services,
                project_id,
                "main",
                parent,
                Some(settings.clone()),
            )
            .await
            .expect("draft reservation");
    }
    manager
        .reserve_fork_session(&services, &source_id)
        .await
        .expect("fork reservation");
    for creation_kind in [
        SessionCreationKind::Worker,
        SessionCreationKind::Orchestrator,
        SessionCreationKind::OrchestrationChild {
            task_id: task_ids[0],
        },
        SessionCreationKind::OrchestrationResearch {
            task_id: task_ids[1],
        },
    ] {
        SessionManager::reserve_session(
            &services,
            project_id,
            "main",
            settings.clone(),
            creation_kind,
        )
        .await
        .expect("regular reservation");
    }
    services.wait_for_cleanup_tasks(None).await;
    let events = receiver.join().expect("telemetry receiver");

    // Assert
    let mut types = events
        .iter()
        .map(|event| {
            assert_session_start_metadata(event, settings.agent, &source_id);

            event["properties"]["session_type"]
                .as_str()
                .expect("session type")
        })
        .collect::<Vec<_>>();
    types.sort_unstable();
    assert_eq!(
        types,
        vec![
            "draft",
            "fork",
            "orchestration_child",
            "orchestration_research",
            "orchestrator",
            "regular",
            "stacked",
        ]
    );
}

#[tokio::test]
async fn record_session_creation_activity_uses_injected_clock() {
    // Arrange
    let timestamp_seconds = 123_i64;
    let session = test_session("", Status::Draft, None, "");
    let database = database_with_session(&session).await;
    let clock = Arc::new(FixedClock::new(
        Instant::now(),
        SystemTime::UNIX_EPOCH + Duration::from_secs(123),
    ));
    let services = test_services_with_fs_client(
        &database,
        clock,
        Arc::new(create_passthrough_mock_fs_client()),
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    SessionManager::record_session_creation_activity(&services, "session-id").await;
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await
        .expect("activity timestamps should load");

    // Assert
    assert_eq!(activity_timestamps, vec![timestamp_seconds]);
}

#[tokio::test]
async fn reply_keeps_history_pending_when_detail_or_transcript_load_fails() {
    for failed_read in ["DROP TABLE session", "DROP TABLE session_message"] {
        // Arrange
        let session = test_session("", Status::Review, Some("Existing"), "");
        let (database, pool) = database_with_session_and_pool(&session).await;
        database
            .sessions()
            .append_session_message(
                "session-id",
                SessionMessageKind::UserPrompt,
                "Saved history",
            )
            .await
            .expect("saved history");
        let services = test_services(
            &database,
            Arc::new(git::MockGitClient::new()),
            Arc::new(forge::MockReviewRequestClient::new()),
        );
        let mut manager = session_manager_with_one_session(session);
        manager.state.handles_mut().insert(
            "session-id".into(),
            SessionHandles::new_unloaded(Status::Review),
        );
        manager.mark_history_replay_pending("session-id");
        sqlx::query(failed_read)
            .execute(&pool)
            .await
            .expect("read should fail");

        // Act
        let accepted = manager.reply(&services, "session-id", "New reply").await;

        // Assert
        assert!(!accepted, "reply should stop after {failed_read}");
        assert!(manager.should_replay_history("session-id"));
        assert!(
            manager
                .state
                .handle("session-id")
                .expect("session handle")
                .needs_transcript_hydration()
        );
    }
}

#[tokio::test]
async fn test_create_session_worktree_uses_local_base_branch_ref() {
    // Arrange
    let session = test_session("", Status::Draft, None, "");
    let database = database_with_session(&session).await;
    let repo_root = PathBuf::from("/tmp/project");
    let folder = PathBuf::from("/tmp/session-worktree");
    let expected_repo_root = repo_root.clone();
    let expected_folder = folder.clone();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_create_worktree()
        .once()
        .withf(
            move |candidate_repo_root, candidate_folder, worktree_branch, start_ref| {
                candidate_repo_root == &expected_repo_root
                    && candidate_folder == &expected_folder
                    && worktree_branch == "wt/session-id"
                    && start_ref == "main"
            },
        )
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    let services = test_services_with_fs_client(
        &database,
        Arc::new(crate::infra::clock::RealClock),
        Arc::new(create_passthrough_mock_fs_client()),
        Arc::new(mock_git_client),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let result = SessionManager::create_session_worktree(
        &services,
        "session-id",
        folder.as_path(),
        repo_root.as_path(),
        "wt/session-id",
        "main",
    )
    .await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn creation_rolls_back_when_metadata_directory_cannot_be_created() {
    // Arrange
    let source = test_session("source", Status::Review, None, "");
    let database = database_with_session(&source).await;
    let mut git = rollback_git_client();
    git.expect_create_worktree()
        .once()
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    let mut filesystem = fs::MockFsClient::new();
    filesystem.expect_create_dir_all().once().returning(|_| {
        Box::pin(async { Err(fs::FsError::Io(std::io::Error::other("directory rejected"))) })
    });
    filesystem
        .expect_remove_dir_all()
        .times(1..)
        .returning(|_| Box::pin(async { Ok(()) }));
    let services = test_services_with_fs_client(
        &database,
        Arc::new(RealClock),
        Arc::new(filesystem),
        Arc::new(git),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let result = SessionManager::create_session_worktree(
        &services,
        "new-session",
        Path::new("/tmp/new-session"),
        Path::new("/tmp/project"),
        "wt/new-session",
        "main",
    )
    .await;

    // Assert
    assert!(
        result
            .expect_err("directory creation must fail")
            .to_string()
            .contains("directory rejected")
    );
    assert_eq!(
        database
            .sessions()
            .load_sessions_metadata()
            .await
            .expect("session count")
            .0,
        1
    );
}

#[tokio::test]
/// Ensures prompt attachment cleanup removes temp files and their image
/// directory after handoff.
async fn test_cleanup_prompt_attachment_paths_removes_files_and_directory() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("temp dir should exist");
    let managed_tmp_root = temp_dir.path().join("tmp");
    let image_directory = managed_tmp_root.join("session-id").join("images");
    std::fs::create_dir_all(&image_directory).expect("image directory should exist");
    let first_image = image_directory.join("image-1.png");
    let second_image = image_directory.join("image-2.png");
    std::fs::write(&first_image, b"png").expect("first image should exist");
    std::fs::write(&second_image, b"png").expect("second image should exist");

    // Act
    SessionManager::cleanup_prompt_attachment_paths_in_root(
        Arc::new(fs::RealFsClient),
        &managed_tmp_root,
        vec![first_image.clone(), second_image.clone()],
    )
    .await;

    // Assert
    assert!(!first_image.exists());
    assert!(!second_image.exists());
    assert!(!image_directory.exists());
}

#[tokio::test]
async fn creation_and_fork_reserve_metadata_before_checkout() {
    for fork in [false, true] {
        // Arrange
        let source = test_session("source", Status::Review, Some("Source"), "history");
        let source_id = source.id.clone();
        let (database, pool) = database_with_session_and_pool(&source).await;
        let project_id = database
            .sessions()
            .load_session_project_id(&source_id)
            .await
            .expect("project lookup")
            .expect("source project");
        sqlx::query(
            "CREATE TRIGGER reject_creation BEFORE INSERT ON session BEGIN SELECT RAISE(ABORT, \
             'creation rejected'); END",
        )
        .execute(&pool)
        .await
        .expect("failure trigger");
        let mut manager = session_manager_with_one_session(source);
        let mut git = git::MockGitClient::new();
        if fork {
            git.expect_find_git_repo_root()
                .once()
                .returning(|path| Box::pin(async move { Some(path) }));
            git.expect_ref_hash()
                .once()
                .returning(|_, _| Box::pin(async { Ok("frozen-source-commit".to_string()) }));
        }
        git.expect_create_worktree().never();
        let services = test_services_with_fs_client(
            &database,
            Arc::new(RealClock),
            Arc::new(create_passthrough_mock_fs_client()),
            Arc::new(git),
            Arc::new(forge::MockReviewRequestClient::new()),
        );

        // Act
        let result = if fork {
            manager.fork_session(&services, &source_id).await
        } else {
            manager
                .create_session_for_project(
                    &services,
                    project_id,
                    "main",
                    PathBuf::from("/tmp/project"),
                    None,
                    SessionCreationKind::Worker,
                )
                .await
        };

        // Assert
        assert!(
            result
                .expect_err("persistence must fail")
                .to_string()
                .contains("creation rejected")
        );
        assert!(
            database
                .sessions()
                .load_session(&source_id)
                .await
                .expect("source lookup")
                .is_some()
        );
        assert_eq!(
            database
                .sessions()
                .load_sessions_metadata()
                .await
                .expect("session count")
                .0,
            1
        );
    }
}

#[tokio::test]
async fn test_ensure_session_worktree_ready_skips_non_draft_sessions() {
    // Arrange
    let session = test_session("", Status::Draft, None, "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client.expect_is_dir().times(0);
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client.expect_create_worktree().times(0);
    mock_git_client.expect_find_git_repo_root().times(0);
    let services = test_services_with_fs_client(
        &database,
        Arc::new(crate::infra::clock::RealClock),
        Arc::new(mock_fs_client),
        Arc::new(mock_git_client),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let result = session_manager
        .ensure_session_worktree_ready(&services, "session-id")
        .await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_ensure_session_worktree_ready_reuses_existing_draft_worktree() {
    // Arrange
    let mut session = test_session("", Status::Draft, None, "");
    session.is_draft = true;
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client.expect_is_dir().times(3).return_const(true);
    mock_fs_client
        .expect_canonicalize()
        .times(2)
        .returning(|path| {
            Box::pin(async move {
                if path == Path::new("/tmp/project") {
                    Ok(PathBuf::from("/tmp/project"))
                } else {
                    Ok(PathBuf::from("/tmp/session"))
                }
            })
        });
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/session-".to_string()) }));
    mock_git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(PathBuf::from("/tmp/project"))) }));
    mock_git_client.expect_create_worktree().times(0);
    mock_git_client.expect_find_git_repo_root().times(0);
    let services = test_services_with_fs_client(
        &database,
        Arc::new(crate::infra::clock::RealClock),
        Arc::new(mock_fs_client),
        Arc::new(mock_git_client),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let result = session_manager
        .ensure_session_worktree_ready(&services, "session-id")
        .await;

    // Assert
    assert!(result.is_ok());
}

#[test]
fn test_formatted_prompt_output_formats_multiline_prompt_with_continuation_prefix() {
    // Arrange
    let prompt = TurnPrompt::from_text("first line\n\n\nafter gap".to_string());

    // Act
    let formatted_prompt = SessionManager::formatted_prompt_output(&prompt, false);

    // Assert
    assert_eq!(
        formatted_prompt,
        " › first line\n   \n   \n   after gap\n\n"
    );
}

#[tokio::test]
/// Ensures an unavailable replacement clears an older tracked draft-title
/// task.
async fn test_track_draft_title_generation_task_clears_older_task() {
    // Arrange
    let session = test_session("Draft prompt", Status::Draft, Some("Draft prompt"), "");
    let mut session_manager = session_manager_with_one_session(session);
    let title_generation_task = tokio::spawn(std::future::pending::<()>());
    session_manager.track_draft_title_generation_task("session-id", 1, Some(title_generation_task));

    // Act
    session_manager.track_draft_title_generation_task("session-id", 2, None);

    // Assert
    assert!(
        session_manager
            .workflow_state
            .title_generation_tasks
            .is_empty()
    );
}

#[tokio::test]
async fn test_stage_draft_message_preserves_persisted_prompt_when_attachment_write_fails() {
    // Arrange
    let mut session = test_session("", Status::Draft, None, "");
    session.is_draft = true;
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_create_dir_all()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_remove_dir_all()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_read_file()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    mock_fs_client
        .expect_remove_file()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_exists()
        .times(0..)
        .returning(|path| path.exists());
    mock_fs_client
        .expect_is_dir()
        .times(0..)
        .returning(|path| path.is_dir());
    mock_fs_client.expect_write_file().once().returning(|_, _| {
        Box::pin(async {
            Err(fs::FsError::Io(std::io::Error::other(
                "simulated attachment write failure",
            )))
        })
    });
    let services = test_services_with_fs_client(
        &database,
        Arc::new(crate::infra::clock::RealClock),
        Arc::new(mock_fs_client),
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let prompt = TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/image-1.png"),
        }],
        text: "Review [Image #1]".to_string(),
        text_source: TurnPromptTextSource::UserPrompt,
    };

    // Act
    let error = session_manager
        .stage_draft_message(&services, "session-id", prompt)
        .await
        .expect_err("attachment metadata failure should abort draft staging");
    let persisted_session = load_persisted_session_row(&database).await;

    // Assert
    assert!(matches!(error, SessionError::Fs(_)));
    assert_eq!(persisted_session.prompt, "");
    assert_eq!(session_manager.sessions()[0].prompt, "");
    assert_eq!(
        session_manager.sessions()[0].draft_attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
}

#[tokio::test]
async fn resolve_session_creation_settings_uses_project_response_style_default() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::DefaultResponseStyle,
            ResponseStyle::Detailed.as_str(),
        )
        .await
        .expect("failed to persist response style default");
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let session = test_session("Prompt", Status::Review, Some("Title"), "");
    let mut session_manager = session_manager_with_one_session(session);

    // Act
    let creation_settings = session_manager
        .resolve_session_creation_settings(&services, project_id, None)
        .await
        .expect("session creation settings should resolve");

    // Assert
    assert_eq!(creation_settings.response_style, ResponseStyle::Detailed);
}

fn assert_session_start_metadata(event: &Value, agent: AgentSelection, private_session_id: &str) {
    assert_eq!(event["event"], "agentty_session_start");
    assert_eq!(event["properties"]["agent"], agent.kind().name());
    assert_eq!(event["properties"]["model"], agent.model().as_str());
    assert!(!event.to_string().contains("private prompt"));
    assert!(!event.to_string().contains(private_session_id));
}

#[tokio::test]
/// Removes each session's harness history and tolerates sessions without one.
async fn harness_directory_cleanup_tolerates_missing_history() {
    // Arrange
    let removed_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut filesystem = fs::MockFsClient::new();
    let recorded_paths = Arc::clone(&removed_paths);
    let mut results = vec![
        Err(fs::FsError::Io(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        ))),
        Err(fs::FsError::Io(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        ))),
        Ok(()),
    ];
    filesystem
        .expect_remove_dir_all()
        .times(3)
        .returning(move |path| {
            recorded_paths.lock().expect("paths lock").push(path);
            let result = results.pop().expect("one result per call");

            Box::pin(async move { result })
        });
    let filesystem: Arc<dyn fs::FsClient> = Arc::new(filesystem);

    // Act
    for session_id in ["removed", "missing", "denied"] {
        SessionManager::cleanup_session_harness_directory(Arc::clone(&filesystem), session_id)
            .with_subscriber(TestSubscriber)
            .await;
    }

    // Assert
    let removed_paths = removed_paths.lock().expect("paths lock");
    assert_eq!(removed_paths.len(), 3);
    assert!(removed_paths[0].ends_with("harness/removed"));
    assert!(removed_paths[2].ends_with("harness/denied"));
}
