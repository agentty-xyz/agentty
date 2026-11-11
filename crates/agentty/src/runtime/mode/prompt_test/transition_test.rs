use std::path::PathBuf;

use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};

use super::super::{
    activate_at_mention, apply_prompt_delete_to_mode, apply_prompt_paste_to_mode,
    clear_prompt_slash_input, dismiss_at_mention, exit_to_list_saving_progress,
    handle_at_mention_down, handle_at_mention_select, handle_at_mention_up, handle_prompt_down_key,
    handle_prompt_slash_submit, handle_prompt_up_key, handle_with_cache, is_plain_char_key,
    mark_prompt_image_paste_pending, move_prompt_slash_selection, prompt_context,
    reset_at_mention_selection, sync_prompt_at_mention_state, take_prompt_snapshot,
    update_prompt_slash_state,
};
use super::support::{
    apply_next_session_diff, install_mock_fs_client, install_mock_git_client, new_test_prompt_app,
    new_test_prompt_mode, press_prompt_key, prompt_focus, test_terminal,
};
use crate::app::App;
use crate::domain::agent::AgentKind;
use crate::domain::composer::PromptAttachment;
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputState;
use crate::infra::fs;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashStage,
    PromptSlashState,
};
use crate::ui::RenderCacheStore;

#[tokio::test]
async fn test_handle_apply_command_invalidates_cache_when_diff_hash_mismatches() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/apply", None).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Review;
    let session_id = app.sessions.sessions()[0].id.clone();
    app.review_cache.insert(
        session_id.clone(),
        crate::app::ReviewCacheEntry::Ready {
            request_id: uuid::Uuid::nil(),
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
            request_id: uuid::Uuid::nil(),
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
            request_id: uuid::Uuid::nil(),
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

    let session_id = take_prompt_snapshot(&mut app.mode)
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
        take_prompt_snapshot(&mut app.mode).expect("expected AppMode::Prompt after leaving diff");
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

/// Sends one key event, including modifiers, through the prompt handler.
async fn press_prompt_event(app: &mut App, key: KeyEvent) {
    let mut terminal = test_terminal();

    handle_with_cache(app, &RenderCacheStore::default(), &mut terminal, key)
        .await
        .expect("prompt key handling failed");
}

/// Returns the composer text and cursor, panicking outside prompt mode.
fn prompt_input(app: &App) -> (String, usize) {
    let AppMode::Prompt { input, .. } = &app.mode else {
        unreachable!("expected AppMode::Prompt");
    };

    (input.text().to_string(), input.cursor)
}

/// Returns the open `@` mention selection, or `None` when it is closed.
fn mention_selection(app: &App) -> Option<usize> {
    let AppMode::Prompt {
        at_mention_state, ..
    } = &app.mode
    else {
        unreachable!("expected AppMode::Prompt");
    };

    at_mention_state.as_ref().map(|state| state.selected_index)
}

/// Builds an open mention dropdown over two `src` files.
fn src_mention_state(selected_index: usize) -> PromptAtMentionState {
    let mut state = PromptAtMentionState::new(vec![
        FileEntry {
            is_dir: false,
            path: "src/alpha.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/beta.rs".to_string(),
        },
    ]);
    state.selected_index = selected_index;

    state
}

#[tokio::test]
async fn cancel_keys_close_a_blank_new_session() {
    for key in [
        KeyEvent::new(KeyCode::Esc, event::KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), event::KeyModifiers::CONTROL),
    ] {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("", None).await;

        // Act
        press_prompt_event(&mut app, key).await;

        // Assert
        assert!(matches!(app.mode, AppMode::List));
        assert!(app.sessions.sessions().is_empty());
    }
}

#[tokio::test]
async fn history_and_unbound_keys_keep_a_single_line_draft() {
    for key in [
        KeyEvent::new(KeyCode::Char('p'), event::KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('n'), event::KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::F(1), event::KeyModifiers::NONE),
    ] {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;

        // Act
        press_prompt_event(&mut app, key).await;

        // Assert
        assert_eq!(prompt_input(&app), ("draft".to_string(), 5));
    }
}

#[tokio::test]
async fn vertical_keys_move_between_draft_lines() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("first\nsecond", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Up).await;
    let (_, cursor_after_up) = prompt_input(&app);
    press_prompt_key(&mut app, KeyCode::Down).await;

    // Assert
    let (text, cursor_after_down) = prompt_input(&app);
    assert_eq!(text, "first\nsecond");
    assert!(cursor_after_up <= "first".chars().count());
    assert!(cursor_after_down > "first\n".chars().count());
}

