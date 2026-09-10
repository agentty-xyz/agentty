use std::path::PathBuf;

use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};

use super::super::{
    PromptInputMode, apply_prompt_apply_outcome, apply_prompt_input_command, handle_at_mention_key,
    handle_at_mention_select, handle_chat_focus_key, handle_paste, handle_prompt_cancel_key,
    handle_prompt_down_key, handle_prompt_image_paste, handle_prompt_slash_submit,
    handle_prompt_submit_key, handle_prompt_up_key, handle_with_cache,
    insert_pasted_image_placeholder, is_active_at_mention, is_prompt_image_paste_key,
    move_prompt_slash_selection, navigate_prompt_history_down, navigate_prompt_history_up,
    paste_image_into_active_prompt, prompt_context, show_prompt_diff, submit_current_text_prompt,
    take_prompt_attachment_cleanup, take_prompt_snapshot, take_submitted_turn_prompt,
};
use super::support::{
    PromptTestAppExt, apply_next_session_diff, install_mock_clipboard_image_client,
    install_mock_git_client, new_test_draft_prompt_app, new_test_prompt_app, press_prompt_key,
    prompt_focus, session_replay_text, test_terminal,
};
use crate::app::prompt_intent::PromptApplyOutcome;
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
};
use crate::domain::composer::PromptAttachment;
use crate::domain::file_entry::FileEntry;
use crate::domain::input::{INPUT_HISTORY_LIMIT, InputCommand, InputState};
use crate::domain::permission::PermissionMode;
use crate::domain::session::SessionId;
use crate::presentation::app_mode::{AppMode, ChatFocus, DiffRestoreTarget};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashStage,
    PromptSlashState, prompt_slash_option_count,
};
use crate::ui::RenderCacheStore;

#[tokio::test]
async fn test_prompt_edit_prunes_attachment_after_undo_revision_eviction() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;

    // Act
    for _ in 0..INPUT_HISTORY_LIMIT {
        apply_prompt_input_command(&mut app, InputCommand::Insert('x')).await;
    }

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            attachment_state, ..
        } if attachment_state.attachments.is_empty()
            && attachment_state.archived_attachments.is_empty()
    ));
}

#[tokio::test]
async fn test_speed_slash_submit_enables_fast_mode_and_compatible_model() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/speed", None).await;
    app.sessions.sessions_mut()[0].agent =
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable5);
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Speed;
        slash_state.selected_index = 1;
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    app.process_pending_app_events().await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, slash_state, .. }
            if input.is_empty() && *slash_state == PromptSlashState::default()
    ));
    assert_eq!(app.sessions.sessions()[0].speed_mode, SpeedMode::Fast);
    assert_eq!(
        app.sessions.sessions()[0].agent,
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5)
    );
}

#[tokio::test]
async fn test_q_in_chat_focus_saves_prompt_for_restore() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    let session_id = app.sessions.sessions()[0].id.clone();
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Act — `g` updates the transcript position before `q` saves the
    // complete composer and returns to the list.
    press_prompt_key(&mut app, KeyCode::Char('g')).await;
    press_prompt_key(&mut app, KeyCode::Char('q')).await;

    // Assert — the cached composer keeps the draft and scroll position,
    // then restores with input focus and consumes the cache entry.
    assert!(matches!(app.mode, AppMode::List));
    let saved_prompt = app
        .prompt_progress
        .get(&session_id)
        .expect("prompt progress should be saved");
    assert_eq!(saved_prompt.input.text(), "draft text");
    assert_eq!(saved_prompt.scroll_offset, Some(0));

    assert!(app.restore_prompt_progress(&session_id).await);
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            focus: ChatFocus::Input,
            input,
            scroll_offset: Some(0),
            ..
        } if input.text() == "draft text"
    ));
    assert!(app.prompt_progress.is_empty());
}

#[tokio::test]
async fn test_q_in_input_focus_edits_prompt() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('q')).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draftq"
    ));
    assert!(app.prompt_progress.is_empty());
}

#[tokio::test]
async fn test_prompt_context_marks_email_pattern_as_inactive_mention() {
    // Arrange
    let state = PromptAtMentionState::new(Vec::new());
    let (mut app, _base_dir) = new_test_prompt_app("email@test", Some(state)).await;

    // Act
    let context = prompt_context(&mut app).expect("expected prompt context");

    // Assert
    assert!(!context.is_at_mention());
}

#[tokio::test]
async fn test_prompt_context_falls_back_to_list_when_session_is_missing() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("follow up", None).await;
    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::with_text("follow up".to_string()),
        session_id: "missing-session".into(),
        slash_state: PromptSlashState::default(),
        scroll_offset: Some(2),
    };

    // Act
    let context = prompt_context(&mut app);

    // Assert
    assert!(context.is_none());
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
/// Verifies that when the active session is `InProgress`, a leading `/`
/// is demoted from slash-command mode to plain text so submission
/// queues the prompt instead of executing a slash command against the
/// running turn.
async fn test_prompt_context_demotes_slash_command_to_text_when_session_is_in_progress() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::InProgress;

    // Act
    let context = prompt_context(&mut app).expect("expected prompt context");

    // Assert
    assert!(
        !context.is_slash_command(),
        "slash command mode must be demoted while session is InProgress"
    );
    assert_eq!(context.input_mode, PromptInputMode::Text);
}

