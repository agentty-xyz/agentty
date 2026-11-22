//! Session API file-listing tests covering worktree walks, the shared
//! `@`-mention cache, sessions without a worktree, and the project checkout.

use ag_session::{SessionError as ApiSessionError, SessionFileEntry, SessionId};

use crate::app::App;
use crate::domain::file_entry::FileEntry;

/// Lists one session's worktree entries through the programmatic session API.
async fn request_files(
    app: &mut App,
    session_id: &SessionId,
) -> Result<Option<Vec<SessionFileEntry>>, ApiSessionError> {
    let service = app.session_service();
    let session_id = session_id.clone();

    app.drive_session_request(async move { service.list_session_files(&session_id).await })
        .await
}

/// Builds an app with one session whose worktree exists on disk.
async fn worktree_app() -> (App, tempfile::TempDir, SessionId) {
    let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
    let session_id = SessionId::from(
        app.create_session()
            .await
            .expect("session should be created"),
    );

    (app, base_dir, session_id)
}

#[tokio::test]
async fn api_session_files_list_the_worktree_directories_first_and_none_for_unloaded_sessions() {
    // Arrange
    let (mut app, _base_dir, session_id) = worktree_app().await;
    let folder = app.sessions.sessions()[0].folder.clone();
    std::fs::create_dir_all(folder.join("src")).expect("directory should be created");
    std::fs::write(folder.join("src/lib.rs"), "").expect("file should be written");
    std::fs::write(folder.join("remote.txt"), "").expect("file should be written");

    // Act
    let files = request_files(&mut app, &session_id)
        .await
        .expect("files should load")
        .expect("session should be loaded");
    let missing = request_files(&mut app, &SessionId::from("missing"))
        .await
        .expect("missing lookup should succeed");

    // Assert
    let position = |is_dir: bool, path: &str| {
        files
            .iter()
            .position(|entry| entry.is_dir == is_dir && entry.path == path)
    };
    let source_dir = position(true, "src").expect("directory should be listed");
    let remote_file = position(false, "remote.txt").expect("file should be listed");
    let source_file = position(false, "src/lib.rs").expect("nested file should be listed");
    assert!(source_dir < remote_file && remote_file < source_file);
    assert_eq!(missing, None);
}

#[tokio::test]
async fn api_session_files_reuse_the_cached_mention_index_of_the_worktree() {
    // Arrange
    let (mut app, _base_dir, session_id) = worktree_app().await;
    let folder = app.sessions.sessions()[0].folder.clone();
    app.sessions.set_at_mention_index_for_root(
        folder,
        vec![FileEntry {
            is_dir: false,
            path: "cached.rs".to_string(),
        }],
    );

    // Act
    let files = request_files(&mut app, &session_id)
        .await
        .expect("files should load");

    // Assert
    assert_eq!(
        files,
        Some(vec![SessionFileEntry {
            is_dir: false,
            path: "cached.rs".to_string(),
        }])
    );
}

#[tokio::test]
async fn api_session_files_list_nothing_without_a_worktree_instead_of_the_project() {
    // Arrange
    let (mut app, base_dir, session_id) = worktree_app().await;
    app.sessions.sessions_mut()[0].folder = base_dir.path().join("removed-worktree");

    // Act
    let files = request_files(&mut app, &session_id)
        .await
        .expect("files should load");

    // Assert
    assert_eq!(files, Some(Vec::new()));
}

/// Lists the active project's checkout entries through the programmatic
/// session API.
async fn request_project_files(app: &mut App) -> Result<Vec<SessionFileEntry>, ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(async move { service.list_project_files().await })
        .await
}

#[tokio::test]
async fn api_project_files_list_the_project_checkout_without_a_session() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
    std::fs::create_dir_all(base_dir.path().join("docs")).expect("directory should be created");
    std::fs::write(base_dir.path().join("docs/guide.md"), "").expect("file should be written");

    // Act
    let files = request_project_files(&mut app)
        .await
        .expect("project files should load");

    // Assert
    assert!(
        files
            .iter()
            .any(|entry| entry.is_dir && entry.path == "docs")
    );
    assert!(
        files
            .iter()
            .any(|entry| !entry.is_dir && entry.path == "docs/guide.md")
    );
}

#[tokio::test]
async fn api_project_files_reuse_the_cached_mention_index_of_the_checkout() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let checkout = app.working_dir().to_path_buf();
    app.sessions.set_at_mention_index_for_root(
        checkout,
        vec![FileEntry {
            is_dir: false,
            path: "draft.rs".to_string(),
        }],
    );

    // Act
    let files = request_project_files(&mut app)
        .await
        .expect("project files should load");

    // Assert
    assert_eq!(
        files,
        vec![SessionFileEntry {
            is_dir: false,
            path: "draft.rs".to_string(),
        }]
    );
}
