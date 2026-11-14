use std::path::PathBuf;

use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};

use super::super::{
    PromptEffect, apply_prompt_delete_to_mode, apply_prompt_paste_to_mode,
    clear_prompt_slash_input, dismiss_at_mention, exit_to_list_saving_progress, is_plain_char_key,
    mark_prompt_image_paste_pending, move_at_mention_selection_down, move_at_mention_selection_up,
    move_prompt_slash_selection, open_chatgpt_sign_in, prompt_context, prompt_image_paste_number,
    reduce_prompt_submit, reduce_prompt_vertical_key, restore_prompt_composer, select_at_mention,
    sync_prompt_at_mention_state, take_prompt_attachment_cleanup, take_prompt_snapshot,
    update_prompt_slash_state,
};
use super::support::{
    TEST_INPUT_WIDTH, apply_next_session_diff, install_mock_fs_client, install_mock_git_client,
    mode_focus, mode_input, new_test_prompt_app, new_test_prompt_context, new_test_prompt_mode,
    press_prompt_key, prompt_focus, reduce_prompt_code, reduce_prompt_event,
    run_prompt_slash_submit,
};
use crate::app::prompt_intent::PromptSessionMode;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::composer::PromptAttachment;
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputState;
use crate::infra::fs;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashStage,
    PromptSlashState,
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
    run_prompt_slash_submit(&mut app, &prompt_context).await;
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
    run_prompt_slash_submit(&mut app, &prompt_context).await;
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
    run_prompt_slash_submit(&mut app, &prompt_context).await;
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

#[test]
fn test_tab_toggles_focus_between_composer_and_chat() {
    // Arrange
    let mut mode = new_test_prompt_mode("draft text");

    // Act
    let first_effects = reduce_prompt_code(&mut mode, KeyCode::Tab);
    let chat_focus = mode_focus(&mode);
    let second_effects = reduce_prompt_code(&mut mode, KeyCode::Tab);

    // Assert
    assert_eq!(chat_focus, ChatFocus::Chat);
    assert_eq!(mode_focus(&mode), ChatFocus::Input);
    assert!(first_effects.is_empty());
    assert!(second_effects.is_empty());
}

#[tokio::test]
async fn test_diff_round_trip_from_chat_focus_preserves_composer_context() {
    // Arrange — a composer with non-default attachment, history, and slash
    // state over a worktree that has a non-empty diff. The draft is a slash
    // command so the per-keystroke slash-state reset (which only runs for
    // non-slash drafts) leaves the slash selection intact, letting this
    // exercise slash preservation through the real handlers. At-mention state
    // is a distinct input mode covered by the diff-mode restore unit test.
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

/// Returns the open `@` mention selection, or `None` when it is closed.
fn mention_selection(mode: &AppMode) -> Option<usize> {
    let AppMode::Prompt {
        at_mention_state, ..
    } = mode
    else {
        unreachable!("expected AppMode::Prompt");
    };

    at_mention_state.as_ref().map(|state| state.selected_index)
}

/// Builds a prompt whose open mention dropdown lists two `src` files.
fn src_mention_mode(input_text: &str, selected_index: usize) -> AppMode {
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
    let mut mode = new_test_prompt_mode(input_text);
    if let AppMode::Prompt {
        at_mention_state, ..
    } = &mut mode
    {
        *at_mention_state = Some(state);
    }

    mode
}

#[test]
fn cancel_keys_request_prompt_cancellation() {
    for key in [
        KeyEvent::new(KeyCode::Esc, event::KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), event::KeyModifiers::CONTROL),
    ] {
        // Arrange
        let mut mode = new_test_prompt_mode("");

        // Act
        let effects = reduce_prompt_event(&mut mode, key);

        // Assert
        assert!(matches!(effects.as_slice(), [PromptEffect::CancelPrompt]));
    }
}

#[test]
fn history_and_unbound_keys_keep_a_single_line_draft() {
    for key in [
        KeyEvent::new(KeyCode::Char('p'), event::KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('n'), event::KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::F(1), event::KeyModifiers::NONE),
    ] {
        // Arrange
        let mut mode = new_test_prompt_mode("draft");

        // Act
        let effects = reduce_prompt_event(&mut mode, key);

        // Assert
        assert_eq!(mode_input(&mode), ("draft".to_string(), 5));
        assert!(effects.is_empty());
    }
}

#[test]
fn vertical_keys_move_between_draft_lines() {
    // Arrange
    let mut mode = new_test_prompt_mode("first\nsecond");

    // Act
    reduce_prompt_code(&mut mode, KeyCode::Up);
    let (_, cursor_after_up) = mode_input(&mode);
    reduce_prompt_code(&mut mode, KeyCode::Down);

    // Assert
    let (text, cursor_after_down) = mode_input(&mode);
    assert_eq!(text, "first\nsecond");
    assert!(cursor_after_up <= "first".chars().count());
    assert!(cursor_after_down > "first\n".chars().count());
}