#[tokio::test]
/// Verifies that when the active session is `Rebasing`, a leading `/`
/// is demoted from slash-command mode to plain text so submission queues
/// the prompt behind the rebase.
async fn test_prompt_context_demotes_slash_command_to_text_when_session_is_rebasing() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Rebasing;

    // Act
    let context = prompt_context(&mut app).expect("expected prompt context");

    // Assert
    assert!(
        !context.is_slash_command(),
        "slash command mode must be demoted while session is Rebasing"
    );
    assert_eq!(context.input_mode, PromptInputMode::Text);
}

#[tokio::test]
async fn test_personality_slash_submit_loads_worktree_profile_and_selects_it() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/personality", None).await;
    app.sessions.sessions_mut()[0].personality_id = Some("reviewer".to_string());
    let session_folder = app.sessions.sessions()[0].folder.clone();
    let agent_directory = session_folder
        .join(".agents")
        .join("agents")
        .join("reviewer");
    tokio::fs::create_dir_all(&agent_directory)
        .await
        .expect("personality directory should be created");
    tokio::fs::write(
        agent_directory.join("agent.md"),
        "---\nid: reviewer\nname: Code Reviewer\ndescription: Reviews code\n---\nReview carefully.",
    )
    .await
    .expect("personality definition should be written");
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { slash_state, .. }
            if slash_state.stage == PromptSlashStage::Personality
                && slash_state.selected_index == 1
    ));

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    app.process_pending_app_events().await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, slash_state, .. }
            if input.is_empty() && *slash_state == PromptSlashState::default()
    ));
    assert_eq!(
        app.sessions.sessions()[0].personality_id.as_deref(),
        Some("reviewer")
    );
}

#[tokio::test]
async fn test_ctrl_c_keeps_chat_focus_without_canceling_prompt() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Act
    let mut terminal = test_terminal();
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('c'), event::KeyModifiers::CONTROL),
    )
    .await
    .expect("prompt key handling failed");

    // Assert
    assert_eq!(prompt_focus(&app), ChatFocus::Chat);
    let AppMode::Prompt { input, .. } = &app.mode else {
        unreachable!("expected AppMode::Prompt");
    };
    assert_eq!(input.text(), "draft text");
}

#[tokio::test]
async fn test_canceling_slash_input_discards_pasted_attachment() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/nonexistent-test-attachment.png"));
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_cancel_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            attachment_state,
            input,
            ..
        } if input.is_empty() && attachment_state.attachments.is_empty()
    ));
}

#[tokio::test]
async fn test_manually_entered_image_placeholder_does_not_restore_attachment() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;

    // Act
    handle_paste(&mut app, "[Image #1]").await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            attachment_state,
            input,
            ..
        } if input.text() == "Review [Image #1]"
            && attachment_state.attachments.is_empty()
            && attachment_state.archived_attachments.len() == 1
    ));
}

#[test]
fn test_prompt_slash_commands_match_model() {
    // Arrange & Act
    let suggestion_list = crate::presentation::prompt::build_prompt_slash_suggestion_list(
        "/m",
        &PromptSlashState::default(),
        AgentKind::Codex,
        true,
    )
    .expect("expected suggestion list");
    let commands = suggestion_list
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(commands, vec!["/mode", "/model"]);
}

/// Verifies the root slash-command menu exposes every command in display
/// order.
#[test]
fn test_prompt_slash_commands_lists_all_commands() {
    // Arrange & Act
    let suggestion_list = crate::presentation::prompt::build_prompt_slash_suggestion_list(
        "/",
        &PromptSlashState::default(),
        AgentKind::Codex,
        true,
    )
    .expect("expected suggestion list");
    let commands = suggestion_list
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        commands,
        vec![
            "/apply",
            "/mode",
            "/model",
            "/personality",
            "/reasoning",
            "/style",
            "/speed"
        ]
    );
}

#[test]
fn test_prompt_slash_commands_no_match() {
    // Arrange & Act
    let commands = crate::presentation::prompt::build_prompt_slash_suggestion_list(
        "/x",
        &PromptSlashState::default(),
        AgentKind::Codex,
        true,
    );

    // Assert
    assert!(commands.is_none());
}

#[test]
fn test_prompt_slash_option_count_for_agent_stage() {
    // Arrange & Act
    let count = prompt_slash_option_count(
        "/model",
        PromptSlashStage::Agent,
        None,
        AgentKind::ALL,
        &[],
        AgentKind::Codex,
        true,
    );

    // Assert
    assert_eq!(count, AgentKind::ALL.len());
}

#[test]
fn test_prompt_slash_option_count_for_model_stage() {
    // Arrange & Act
    let count = prompt_slash_option_count(
        "/model",
        PromptSlashStage::Model,
        Some(AgentKind::Claude),
        AgentKind::ALL,
        &[],
        AgentKind::Codex,
        true,
    );

    // Assert
    assert_eq!(count, AgentKind::Claude.models().len());
}

