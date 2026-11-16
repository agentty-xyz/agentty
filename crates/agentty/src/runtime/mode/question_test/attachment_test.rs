use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_session::GraphNodeContinuation;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use super::super::{handle_paste, mark_question_image_paste_pending, take_question_snapshot};
use super::support::{TEST_TERMINAL_SIZE, handle, question_mode_with_options};
use crate::app::graph_question::tests::{awaiting_node_repository, unavailable, viewing_app};
use crate::app::{App, AppEvent};
use crate::domain::composer::{PendingImagePaste, insert_local_image_at};
use crate::domain::input::InputState;
use crate::domain::question::{QuestionAttachmentState, QuestionItem};
use crate::domain::turn_prompt::TurnPromptAttachment;
use crate::infra::clipboard_image::{MockClipboardImageClient, PersistedClipboardImage};
use crate::presentation::app_mode::{AppMode, ChatFocus};

/// Creates one pasted-image file inside Agentty's managed tmp root, where
/// attachment cleanup is allowed to remove it.
fn managed_image(file_name: &str) -> (TempDir, PathBuf) {
    let managed_root = crate::app::agentty_home().join("tmp");
    std::fs::create_dir_all(&managed_root).expect("managed tmp root");
    let directory = tempfile::tempdir_in(managed_root).expect("session directory");
    let images = directory.path().join("images");
    std::fs::create_dir(&images).expect("image directory");
    let image_path = images.join(file_name);
    std::fs::write(&image_path, "image").expect("image fixture");

    (directory, image_path)
}

/// Serves `image_path` for exactly one clipboard capture.
fn install_clipboard_image(app: &mut App, image_path: &Path) {
    let image_path = image_path.to_path_buf();
    let mut clipboard_image_client = MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .returning(move |_, _| {
            let local_image_path = image_path.clone();

            Box::pin(async move { Ok(PersistedClipboardImage { local_image_path }) })
        });
    app.services
        .set_clipboard_image_client(Arc::new(clipboard_image_client));
}

/// Applies app events until the next clipboard capture completion.
async fn apply_next_image_paste(app: &mut App) {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), app.next_app_event())
            .await
            .expect("image paste event should arrive")
            .expect("app event channel should remain open");
        let is_image_paste = matches!(event, AppEvent::PromptImagePasted { .. });
        app.apply_single_app_event(event).await;
        if is_image_paste {
            return;
        }
    }
}

/// Opens the graph question panel and pastes `image_path` after `answer`.
async fn paste_graph_answer_image(app: &mut App, answer: &str, image_path: &Path) {
    app.reconcile_graph_question_mode().await;
    if let AppMode::Question { input, .. } = &mut app.mode {
        *input = InputState::with_text(answer.to_string());
    }
    install_clipboard_image(app, image_path);
    let _ = handle(app, TEST_TERMINAL_SIZE, ctrl_v()).await;
    apply_next_image_paste(app).await;
}

/// Tracks `image_path` as an image pasted at the answer cursor.
fn attach_answer_image(mode: &mut AppMode, image_path: &Path) {
    if let AppMode::Question {
        attachment_state,
        input,
        ..
    } = mode
    {
        let anchor = input.cursor;
        insert_local_image_at(
            &mut attachment_state.composer,
            input,
            image_path.to_path_buf(),
            anchor,
        );
    }
}

/// Returns the images owned by the open question panel.
fn question_attachments(app: &App) -> QuestionAttachmentState {
    match &app.mode {
        AppMode::Question {
            attachment_state, ..
        } => attachment_state.clone(),
        _ => QuestionAttachmentState::default(),
    }
}

fn ctrl_v() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

