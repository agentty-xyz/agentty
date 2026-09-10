use std::path::PathBuf;

use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};

use super::super::{
    handle_prompt_slash_submit, is_plain_char_key, prompt_context, take_prompt_snapshot,
};
use super::support::{
    apply_next_session_diff, install_mock_fs_client, install_mock_git_client, new_test_prompt_app,
    press_prompt_key, prompt_focus,
};
use crate::domain::agent::AgentKind;
use crate::domain::composer::PromptAttachment;
use crate::domain::input::InputState;
use crate::infra::fs;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{
    PromptAttachmentState, PromptHistoryState, PromptSlashStage, PromptSlashState,
};

#[tokio::test]
async fn test_handle_apply_command_invalidates_cache_when_diff_hash_mismatches() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/apply", None).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Review;
    let session_id = app.sessions.sessions()[0].id.clone();
    app.review_cache.insert(
        session_id.clone(),
        crate::app::ReviewCacheEntry::Ready {
            diff_hash: u64::MAX,
            text: "## Review\n### Suggestions\n- Fix the typo.".to_string(),
        },
    );
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .once()
        .returning(|_, _| Box::pin(async { Ok("current diff".to_string()) }));
    install_mock_git_client(&mut app, mock_git_client);
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id.as_str()));
    assert!(matches!(app.mode, AppMode::View { .. }));
}

#[tokio::test]
async fn test_handle_apply_command_submits_suggestions_when_diff_hash_matches() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/apply", None).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Review;
    let session_id = app.sessions.sessions()[0].id.clone();
    let folder = app.sessions.sessions()[0].folder.clone();
    let base_branch = app.sessions.sessions()[0].base_branch.clone();
    let current_diff = app
        .services
        .git_client()
        .diff(folder, base_branch)
        .await
        .unwrap_or_default();
    let current_hash = crate::app::test_support::diff_content_hash(&current_diff);
    app.review_cache.insert(
        session_id.clone(),
        crate::app::ReviewCacheEntry::Ready {
            diff_hash: current_hash,
            text: "## Review\n### Suggestions\n- Fix the typo in `README.md`.".to_string(),
        },
    );
    let image_path = crate::infra::home::agentty_home()
        .join("tmp")
        .join(session_id.as_str())
        .join("images")
        .join("image-1.png");
    if let AppMode::Prompt {
        attachment_state, ..
    } = &mut app.mode
    {
        attachment_state
            .attachments
            .push(PromptAttachment::new(1, image_path.clone()));
    }
    let expected_image_path = image_path.clone();
    let expected_image_directory = image_path
        .parent()
        .expect("managed image path should have a parent")
        .to_path_buf();
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_remove_file()
        .once()
        .withf(move |path| path == &expected_image_path)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_remove_dir()
        .once()
        .withf(move |path| path == &expected_image_directory)
        .returning(|_| Box::pin(async { Ok(()) }));
    install_mock_fs_client(&mut app, mock_fs_client);
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(!app.review_cache.contains_key(session_id.as_str()));
    assert!(matches!(app.mode, AppMode::View { .. }));
}

#[tokio::test]
async fn test_handle_apply_command_preserves_cache_on_git_diff_error() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/apply", None).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Review;
    let session_id = app.sessions.sessions()[0].id.clone();
    app.review_cache.insert(
        session_id.clone(),
        crate::app::ReviewCacheEntry::Ready {
            diff_hash: 42,
            text: "## Review\n### Suggestions\n- Fix the typo in `README.md`.".to_string(),
        },
    );

    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client.expect_diff().returning(|_, _| {
        Box::pin(async {
            Err(ag_git::GitError::OutputParse(
                "simulated git failure".to_string(),
            ))
        })
    });
    install_mock_git_client(&mut app, mock_git_client);

    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    apply_next_session_diff(&mut app).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert!(
        matches!(
            app.review_cache.get(session_id.as_str()),
            Some(crate::app::ReviewCacheEntry::Ready { diff_hash: 42, .. }),
        ),
        "cached review must survive a transient git diff error",
    );
}