#[test]
fn test_prompt_slash_option_count_for_agent_stage_uses_available_agent_kinds() {
    // Arrange
    let available_agent_kinds = [AgentKind::Codex];

    // Act
    let count = prompt_slash_option_count(
        "/model",
        PromptSlashStage::Agent,
        None,
        &available_agent_kinds,
        &[],
        AgentKind::Codex,
        true,
    );

    // Assert
    assert_eq!(count, 1);
}

/// Verifies slash navigation leaves selection unchanged when the current
/// command text matches no slash-command options.
#[tokio::test]
async fn test_move_prompt_slash_selection_ignores_empty_command_matches() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/x", None).await;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.selected_index = 2;
    }

    // Act
    move_prompt_slash_selection(&mut app, true);

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.selected_index, 2);
    }
}

#[tokio::test]
async fn stale_prompt_submission_keeps_the_current_navigation() {
    // Arrange
    let (mut app, _directory) = new_test_prompt_app("draft", None).await;
    let context = prompt_context(&mut app).expect("context");
    app.mode = AppMode::List;

    // Act
    handle_prompt_submit_key(&mut app, &context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_take_prompt_snapshot_keeps_non_prompt_mode_untouched() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    app.mode = AppMode::List;

    // Act
    let snapshot = take_prompt_snapshot(&mut app);

    // Assert — nothing to capture, and the active mode is restored.
    assert!(snapshot.is_none());
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_d_key_in_chat_focus_opens_diff_with_prompt_snapshot() {
    // Arrange — prompt mode with chat focused over a worktree that has a
    // non-empty diff against its base branch.
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    let session_folder = app.sessions.sessions()[0].folder.clone();
    std::fs::write(session_folder.join("README.md"), "updated content")
        .expect("failed to write diff fixture");
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('d')).await;

    // Assert — transitioned to diff loading carrying a prompt snapshot that
    // captured the composer draft.
    assert!(
        matches!(
            &app.mode,
            AppMode::DiffLoading {
                restore: Some(restore_target),
                ..
            } if matches!(
                restore_target.as_ref(),
                DiffRestoreTarget::Prompt(snapshot) if snapshot.input.text() == "draft text"
            )
        ),
        "expected diff loading carrying a prompt restore snapshot of the draft"
    );
}

#[tokio::test]
async fn test_d_key_in_chat_focus_keeps_prompt_when_diff_empty() {
    // Arrange — prompt mode with chat focused over an unchanged worktree.
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    let mut mock_git_client = ag_git::MockGitClient::new();
    mock_git_client
        .expect_diff()
        .once()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    install_mock_git_client(&mut app, mock_git_client);
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('d')).await;
    apply_next_session_diff(&mut app).await;

    // Assert — no diff to show, so loading restores the composer with its
    // draft intact and returns focus to the editable input.
    assert_eq!(prompt_focus(&app), ChatFocus::Input);
    let snapshot = take_prompt_snapshot(&mut app).expect("expected AppMode::Prompt");
    assert_eq!(snapshot.input.text(), "draft text");
}

#[tokio::test]
async fn test_d_key_in_input_focus_inserts_character() {
    // Arrange — composer focused, so `d` is ordinary draft text.
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('d')).await;

    // Assert — the character was inserted and the composer kept prompt
    // mode.
    let snapshot = take_prompt_snapshot(&mut app).expect("expected AppMode::Prompt");
    assert_eq!(snapshot.input.text(), "draftd");
}

#[tokio::test]
async fn test_handle_at_mention_select_dismisses_stale_mention_state() {
    // Arrange
    let state = PromptAtMentionState::new(vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }]);
    let (mut app, _base_dir) = new_test_prompt_app("email@test", Some(state)).await;

    // Act
    handle_at_mention_select(&mut app).await;

    // Assert
    assert!(matches!(app.mode, AppMode::Prompt { .. }));
    if let AppMode::Prompt {
        at_mention_state,
        input,
        ..
    } = &app.mode
    {
        assert!(at_mention_state.is_none());
        assert_eq!(input.text(), "email@test");
    }
}

#[tokio::test]
async fn test_handle_at_mention_key_supports_enter_tab_and_unhandled_keys() {
    // Arrange
    let entry = FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    };
    let (mut enter_app, _enter_base_dir) =
        new_test_prompt_app("@src", Some(PromptAtMentionState::new(vec![entry.clone()]))).await;
    let (mut tab_app, _tab_base_dir) =
        new_test_prompt_app("@src", Some(PromptAtMentionState::new(vec![entry]))).await;
    let (mut ignored_app, _ignored_base_dir) =
        new_test_prompt_app("@src", Some(PromptAtMentionState::new(Vec::new()))).await;

    // Act
    let enter_handled = handle_at_mention_key(
        &mut enter_app,
        KeyEvent::new(KeyCode::Enter, event::KeyModifiers::NONE),
    )
    .await;
    let tab_handled = handle_at_mention_key(
        &mut tab_app,
        KeyEvent::new(KeyCode::Tab, event::KeyModifiers::NONE),
    )
    .await;
    let character_handled = handle_at_mention_key(
        &mut ignored_app,
        KeyEvent::new(KeyCode::Char('x'), event::KeyModifiers::NONE),
    )
    .await;

    // Assert
    assert!(enter_handled);
    assert!(tab_handled);
    assert!(!character_handled);
    assert!(matches!(
        &enter_app.mode,
        AppMode::Prompt { input, .. } if input.text() == "@src/main.rs "
    ));
    assert!(matches!(
        &tab_app.mode,
        AppMode::Prompt { input, .. } if input.text() == "@src/main.rs "
    ));
}

