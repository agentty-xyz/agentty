//! Session API tests for messages with images a remote device sent.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::MockAgentChannel;
use ag_session::{
    CreateSessionMode, CreateSessionRequest, MessageImage, SessionError as ApiSessionError,
    SessionId, SessionOperation, SessionStatus,
};
use tokio::sync::Notify;

use super::support::{request_message_with_images, request_session_creation};
use crate::app::App;
use crate::app::sync::{ProjectSyncContext, ProjectSyncPhase, ProjectSyncStatus};
use crate::app::test_support::TestSessionRunFactory;
use crate::infra::clipboard_image::clipboard_image_directory;
use crate::infra::fs::MockFsClient;

/// Smallest prefix recognized as a PNG file.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 7];
/// Smallest prefix recognized as a JPEG file.
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 9];

/// Image files a test filesystem holds: written, and removed again.
#[derive(Default)]
struct ImageFiles {
    /// Number of files written before every further write fails, if any.
    failing_after: Option<usize>,
    removed: Mutex<Vec<PathBuf>>,
    written: Mutex<Vec<(PathBuf, Vec<u8>)>>,
}

impl ImageFiles {
    /// Returns a filesystem that records image writes and removals, and
    /// resolves every path to itself.
    fn filesystem(self: &Arc<Self>) -> MockFsClient {
        let mut fs = MockFsClient::new();
        fs.expect_create_dir_all()
            .returning(|_| Box::pin(async { Ok(()) }));
        let files = Arc::clone(self);
        fs.expect_write_file().returning(move |path, contents| {
            let mut written = files.written.lock().expect("written files");
            if files.failing_after == Some(written.len()) {
                return Box::pin(async { Err(std::io::Error::other("disk full").into()) });
            }
            written.push((path, contents));

            Box::pin(async { Ok(()) })
        });
        fs.expect_canonicalize()
            .returning(|path| Box::pin(async move { Ok(path) }));
        let files = Arc::clone(self);
        fs.expect_remove_file().returning(move |path| {
            files.removed.lock().expect("removed files").push(path);

            Box::pin(async { Ok(()) })
        });
        fs.expect_remove_dir()
            .returning(|_| Box::pin(async { Ok(()) }));
        // A turn checks that the session worktree exists.
        fs.expect_is_dir().returning(|_| true);

        fs
    }

    fn written(&self) -> Vec<(PathBuf, Vec<u8>)> {
        self.written.lock().expect("written files").clone()
    }

    fn removed(&self) -> Vec<PathBuf> {
        self.removed.lock().expect("removed files").clone()
    }
}