#[tokio::test]
async fn vim_keys_move_the_slash_menu_selection() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('j')).await;
    let selection_after_j = prompt_slash_selection(&app);
    press_prompt_key(&mut app, KeyCode::Char('k')).await;

    // Assert
    assert_eq!(selection_after_j, 1);
    assert_eq!(prompt_slash_selection(&app), 0);
    assert_eq!(prompt_input(&app).0, "/");
}

/// Returns the highlighted slash-menu row, panicking outside prompt mode.
fn prompt_slash_selection(app: &App) -> usize {
    let AppMode::Prompt { slash_state, .. } = &app.mode else {
        unreachable!("expected AppMode::Prompt");
    };

    slash_state.selected_index
}

#[tokio::test]
async fn mention_dropdown_claims_navigation_and_completion_keys() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("@src", Some(src_mention_state(0))).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Down).await;
    let selection_after_down = mention_selection(&app);
    press_prompt_key(&mut app, KeyCode::Up).await;
    let selection_after_up = mention_selection(&app);
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Assert
    assert_eq!(selection_after_down, Some(1));
    assert_eq!(selection_after_up, Some(0));
    // Fuzzy ranking lists the shorter matching path first.
    assert_eq!(prompt_input(&app).0, "@src/beta.rs ");
    assert_eq!(mention_selection(&app), None);
}

#[tokio::test]
async fn escape_closes_the_mention_dropdown_and_keeps_the_draft() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("@src", Some(src_mention_state(1))).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Esc).await;

    // Assert
    assert_eq!(mention_selection(&app), None);
    assert_eq!(prompt_input(&app).0, "@src");
    assert_eq!(app.sessions.sessions().len(), 1);
}

#[tokio::test]
async fn typing_inside_an_open_mention_resets_its_selection() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("@sr", Some(src_mention_state(1))).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('c')).await;

    // Assert
    assert_eq!(prompt_input(&app).0, "@src");
    assert_eq!(mention_selection(&app), Some(0));
}

#[tokio::test]
async fn mentions_do_not_open_inside_slash_commands() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model ", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('@')).await;

    // Assert
    assert_eq!(prompt_input(&app).0, "/model @");
    assert_eq!(mention_selection(&app), None);
}

#[tokio::test]
async fn keys_for_a_missing_prompt_session_return_to_the_list() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
    app.mode = new_test_prompt_mode("draft");

    // Act
    press_prompt_key(&mut app, KeyCode::Char('x')).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn prompt_helpers_leave_other_modes_unchanged() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let terminal = test_terminal();
    app.mode = new_test_prompt_mode("draft");

    // Act
    sync_prompt_at_mention_state(&mut app);
    let mode_after_missing_session_sync = matches!(app.mode, AppMode::List);
    exit_to_list_saving_progress(&mut app);
    sync_prompt_at_mention_state(&mut app);
    activate_at_mention(&mut app, &prompt_context);
    dismiss_at_mention(&mut app);
    handle_at_mention_up(&mut app);
    handle_at_mention_down(&mut app);
    handle_at_mention_select(&mut app).await;
    clear_prompt_slash_input(&mut app).await;
    move_prompt_slash_selection(&mut app, true);
    handle_prompt_up_key(&mut app, &terminal, &prompt_context).expect("up key should not fail");
    handle_prompt_down_key(&mut app, &terminal, &prompt_context).expect("down key should not fail");

    // Assert
    assert!(mode_after_missing_session_sync);
    assert!(matches!(app.mode, AppMode::List));
    assert!(
        !app.restore_prompt_progress(&prompt_context.session_id)
            .await
    );
}

#[test]
fn prompt_reducers_leave_other_modes_unchanged() {
    // Arrange
    let mut mode = AppMode::List;
    let mut slash_state_updated = false;

    // Act
    let paste_cleanup = apply_prompt_paste_to_mode(&mut mode, "pasted");
    let delete_cleanup = apply_prompt_delete_to_mode(&mut mode, 0, 1);
    reset_at_mention_selection(&mut mode);
    update_prompt_slash_state(&mut mode, |_| slash_state_updated = true);
    mark_prompt_image_paste_pending(&mut mode, 7);

    // Assert
    assert_eq!(paste_cleanup, Some(Vec::new()));
    assert_eq!(delete_cleanup, Vec::<PromptAttachment>::new());
    assert!(!slash_state_updated);
    assert!(matches!(mode, AppMode::List));
}