#[tokio::test]
async fn test_handle_at_mention_select_inserts_directory_with_trailing_slash() {
    // Arrange
    let state = PromptAtMentionState::new(vec![FileEntry {
        is_dir: true,
        path: "src".to_string(),
    }]);
    let (mut app, _base_dir) = new_test_prompt_app("@src", Some(state)).await;

    // Act
    handle_at_mention_select(&mut app).await;

    // Assert
    assert!(matches!(app.mode, AppMode::Prompt { .. }));
    if let AppMode::Prompt { input, .. } = &app.mode {
        assert_eq!(input.text(), "@src/ ");
    }
}

/// Verifies stale at-mention selections are clamped to the filtered entry
/// list before insertion.
#[tokio::test]
async fn test_handle_at_mention_select_clamps_stale_selected_index() {
    // Arrange
    let mut state = PromptAtMentionState::new(vec![
        FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "tests/main.rs".to_string(),
        },
    ]);
    state.selected_index = 9;
    let (mut app, _base_dir) = new_test_prompt_app("@src/main", Some(state)).await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "@src/ma".chars().count();
    }

    // Act
    handle_at_mention_select(&mut app).await;

    // Assert
    if let AppMode::Prompt {
        at_mention_state,
        input,
        ..
    } = &app.mode
    {
        assert!(at_mention_state.is_none());
        assert_eq!(input.text(), "@src/main.rs ");
    }
}

#[tokio::test]
async fn test_prompt_undo_restores_slash_command_context() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    apply_prompt_input_command(&mut app, InputCommand::Insert('/')).await;
    apply_prompt_input_command(&mut app, InputCommand::Insert('x')).await;

    // Act
    apply_prompt_input_command(&mut app, InputCommand::Undo).await;

    // Assert
    let context = prompt_context(&mut app).expect("prompt context should remain available");
    assert!(context.is_slash_command());
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "/"
    ));
}

#[tokio::test]
async fn test_chat_focus_key_ignores_non_prompt_mode() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    let prompt_context = prompt_context(&mut app).expect("prompt context should be available");
    app.mode = AppMode::List;
    let terminal = test_terminal();

    // Act
    let is_consumed = handle_chat_focus_key(
        &mut app,
        &RenderCacheStore::default(),
        &terminal,
        &prompt_context,
        KeyEvent::new(KeyCode::Tab, event::KeyModifiers::NONE),
    )
    .expect("chat focus handling should not fail");

    // Assert
    assert!(!is_consumed);
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_chat_focus_key_leaves_input_panel_keys_unclaimed() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    let prompt_context = prompt_context(&mut app).expect("prompt context should be available");
    let terminal = test_terminal();

    // Act
    let is_consumed = handle_chat_focus_key(
        &mut app,
        &RenderCacheStore::default(),
        &terminal,
        &prompt_context,
        KeyEvent::new(KeyCode::Char('q'), event::KeyModifiers::NONE),
    )
    .expect("chat focus handling should not fail");

    // Assert
    assert!(!is_consumed);
    assert_eq!(prompt_focus(&app), ChatFocus::Input);
}

#[tokio::test]
async fn test_handle_paste_is_ignored_while_chat_is_focused() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Act
    handle_paste(&mut app, "pasted").await;

    // Assert
    let AppMode::Prompt { input, .. } = &app.mode else {
        unreachable!("expected AppMode::Prompt");
    };
    assert_eq!(input.text(), "draft text");
}

#[tokio::test]
async fn test_handle_paste_inserts_multiline_content_with_normalized_newlines() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("prefix ", None).await;

    // Act
    handle_paste(&mut app, "line 1\r\nline 2\rline 3").await;

    // Assert
    if let AppMode::Prompt { input, .. } = &app.mode {
        assert_eq!(input.text(), "prefix line 1\nline 2\nline 3");
        assert_eq!(
            input.cursor,
            "prefix line 1\nline 2\nline 3".chars().count()
        );
    }
}

#[tokio::test]
async fn test_reasoning_slash_submit_sets_level_and_resets_input() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/reasoning", None).await;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Reasoning;
        slash_state.selected_index = 2;
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    app.process_pending_app_events().await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, slash_state, .. }
            if input.is_empty() && *slash_state == PromptSlashState::default()
    ));
    assert_eq!(
        app.sessions.sessions()[0].reasoning_level_override,
        Some(ReasoningLevel::High)
    );
}

