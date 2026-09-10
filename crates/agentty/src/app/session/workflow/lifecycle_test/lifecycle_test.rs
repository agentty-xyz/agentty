use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ag_forge as forge;
use ag_git as git;

use super::support::{
    create_passthrough_mock_fs_client, database_with_session, database_with_session_and_pool,
    load_persisted_session_row, rollback_git_client, session_manager_with_one_session,
    test_services, test_services_with_fs_client, test_session,
};
use crate::app::SessionManager;
use crate::app::session::{SessionCreationKind, SessionError};
use crate::domain::agent::ResponseStyle;
use crate::domain::session::Status;
use crate::domain::setting::SettingName;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::test_support::FixedClock;

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