#[tokio::test]
async fn pasted_answer_images_reach_the_asking_graph_node() {
    // Arrange
    let (_directory, image_path) = managed_image("answer.png");
    let expected_attachment = TurnPromptAttachment {
        local_image_path: image_path.clone(),
        placeholder: "[Image #1]".to_string(),
    };
    let (mut repository, _execution) = awaiting_node_repository(usize::MAX);
    repository
        .expect_continue_node()
        .once()
        .withf(move |request| {
            request.kind
                == GraphNodeContinuation::Reply {
                    attachments: vec![expected_attachment.clone()],
                    message: "Clarifications:\n1. Q: Which update?\n   A: Docs [Image #1]".into(),
                }
        })
        .returning(|_| Ok(true));
    let (mut app, _app_directory) = viewing_app(repository).await;
    app.reconcile_graph_question_mode().await;
    if let AppMode::Question { input, .. } = &mut app.mode {
        *input = InputState::with_text("Docs ".to_string());
    }
    install_clipboard_image(&mut app, &image_path);

    // Act
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, ctrl_v()).await;
    let is_pasting = question_attachments(&app).composer.is_pasting_image();
    let ticks_while_pasting = app.has_visible_tick_driven_ui();
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;
    let blocked_while_pasting = matches!(&app.mode, AppMode::Question { .. });
    apply_next_image_paste(&mut app).await;
    let ticks_after_paste = app.has_visible_tick_driven_ui();
    let pasted_input = match &app.mode {
        AppMode::Question { input, .. } => input.text().to_string(),
        _ => String::new(),
    };
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;

    // Assert
    assert!(is_pasting);
    assert!(ticks_while_pasting, "the paste loader should animate");
    assert!(!ticks_after_paste);
    assert!(blocked_while_pasting);
    assert_eq!(pasted_input, "Docs [Image #1]");
    assert!(matches!(
        &app.mode,
        AppMode::View { session_id, .. } if session_id == "sess"
    ));
    assert!(image_path.exists());
}

#[tokio::test]
async fn retryable_graph_rejection_reopens_the_answer_with_its_image() {
    // Arrange
    let (_directory, image_path) = managed_image("retry.png");
    let (mut repository, _execution) = awaiting_node_repository(usize::MAX);
    repository
        .expect_continue_node()
        .once()
        .returning(|_| Err(unavailable()));
    let (mut app, _app_directory) = viewing_app(repository).await;
    paste_graph_answer_image(&mut app, "Docs ", &image_path).await;

    // Act
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;

    // Assert
    let attachment_state = question_attachments(&app);
    assert!(matches!(
        &app.mode,
        AppMode::Question { input, .. } if input.text() == "Docs [Image #1]"
    ));
    assert_eq!(attachment_state.answered.len(), 0);
    assert_eq!(attachment_state.composer.attachments.len(), 1);
    assert_eq!(
        attachment_state.composer.attachments[0].local_image_path,
        image_path
    );
    assert!(image_path.exists());
}

#[tokio::test]
async fn stale_graph_answers_and_ended_graph_turns_remove_pasted_images() {
    // Arrange
    let (_stale_directory, stale_image) = managed_image("stale.png");
    let (_ended_directory, ended_image) = managed_image("ended.png");
    let (stale_repository, _execution) = awaiting_node_repository(1);
    let (mut stale_app, _stale_app_directory) = viewing_app(stale_repository).await;
    paste_graph_answer_image(&mut stale_app, "Docs ", &stale_image).await;
    let (ended_repository, _execution) = awaiting_node_repository(usize::MAX);
    let (mut ended_app, _ended_app_directory) = viewing_app(ended_repository).await;
    paste_graph_answer_image(&mut ended_app, "Docs ", &ended_image).await;

    // Act
    let _ = handle(&mut stale_app, TEST_TERMINAL_SIZE, enter()).await;
    let _ = handle(
        &mut ended_app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert
    assert!(matches!(&stale_app.mode, AppMode::View { .. }));
    assert!(matches!(&ended_app.mode, AppMode::View { .. }));
    assert!(!stale_image.exists());
    assert!(!ended_image.exists());
}

#[tokio::test]
async fn image_paste_is_ignored_while_navigating_options_or_capturing() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = question_mode_with_options();
    let running_capture = PendingImagePaste {
        anchor: Some(0),
        request_id: 9,
    };

    // Act
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, ctrl_v()).await;
    let after_navigating = question_attachments(&app);
    if let AppMode::Question {
        attachment_state,
        selected_option_index,
        ..
    } = &mut app.mode
    {
        *selected_option_index = None;
        attachment_state.composer.pending_image_paste = Some(running_capture);
    }
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, ctrl_v()).await;

    // Assert
    assert_eq!(after_navigating, QuestionAttachmentState::default());
    assert_eq!(
        question_attachments(&app).composer.pending_image_paste,
        Some(running_capture)
    );
}