#[tokio::test]
async fn test_esc_keeps_chat_focus_without_canceling_prompt() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Esc).await;

    // Assert
    assert_eq!(prompt_focus(&app), ChatFocus::Chat);
    let AppMode::Prompt { input, .. } = &app.mode else {
        unreachable!("expected AppMode::Prompt");
    };
    assert_eq!(input.text(), "draft text");
}

#[tokio::test]
async fn test_next_prompt_slash_selection_wraps_to_first_agent() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Agent;
        slash_state.selected_index = AgentKind::ALL.len().saturating_sub(1);
    }
    let terminal = test_terminal();
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_down_key(&mut app, &terminal, &prompt_context)
        .expect("slash selection should move down");

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Agent);
        assert_eq!(slash_state.selected_index, 0);
    }
}

#[test]
fn test_is_active_at_mention_true_for_valid_query() {
    // Arrange
    let at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
    let input = InputState::with_text("@read".to_string());

    // Act
    let result = is_active_at_mention(at_mention_state.as_ref(), &input);

    // Assert
    assert!(result);
}

#[test]
fn test_is_active_at_mention_false_for_email_pattern() {
    // Arrange
    let at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
    let input = InputState::with_text("email@test".to_string());

    // Act
    let result = is_active_at_mention(at_mention_state.as_ref(), &input);

    // Assert
    assert!(!result);
}

#[test]
fn test_is_active_at_mention_false_without_state() {
    // Arrange
    let at_mention_state = None;
    let input = InputState::with_text("@read".to_string());

    // Act
    let result = is_active_at_mention(at_mention_state.as_ref(), &input);

    // Assert
    assert!(!result);
}

#[tokio::test]
async fn test_prompt_input_command_ignores_non_prompt_mode() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
    app.mode = AppMode::List;

    // Act
    apply_prompt_input_command(&mut app, InputCommand::Insert('x')).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_previous_prompt_slash_selection_wraps_to_last_agent() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Agent;
        slash_state.selected_index = 0;
    }
    let terminal = test_terminal();
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_up_key(&mut app, &terminal, &prompt_context)
        .expect("slash selection should move up");

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Agent);
        assert_eq!(
            slash_state.selected_index,
            AgentKind::ALL.len().saturating_sub(1)
        );
    }
}

#[tokio::test]
async fn test_backtab_cycles_permission_modes_and_preserves_input() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::BackTab).await;
    let auto_address_mode = app.sessions.sessions()[0].permission_mode;
    press_prompt_key(&mut app, KeyCode::BackTab).await;
    let read_only_mode = app.sessions.sessions()[0].permission_mode;
    press_prompt_key(&mut app, KeyCode::BackTab).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draft text"
    ));
    assert_eq!(auto_address_mode, PermissionMode::AutoEditAddressComments);
    assert_eq!(read_only_mode, PermissionMode::ReadOnly);
    assert_eq!(
        app.sessions.sessions()[0].permission_mode,
        PermissionMode::AutoEdit
    );
}

#[tokio::test]
async fn test_navigate_prompt_history_up_stays_on_first_entry() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &mut app.mode
    {
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(0);
        *input = InputState::with_text("first".to_string());
    }

    // Act
    navigate_prompt_history_up(&mut app);

    // Assert
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "first");
        assert_eq!(history_state.selected_index, Some(0));
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_navigate_prompt_history_down_selects_next_entry() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("first", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.entries = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        history_state.selected_index = Some(0);
    }

    // Act
    navigate_prompt_history_down(&mut app);

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            history_state,
            input,
            ..
        } if input.text() == "second" && history_state.selected_index == Some(1)
    ));
}

#[tokio::test]
async fn test_prompt_mode_handler_edits_and_submits_input() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;

    // Act
    press_prompt_key(&mut app, KeyCode::Char('!')).await;
    press_prompt_key(&mut app, KeyCode::Enter).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(app.sessions.sessions()[0].prompt, "draft!");
}

#[tokio::test]
async fn test_submit_current_text_prompt_ignores_missing_context_and_demotes_slash_text() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    let original_mode = std::mem::replace(&mut app.mode, AppMode::List);

    // Act
    submit_current_text_prompt(&mut app).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));

    // Arrange
    app.mode = original_mode;

    // Act
    submit_current_text_prompt(&mut app).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
}

#[tokio::test]
async fn test_submit_current_text_prompt_submits_normal_prompt() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("follow up", None).await;

    // Act
    submit_current_text_prompt(&mut app).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
}

#[tokio::test]
async fn test_style_slash_submit_sets_preference_and_resets_input() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/style", None).await;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Style;
        slash_state.selected_index = 2;
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    app.process_pending_app_events().await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, slash_state, .. }
            if input.is_empty() && *slash_state == PromptSlashState::default()
    ));
    assert_eq!(
        app.sessions.sessions()[0].response_style,
        ResponseStyle::Detailed
    );
}

#[tokio::test]
async fn test_mode_slash_command_selects_auto_address_mode() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/mode", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;
    press_prompt_key(&mut app, KeyCode::Down).await;
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            input, slash_state, ..
        } if input.text().is_empty() && *slash_state == PromptSlashState::default()
    ));
    assert_eq!(
        app.sessions.sessions()[0].permission_mode,
        PermissionMode::AutoEditAddressComments
    );
}