#[tokio::test]
async fn message_images_reach_the_agent_as_attachments_stored_by_the_host() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = review_session(&mut app).await;
    let files = Arc::new(ImageFiles::default());
    app.services.set_fs_client(Arc::new(files.filesystem()));
    let prompt = Arc::new(Mutex::new(None));
    let started = Arc::new(Notify::new());
    let mut channel = MockAgentChannel::new();
    let observed = Arc::clone(&prompt);
    let started_for_agent = Arc::clone(&started);
    channel
        .expect_run_turn()
        .once()
        .returning(move |_, request, _| {
            *observed.lock().expect("prompt") = Some(request.prompt);
            started_for_agent.notify_one();

            Box::pin(std::future::pending())
        });
    TestSessionRunFactory::install(&mut app.services).register(&session_id, Arc::new(channel));
    let images = vec![image(PNG), image(JPEG)];

    // Act
    let sent = request_message_with_images(
        &mut app,
        session_id.clone(),
        "Compare [Image #2] with",
        images,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("the turn should start");

    // Assert
    assert_eq!(sent, Ok(()));
    assert!(
        app.session_service()
            .supports(SessionOperation::MessageImages)
    );
    let image_directory = clipboard_image_directory(session_id.as_str()).expect("directory");
    let written = files.written();
    assert_eq!(written.len(), 2);
    assert!(
        written
            .iter()
            .all(|(path, _)| path.parent() == Some(image_directory.as_path()))
    );
    assert!(written[0].0.to_string_lossy().ends_with(".png"));
    assert_eq!(written[0].1, PNG);
    assert!(written[1].0.to_string_lossy().ends_with(".jpg"));
    assert_eq!(written[1].1, JPEG);
    let prompt = prompt.lock().expect("prompt").clone().expect("prompt sent");
    assert_eq!(prompt.text, "Compare [Image #2] with");
    let attachments: Vec<(PathBuf, String)> = prompt
        .attachments
        .iter()
        .map(|attachment| {
            (
                attachment.local_image_path.clone(),
                attachment.placeholder.clone(),
            )
        })
        .collect();
    assert_eq!(
        attachments,
        [
            (written[0].0.clone(), "[Image #1]".to_string()),
            (written[1].0.clone(), "[Image #2]".to_string()),
        ]
    );
    assert_eq!(files.removed(), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn message_images_are_removed_when_the_session_refuses_the_message() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = review_session(&mut app).await;
    crate::test_support::set_session_status_for_test(&mut app, &session_id, SessionStatus::Done);
    let files = Arc::new(ImageFiles::default());
    app.services.set_fs_client(Arc::new(files.filesystem()));

    // Act
    let refused =
        request_message_with_images(&mut app, session_id.clone(), "", vec![image(PNG)]).await;

    // Assert
    assert!(matches!(refused, Err(ApiSessionError::Operation(_))));
    let written: Vec<PathBuf> = files.written().into_iter().map(|(path, _)| path).collect();
    assert_eq!(written.len(), 1);
    assert_eq!(files.removed(), written);
}

#[tokio::test]
async fn a_staged_draft_keeps_its_images_when_starting_it_fails() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        &mut app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Draft,
            project_id,
        },
    )
    .await
    .expect("draft session should be created");
    let files = Arc::new(ImageFiles::default());
    app.services.set_fs_client(Arc::new(files.filesystem()));
    app.project_sync_status = Some(ProjectSyncStatus {
        context: ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        phase: ProjectSyncPhase::Running,
    });

    // Act
    let failed =
        request_message_with_images(&mut app, session_id.clone(), "Look", vec![image(PNG)]).await;

    // Assert
    assert!(matches!(
        failed,
        Err(ApiSessionError::Operation(ref error)) if error.contains("is synchronizing `main`")
    ));
    let written: Vec<PathBuf> = files.written().into_iter().map(|(path, _)| path).collect();
    assert_eq!(written.len(), 1);
    let staged: Vec<PathBuf> = app
        .sessions
        .session_for_id(&session_id)
        .expect("session")
        .draft_attachments
        .iter()
        .map(|attachment| attachment.local_image_path.clone())
        .collect();
    assert_eq!(staged, written);
    assert_eq!(files.removed(), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn images_already_stored_are_removed_when_another_cannot_be() {
    // Arrange
    let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
    let session_id = review_session(&mut app).await;
    let files = Arc::new(ImageFiles {
        failing_after: Some(1),
        ..ImageFiles::default()
    });
    app.services.set_fs_client(Arc::new(files.filesystem()));

    // Act
    let failed =
        request_message_with_images(&mut app, session_id, "Look", vec![image(PNG), image(JPEG)])
            .await;

    // Assert
    assert!(matches!(
        failed,
        Err(ApiSessionError::Operation(ref error))
            if error.starts_with("Cannot store the message's images:")
    ));
    let written: Vec<PathBuf> = files.written().into_iter().map(|(path, _)| path).collect();
    assert_eq!(written.len(), 1);
    // The image that failed partway is removed with the one stored before.
    let removed = files.removed();
    assert_eq!(removed.len(), 2);
    assert!(removed.contains(&written[0]));
    assert!(
        removed
            .iter()
            .any(|path| path.to_string_lossy().ends_with(".jpg"))
    );
}

/// Creates a regular session in review, ready for a follow-up turn.
async fn review_session(app: &mut App) -> SessionId {
    let project_id = app.active_project_id();
    let session_id = request_session_creation(
        app,
        CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id,
        },
    )
    .await
    .expect("session should be created");
    crate::test_support::set_session_status_for_test(app, &session_id, SessionStatus::Review);

    session_id
}

fn image(data: &[u8]) -> MessageImage {
    MessageImage::new(data.to_vec()).expect("image")
}