#[test]
fn vim_keys_move_the_slash_menu_selection() {
    // Arrange
    let mut mode = new_test_prompt_mode("/");

    // Act
    reduce_prompt_code(&mut mode, KeyCode::Char('j'));
    let selection_after_j = prompt_slash_selection(&mode);
    reduce_prompt_code(&mut mode, KeyCode::Char('k'));

    // Assert
    assert_eq!(selection_after_j, 1);
    assert_eq!(prompt_slash_selection(&mode), 0);
    assert_eq!(mode_input(&mode).0, "/");
}

/// Returns the highlighted slash-menu row, panicking outside prompt mode.
fn prompt_slash_selection(mode: &AppMode) -> usize {
    let AppMode::Prompt { slash_state, .. } = mode else {
        unreachable!("expected AppMode::Prompt");
    };

    slash_state.selected_index
}

#[test]
fn mention_dropdown_claims_navigation_and_completion_keys() {
    // Arrange
    let mut mode = src_mention_mode("@src", 0);

    // Act
    reduce_prompt_code(&mut mode, KeyCode::Down);
    let selection_after_down = mention_selection(&mode);
    reduce_prompt_code(&mut mode, KeyCode::Up);
    let selection_after_up = mention_selection(&mode);
    reduce_prompt_code(&mut mode, KeyCode::Tab);

    // Assert
    assert_eq!(selection_after_down, Some(1));
    assert_eq!(selection_after_up, Some(0));
    // Fuzzy ranking lists the shorter matching path first.
    assert_eq!(mode_input(&mode).0, "@src/beta.rs ");
    assert_eq!(mention_selection(&mode), None);
}

#[test]
fn escape_closes_the_mention_dropdown_and_keeps_the_draft() {
    // Arrange
    let mut mode = src_mention_mode("@src", 1);

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Esc);

    // Assert
    assert_eq!(mention_selection(&mode), None);
    assert_eq!(mode_input(&mode).0, "@src");
    assert!(effects.is_empty());
}

#[test]
fn typing_inside_an_open_mention_resets_its_selection() {
    // Arrange
    let mut mode = src_mention_mode("@sr", 1);

    // Act
    reduce_prompt_code(&mut mode, KeyCode::Char('c'));

    // Assert
    assert_eq!(mode_input(&mode).0, "@src");
    assert_eq!(mention_selection(&mode), Some(0));
}

#[test]
fn mentions_do_not_open_inside_slash_commands() {
    // Arrange
    let mut mode = new_test_prompt_mode("/model ");

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Char('@'));

    // Assert
    assert_eq!(mode_input(&mode).0, "/model @");
    assert_eq!(mention_selection(&mode), None);
    assert!(effects.is_empty());
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

#[test]
fn prompt_reducers_leave_other_modes_unchanged() {
    // Arrange
    let mut mode = AppMode::List;
    let mut prompt_context = new_test_prompt_context();
    prompt_context.session_mode = PromptSessionMode::NewDeletable;
    let mut effects = Vec::new();
    let mut slash_state_updated = false;

    // Act
    let paste_cleanup = apply_prompt_paste_to_mode(&mut mode, "pasted");
    let delete_cleanup = apply_prompt_delete_to_mode(&mut mode, 0, 1);
    let slash_cleanup = clear_prompt_slash_input(&mut mode);
    let attachment_cleanup = take_prompt_attachment_cleanup(&mut mode);
    let image_paste_number = prompt_image_paste_number(&mode);
    update_prompt_slash_state(&mut mode, |_| slash_state_updated = true);
    mark_prompt_image_paste_pending(&mut mode, 7);
    sync_prompt_at_mention_state(&mut mode, &prompt_context, &mut effects);
    exit_to_list_saving_progress(&mut mode, &mut effects);
    dismiss_at_mention(&mut mode);
    move_at_mention_selection_up(&mut mode);
    move_at_mention_selection_down(&mut mode);
    select_at_mention(&mut mode, &prompt_context, &mut effects);
    move_prompt_slash_selection(&mut mode, &prompt_context, true);
    reduce_prompt_vertical_key(
        &mut mode,
        &prompt_context,
        TEST_INPUT_WIDTH,
        false,
        &mut effects,
    );
    reduce_prompt_vertical_key(
        &mut mode,
        &prompt_context,
        TEST_INPUT_WIDTH,
        true,
        &mut effects,
    );
    reduce_prompt_submit(&mut mode, &prompt_context, &mut effects);
    restore_prompt_composer(
        &mut mode,
        PromptAttachmentState::default(),
        InputState::with_text("kept draft".to_string()),
    );
    open_chatgpt_sign_in(
        &mut mode,
        AgentSelection::new(AgentKind::Harness, AgentModel::ChatGptGpt6Sol),
        &mut effects,
    );

    // Assert
    assert_eq!(paste_cleanup, Some(Vec::new()));
    assert_eq!(delete_cleanup, Vec::<PromptAttachment>::new());
    assert_eq!(slash_cleanup, Vec::<PromptAttachment>::new());
    assert_eq!(attachment_cleanup, Vec::<PromptAttachment>::new());
    assert_eq!(image_paste_number, None);
    assert!(!slash_state_updated);
    assert!(effects.is_empty());
    assert!(matches!(mode, AppMode::List));
}