#[tokio::test]
async fn test_apply_prompt_apply_outcome_keeps_composer_for_retry() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/apply", None).await;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Model;
        slash_state.selected_agent = Some(AgentKind::Codex);
        slash_state.selected_index = 2;
    }

    // Act
    apply_prompt_apply_outcome(&mut app, PromptApplyOutcome::KeepComposer).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            input, slash_state, ..
        } if input.text() == "/apply" && *slash_state == PromptSlashState::default()
    ));
}

#[tokio::test]
async fn test_show_prompt_diff_restores_composer_when_session_is_missing() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;

    // Act
    show_prompt_diff(&mut app, "missing-session");

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draft text"
    ));
}

#[tokio::test]
async fn test_model_slash_submit_discards_pasted_image_before_normal_submission() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/nonexistent-test-attachment.png"));
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Model;
        slash_state.selected_agent = Some(AgentKind::Claude);
        slash_state.selected_index = 0;
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.reset_text("normal prompt".to_string());
    }
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, "normal prompt");
    assert_eq!(
        prompt.attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
}

#[tokio::test]
async fn test_insert_pasted_image_placeholder_records_attachment_and_resets_prompt_state() {
    // Arrange
    let mut at_mention_state = PromptAtMentionState::new(vec![FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }]);
    at_mention_state.selected_index = 4;
    let (mut app, _base_dir) = new_test_prompt_app("Review ", Some(at_mention_state)).await;
    if let AppMode::Prompt {
        history_state,
        slash_state,
        ..
    } = &mut app.mode
    {
        history_state.selected_index = Some(0);
        history_state.draft_text = Some("draft".to_string());
        slash_state.selected_index = 2;
    }

    // Act
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));

    // Assert
    if let AppMode::Prompt {
        at_mention_state,
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "Review [Image #1]");
        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(
            attachment_state.attachments[0].local_image_path,
            PathBuf::from("/tmp/image-1.png")
        );
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
        assert_eq!(*slash_state, PromptSlashState::default());
        assert!(at_mention_state.is_none());
    }
}

#[tokio::test]
async fn test_handle_prompt_image_paste_uses_injected_clipboard_image_client() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let expected_session_id = prompt_context.session_id.as_str().to_string();
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .withf(move |session_id, attachment_number| {
            session_id == &expected_session_id && *attachment_number == 1
        })
        .returning(|_, _| {
            Box::pin(async {
                Ok(crate::infra::clipboard_image::PersistedClipboardImage {
                    local_image_path: PathBuf::from("/tmp/pasted.png"),
                })
            })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);

    // Act
    handle_prompt_image_paste(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "Review [Image #1]");
        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(
            attachment_state.attachments[0].local_image_path,
            PathBuf::from("/tmp/pasted.png")
        );
    }
}

#[tokio::test]
async fn test_handle_prompt_image_paste_reports_injected_clipboard_image_errors() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .returning(|_, _| {
            Box::pin(async { Err(crate::infra::clipboard_image::ClipboardError::NoImage) })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);

    // Act
    handle_prompt_image_paste(&mut app, &prompt_context).await;

    // Assert
    app.sessions.sync_from_handles();
    assert!(
        session_replay_text(&app.sessions.sessions()[0])
            .contains("[Paste Image Error] Clipboard does not contain an image.")
    );
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "Review ");
        assert_eq!(
            attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
    }
}

/// Verifies unavailable clipboard backends surface as inline paste errors
/// without mutating prompt attachments.
#[tokio::test]
async fn test_handle_prompt_image_paste_reports_unavailable_clipboard_backend() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Err(crate::infra::clipboard_image::ClipboardError::Unavailable {
                    reason: "unsupported clipboard backend".to_string(),
                })
            })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);

    // Act
    handle_prompt_image_paste(&mut app, &prompt_context).await;

    // Assert
    app.sessions.sync_from_handles();
    assert!(session_replay_text(&app.sessions.sessions()[0]).contains(
        "[Paste Image Error] Clipboard is unavailable. Try again after granting clipboard access."
    ));
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "Review ");
        assert_eq!(
            attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_advances_model_command_to_agent_stage() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt {
        input, slash_state, ..
    } = &app.mode
    {
        assert_eq!(input.text(), "/model");
        assert_eq!(slash_state.stage, PromptSlashStage::Agent);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 0);
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_ignores_non_prompt_mode() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    app.mode = AppMode::List;

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_maps_filtered_first_command_to_mode() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Mode);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 0);
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_selects_agent_and_advances_to_model_stage() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    let selected_index = AgentKind::ALL
        .iter()
        .position(|agent_kind| *agent_kind == AgentKind::Claude)
        .expect("expected Claude agent");
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Agent;
        slash_state.selected_index = selected_index;
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Model);
        assert_eq!(slash_state.selected_agent, Some(AgentKind::Claude));
        assert_eq!(slash_state.selected_index, 0);
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_sets_selected_model_and_resets_input() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    let expected_model = AgentKind::Claude.models()[0];
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Model;
        slash_state.selected_agent = Some(AgentKind::Claude);
        slash_state.selected_index = 0;
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;
    app.process_pending_app_events().await;

    // Assert
    if let AppMode::Prompt {
        input, slash_state, ..
    } = &app.mode
    {
        assert_eq!(input.text(), "");
        assert_eq!(*slash_state, PromptSlashState::default());
    }
    assert_eq!(app.sessions.sessions()[0].agent.model(), expected_model);
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_prefills_reasoning_selection_from_session_value() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/reasoning", None).await;
    app.settings.default_smart_reasoning_level = ReasoningLevel::Medium;
    app.sessions.sessions_mut()[0].reasoning_level_override = Some(ReasoningLevel::Low);
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Reasoning);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 0);
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_prefills_reasoning_selection_from_session_override() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/reasoning", None).await;
    app.sessions.sessions_mut()[0].reasoning_level_override = Some(ReasoningLevel::High);
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Reasoning);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 2);
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_prefills_response_style_selection() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/style", None).await;
    app.sessions.sessions_mut()[0].response_style = ResponseStyle::Detailed;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Style);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 2);
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_prefills_speed_selection() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/speed", None).await;
    app.sessions.sessions_mut()[0].agent =
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol);
    app.sessions.sessions_mut()[0].speed_mode = SpeedMode::Fast;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Speed);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 1);
    }
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_clamps_stale_command_selection() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/reasoning", None).await;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.selected_index = 99;
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt { slash_state, .. } = &app.mode {
        assert_eq!(slash_state.stage, PromptSlashStage::Reasoning);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 2);
    }
}

