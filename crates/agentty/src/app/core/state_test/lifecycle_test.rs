use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use app::sync;
use tempfile::tempdir;

use super::super::App;
use super::support::install_mock_git_client;
use crate::app;
use crate::domain::session::Status;
use crate::infra::db::AppRepositories;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn session_git_status_targets_skip_unmaterialized_drafts() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let mut draft_session =
        crate::test_support::session_fixture_with_folder(PathBuf::from("/tmp/session-draft"));
    draft_session.id = "draft-1".into();
    draft_session.is_draft = true;
    draft_session.status = Status::Draft;
    app.sessions.push_session(draft_session);

    // Act
    let targets_before_materialization = App::session_git_status_targets(&app.sessions);
    app.sessions.set_session_worktree_available("draft-1", true);
    let targets_after_materialization = App::session_git_status_targets(&app.sessions);

    // Assert
    assert_eq!(
        targets_before_materialization,
        [] as [crate::app::sync::SessionGitStatusTarget; 0]
    );
    assert_eq!(
        targets_after_materialization,
        vec![sync::SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "wt/draft-1".to_string(),
            session_id: "draft-1".into(),
        }]
    );
}

#[tokio::test]
async fn test_continue_terminal_session_opens_draft_prompt_for_done_session_with_hash() {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = AppRepositories::in_memory().await.expect("db should open");
    let clients = crate::test_support::test_app_clients()
        .with_app_server_client_override(crate::test_support::mock_app_server())
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path.clone(),
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build test app");
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&base_path.to_string_lossy(), None)
        .await
        .expect("failed to insert project");
    app.services
        .db()
        .sessions()
        .insert_session("done-source", "gpt-5.6-sol", "release", "Done", project_id)
        .await
        .expect("failed to insert source session row");
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    app.services
        .db()
        .sessions()
        .update_session_merged_commit_hash("done-source", Some(merged_commit_hash.to_string()))
        .await
        .expect("failed to persist merged commit hash");
    let mut source_session = crate::test_support::SessionFixtureBuilder::new()
        .id("done-source")
        .status(Status::Done)
        .project_name("project-alpha")
        .title(Some("Done source".to_string()))
        .build();
    source_session.base_branch = "release".to_string();
    app.sessions.push_session(source_session);

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .never()
        .returning(|path| Box::pin(async move { Some(path) }));
    mock_git_client
        .expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    install_mock_git_client(&mut app, mock_git_client);

    // Act
    let continued_session_id = app
        .continue_terminal_session("done-source")
        .await
        .expect("expected terminal continuation to succeed");

    // Assert
    assert_ne!(continued_session_id, "done-source");
    assert!(matches!(
        app.mode,
        AppMode::Prompt {
            ref input,
            ref session_id,
            ..
        } if session_id.as_str() == continued_session_id
            && input.text().is_empty()
    ));
    let continued_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == continued_session_id)
        .expect("expected created continuation draft");
    assert!(continued_session.is_draft_session());
    assert_eq!(continued_session.base_branch, "release");
    assert_eq!(continued_session.status, Status::Draft);
    assert_eq!(
        continued_session.prompt,
        format!("Use {merged_commit_hash} commit as an initial context for this session")
    );
    assert!(matches!(
        app.selected_session(),
        Some(session) if session.id == continued_session_id
    ));
}