#[tokio::test]
async fn test_tab_toggles_focus_between_composer_and_chat() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Tab).await;
    let chat_focus = prompt_focus(&app);
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Assert
    assert_eq!(chat_focus, ChatFocus::Chat);
    assert_eq!(prompt_focus(&app), ChatFocus::Input);
}

#[tokio::test]
async fn test_diff_round_trip_from_chat_focus_preserves_composer_context() {
    // Arrange — a composer with non-default attachment, history, and slash
    // state over a worktree that has a non-empty diff. The draft is a slash
    // command so the per-keystroke `reset_prompt_slash_state` normalization
    // (which only runs for non-slash drafts) leaves the slash selection
    // intact, letting this exercise slash preservation through the real
    // handlers. At-mention state is a distinct input mode covered by the
    // diff-mode restore unit test.
    let (mut app, _base_dir) = new_test_prompt_app("/keep-draft", None).await;
    let session_folder = app.sessions.sessions()[0].folder.clone();
    std::fs::write(session_folder.join("README.md"), "updated content")
        .expect("failed to write diff fixture");

    let mut attachment_state = PromptAttachmentState::default();
    attachment_state.register_local_image(PathBuf::from("/tmp/pic.png"), 0);
    let expected_attachment_state = attachment_state.clone();

    let mut history_state =
        PromptHistoryState::new(vec!["prev one".to_string(), "prev two".to_string()]);
    history_state.draft_text = Some("saved draft".to_string());
    history_state.selected_index = Some(1);
    let expected_history_state = history_state.clone();

    let mut slash_state = PromptSlashState::with_available_agent_kinds(vec![AgentKind::Codex]);
    slash_state.stage = PromptSlashStage::Model;
    slash_state.selected_index = 2;
    let expected_slash_state = slash_state.clone();

    let session_id = take_prompt_snapshot(&mut app)
        .expect("expected AppMode::Prompt")
        .session_id;
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state,
        focus: ChatFocus::Input,
        history_state,
        slash_state,
        session_id,
        input: InputState::with_text("/keep-draft".to_string()),
        scroll_offset: Some(4),
    };

    // Act — focus the transcript, request the diff, then cancel loading.
    // This exercises the real capture-and-restore workflow end to end.
    press_prompt_key(&mut app, KeyCode::Tab).await;
    press_prompt_key(&mut app, KeyCode::Char('d')).await;
    assert!(
        matches!(app.mode, AppMode::DiffLoading { .. }),
        "pressing d in chat focus must open diff loading"
    );
    crate::runtime::mode::diff::handle_loading(
        &mut app,
        KeyEvent::new(KeyCode::Esc, event::KeyModifiers::NONE),
    );

    // Assert — every durable composer field survives the round-trip.
    assert_eq!(prompt_focus(&app), ChatFocus::Input);
    let snapshot =
        take_prompt_snapshot(&mut app).expect("expected AppMode::Prompt after leaving diff");
    assert_eq!(snapshot.input.text(), "/keep-draft");
    assert_eq!(snapshot.scroll_offset, Some(4));
    assert_eq!(snapshot.attachment_state, expected_attachment_state);
    assert_eq!(snapshot.history_state, expected_history_state);
    assert_eq!(snapshot.slash_state, expected_slash_state);
}

#[test]
fn test_is_plain_char_key_for_plain_character() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

    // Act
    let result = is_plain_char_key(key, 'j');

    // Assert
    assert!(result);
}

#[test]
fn test_is_plain_char_key_rejects_modifier_keys() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('k'), event::KeyModifiers::SHIFT);

    // Act
    let result = is_plain_char_key(key, 'k');

    // Assert
    assert!(!result);
}

#[test]
fn test_is_plain_char_key_rejects_other_character() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('j'), event::KeyModifiers::NONE);

    // Act
    let result = is_plain_char_key(key, 'k');

    // Assert
    assert!(!result);
}