/// Verifies slash submit ignores unmatched commands and preserves the
/// prompt state.
#[tokio::test]
async fn test_handle_prompt_slash_submit_ignores_unknown_command() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/x", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    if let AppMode::Prompt {
        input, slash_state, ..
    } = &app.mode
    {
        assert_eq!(input.text(), "/x");
        assert_eq!(*slash_state, PromptSlashState::default());
    }
}

#[tokio::test]
async fn test_handle_prompt_left_with_alt_moves_cursor_to_previous_word_start() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "hello brave world".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::MoveWordLeft).await;

    // Assert
    if let AppMode::Prompt { input, .. } = &app.mode {
        assert_eq!(input.cursor, "hello brave ".chars().count());
    }
}

#[tokio::test]
async fn test_handle_prompt_right_with_alt_moves_cursor_to_next_word_start() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = 0;
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::MoveWordRight).await;

    // Assert
    if let AppMode::Prompt { input, .. } = &app.mode {
        assert_eq!(input.cursor, "hello ".chars().count());
    }
}

#[tokio::test]
async fn test_handle_prompt_left_with_super_moves_cursor_to_line_start() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("first\nsecond\nthird", None).await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "first\nseco".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::MoveLineStart).await;

    // Assert
    if let AppMode::Prompt { input, .. } = &app.mode {
        assert_eq!(input.cursor, "first\n".chars().count());
    }
}

#[tokio::test]
async fn test_handle_prompt_right_with_super_moves_cursor_to_line_end() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("first\nsecond\nthird", None).await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "first\nse".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::MoveLineEnd).await;

    // Assert
    if let AppMode::Prompt { input, .. } = &app.mode {
        assert_eq!(input.cursor, "first\nsecond".chars().count());
    }
}

#[tokio::test]
async fn test_handle_prompt_backspace_resets_history_navigation() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("second", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.draft_text = Some("draft".to_string());
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(1);
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;

    // Assert
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "secon");
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_handle_prompt_backspace_on_empty_input_is_noop() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;

    // Assert
    if let AppMode::Prompt { input, .. } = &app.mode {
        assert!(input.is_empty());
        assert_eq!(input.cursor, 0);
    }
}

#[tokio::test]
async fn test_handle_prompt_left_reactivates_existing_at_mention_without_cached_state() {
    // Arrange
    let input_text = "@src/main.rs more";
    let (mut app, _base_dir) = new_test_prompt_app(input_text, None).await;
    let moves_back_into_mention = " more".chars().count();

    // Act
    for _ in 0..moves_back_into_mention {
        apply_prompt_input_command(&mut app, InputCommand::MoveLeft).await;
    }

    // Assert
    if let AppMode::Prompt {
        at_mention_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.cursor, "@src/main.rs".chars().count());
        assert!(at_mention_state.is_some());
    }
}

#[tokio::test]
async fn test_handle_prompt_submit_key_ignores_empty_prompt() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::Prompt { .. }));
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].prompt, "");
}

#[tokio::test]
async fn test_handle_prompt_submit_key_routes_slash_command() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            input, slash_state, ..
        } if input.text() == "/model" && slash_state.stage == PromptSlashStage::Agent
    ));
}