#[test]
fn marking_a_running_capture_leaves_other_modes_unchanged() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    mark_question_image_paste_pending(&mut mode, 3);

    // Assert
    assert!(matches!(mode, AppMode::List));
}

#[tokio::test]
async fn choosing_an_option_removes_images_pasted_into_the_free_text_answer() {
    // Arrange
    let (_directory, image_path) = managed_image("unused.png");
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        attachment_state: QuestionAttachmentState::default(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        questions: vec![
            QuestionItem::with_options("Ship it?", vec!["Yes".to_string()]),
            QuestionItem::new("Anything else?"),
        ],
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "missing-session".into(),
    };
    attach_answer_image(&mut app.mode, &image_path);

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    )
    .await;
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Question { current_index: 1, input, responses, .. }
            if input.text().is_empty() && responses == &["Yes".to_string()]
    ));
    assert_eq!(
        question_attachments(&app),
        QuestionAttachmentState::default()
    );
    assert!(!image_path.exists());
}

#[tokio::test]
async fn text_paste_keeps_image_placeholders_attached_to_the_answer() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        attachment_state: QuestionAttachmentState::default(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        questions: vec![
            QuestionItem::new("Which screen?"),
            QuestionItem::new("Anything else?"),
        ],
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "missing-session".into(),
    };
    attach_answer_image(&mut app.mode, Path::new("/tmp/screen.png"));
    if let AppMode::Question { input, .. } = &mut app.mode {
        input.cursor = 0;
    }

    // Act
    handle_paste(&mut app, "See ");
    let _ = handle(&mut app, TEST_TERMINAL_SIZE, enter()).await;

    // Assert
    let attachment_state = question_attachments(&app);
    assert!(matches!(
        &app.mode,
        AppMode::Question { responses, .. } if responses == &["See [Image #1]".to_string()]
    ));
    assert_eq!(attachment_state.answered.len(), 1);
    assert_eq!(
        attachment_state.answered[0].local_image_path,
        PathBuf::from("/tmp/screen.png")
    );
    assert_eq!(attachment_state.composer.next_attachment_number, 2);
}

#[tokio::test]
async fn ending_a_session_turn_removes_pasted_answer_images() {
    // Arrange
    let (_directory, image_path) = managed_image("ended-session.png");
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Question {
        at_mention_state: None,
        attachment_state: QuestionAttachmentState::default(),
        current_index: 0,
        focus: ChatFocus::Input,
        input: InputState::default(),
        questions: vec![QuestionItem::new("Which screen?")],
        responses: Vec::new(),
        scroll_offset: None,
        selected_option_index: None,
        session_id: "session-ctrl-c".into(),
    };
    attach_answer_image(&mut app.mode, &image_path);

    // Act
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;

    // Assert
    assert!(matches!(&app.mode, AppMode::View { .. }));
    assert!(!image_path.exists());
}

#[tokio::test]
async fn leaving_the_panel_keeps_answer_images_and_drops_running_captures() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let mut mode = question_mode_with_options();
    attach_answer_image(&mut mode, Path::new("/tmp/kept.png"));
    if let AppMode::Question {
        attachment_state, ..
    } = &mut mode
    {
        attachment_state.composer.pending_image_paste = Some(PendingImagePaste {
            anchor: Some(0),
            request_id: 4,
        });
    }
    app.mode = mode;
    let questions = match question_mode_with_options() {
        AppMode::Question { questions, .. } => questions,
        _ => Vec::new(),
    };

    // Act
    let snapshot = take_question_snapshot(&mut app).expect("question snapshot");
    app.mode = snapshot.into_question_mode();
    let _ = handle(
        &mut app,
        TEST_TERMINAL_SIZE,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await;
    let saved = app
        .question_progress
        .get("session-id")
        .map(|progress| progress.attachment_state.clone());
    app.enter_question_mode("session-id", questions);

    // Assert
    let attachment_state = question_attachments(&app);
    assert_eq!(saved, Some(attachment_state.clone()));
    assert!(!attachment_state.composer.is_pasting_image());
    assert_eq!(attachment_state.composer.attachments.len(), 1);
    assert_eq!(
        attachment_state.composer.attachments[0].local_image_path,
        PathBuf::from("/tmp/kept.png")
    );
}