#[tokio::test]
async fn test_handle_prompt_submit_key_drains_supported_image_turn() {
    // Arrange
    let (mut app, _base_dir) = new_test_draft_prompt_app("Review ", None).await;
    app.sessions.sessions_mut()[0].agent =
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5);
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(app.sessions.sessions()[0].prompt, "Review [Image #1]");
    assert_eq!(
        app.sessions.sessions()[0].status,
        crate::domain::session::Status::Draft
    );
    assert_eq!(app.sessions.sessions()[0].draft_attachments.len(), 1);
    assert_eq!(
        app.sessions.sessions()[0].draft_attachments[0].placeholder,
        "[Image #1]"
    );
}

#[tokio::test]
async fn test_handle_prompt_submit_key_starts_regular_session_with_image_turn() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    app.sessions.sessions_mut()[0].agent =
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5);
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(app.sessions.sessions()[0].prompt, "Review [Image #1]");
    assert_eq!(
        app.sessions.sessions()[0].title.as_deref(),
        Some("Review [Image #1]")
    );
    assert_eq!(
        app.sessions.sessions()[0].draft_attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_preserves_attachments_when_apply_bails_out() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/apply", None).await;
    if let AppMode::Prompt {
        attachment_state, ..
    } = &mut app.mode
    {
        attachment_state.attachments.push(PromptAttachment::new(
            1,
            PathBuf::from("/tmp/nonexistent-test-attachment.png"),
        ));
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    let AppMode::Prompt {
        attachment_state, ..
    } = &app.mode
    else {
        unreachable!("expected AppMode::Prompt after /apply bail-out");
    };
    assert_eq!(
        attachment_state.attachments.len(),
        1,
        "attachments must survive validation failure so the user keeps their pasted files",
    );
}

#[tokio::test]
async fn test_handle_prompt_slash_submit_ignores_apply_when_suggestions_are_empty() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/apply", None).await;
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Review;
    let session_id = app.sessions.sessions()[0].id.clone();
    app.review_cache.insert(
        session_id.clone(),
        crate::app::ReviewCacheEntry::Ready {
            diff_hash: 0,
            text: "## Review\n### Suggestions\n- None".to_string(),
        },
    );
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_slash_submit(&mut app, &prompt_context).await;

    // Assert
    let AppMode::Prompt {
        input, slash_state, ..
    } = &app.mode
    else {
        unreachable!("expected AppMode::Prompt after unavailable /apply");
    };
    assert_eq!(input.text(), "/apply");
    assert_eq!(*slash_state, PromptSlashState::default());
    assert!(
        matches!(
            app.review_cache.get(session_id.as_str()),
            Some(crate::app::ReviewCacheEntry::Ready { .. }),
        ),
        "unavailable /apply should not consume the cached review",
    );
}

#[test]
fn test_is_prompt_image_paste_key_accepts_alt_v() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::ALT);

    // Act
    let result = is_prompt_image_paste_key(key);

    // Assert
    assert!(result);
}

#[test]
fn test_is_prompt_image_paste_key_accepts_ctrl_v() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::CONTROL);

    // Act
    let result = is_prompt_image_paste_key(key);

    // Assert
    assert!(result);
}

#[test]
fn test_is_prompt_image_paste_key_accepts_ctrl_shift_v() {
    // Arrange
    let key = KeyEvent::new(
        KeyCode::Char('V'),
        event::KeyModifiers::CONTROL | event::KeyModifiers::SHIFT,
    );

    // Act
    let result = is_prompt_image_paste_key(key);

    // Assert
    assert!(result);
}

#[test]
fn test_is_prompt_image_paste_key_rejects_plain_v() {
    // Arrange
    let key = KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::NONE);

    // Act
    let result = is_prompt_image_paste_key(key);

    // Assert
    assert!(!result);
}

#[tokio::test]
async fn test_take_submitted_turn_prompt_drains_text_and_attachment_state() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));

    // Act
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, "Review [Image #1]");
    assert_eq!(prompt.attachments.len(), 1);
    assert_eq!(prompt.attachments[0].placeholder, "[Image #1]");
    assert_eq!(
        prompt.attachments[0].local_image_path,
        PathBuf::from("/tmp/image-1.png")
    );
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "");
        assert_eq!(
            attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(attachment_state.next_attachment_number, 1);
    }
}

#[tokio::test]
async fn test_take_submitted_turn_prompt_sorts_attachments_by_text_position() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = 0;
    }
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-2.png"));
    handle_paste(&mut app, " then ").await;

    // Act
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.attachments.len(), 2);
    assert_eq!(prompt.attachments[0].placeholder, "[Image #2]");
    assert_eq!(prompt.attachments[1].placeholder, "[Image #1]");
}

#[tokio::test]
async fn test_prompt_attachment_helpers_ignore_non_prompt_mode() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    app.mode = AppMode::List;
    let session_id = SessionId::from("session-id");

    // Act
    paste_image_into_active_prompt(&mut app, &session_id).await;
    let inserted_attachments =
        insert_pasted_image_placeholder(&mut app, PathBuf::from("/tmp/image-1.png"));
    let (prompt, archived_attachments) = take_submitted_turn_prompt(&mut app);
    let cleanup_attachments = take_prompt_attachment_cleanup(&mut app);

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert_eq!(
        inserted_attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
    assert!(prompt.is_empty());
    assert_eq!(
        archived_attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
    assert_eq!(
        cleanup_attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
}
