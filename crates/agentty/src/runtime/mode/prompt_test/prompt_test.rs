use std::path::PathBuf;

use ag_session::test_support as model_fixture;
use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};

use super::super::{
    PromptEffect, PromptInputMode, apply_prompt_apply_outcome, apply_prompt_effects,
    apply_prompt_input_to_mode, apply_prompt_paste_to_mode, handle_paste, handle_with_cache,
    is_active_at_mention, is_prompt_image_paste_key, mark_prompt_image_paste_pending,
    move_prompt_slash_selection, navigate_prompt_history_down, navigate_prompt_history_up,
    paste_image_into_active_prompt, prompt_context, prompt_input_mode, prompt_session_mode,
    reduce_at_mention_key, reduce_chat_focus_key, reduce_prompt_input_command,
    reduce_prompt_slash_submit, select_at_mention, submit_current_text_prompt,
    take_prompt_attachment_cleanup, take_prompt_snapshot, take_submitted_turn_prompt,
};
use super::support::{
    PromptTestAppExt, PromptTestModeExt, apply_next_prompt_image_paste, apply_next_session_diff,
    expect_prompt_effects, install_mock_clipboard_image_client, install_mock_fs_client,
    install_mock_git_client, mode_focus, mode_input, new_test_draft_prompt_app,
    new_test_prompt_app, new_test_prompt_context, new_test_prompt_mode, press_prompt_key,
    prompt_focus, reduce_prompt_code, reduce_prompt_event, reduce_test_prompt_key,
    refresh_test_input_mode, run_prompt_cancel, run_prompt_slash_submit, run_prompt_submit,
    session_replay_text, test_terminal,
};
use crate::app::AppEvent;
use crate::app::prompt_intent::{PromptApplyOutcome, PromptImagePasteUpdate, PromptSessionMode};
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
};
use crate::domain::composer::PromptAttachment;
use crate::domain::file_entry::FileEntry;
use crate::domain::input::{INPUT_HISTORY_LIMIT, InputCommand, InputState};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{SessionId, Status};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::presentation::app_mode::{AppMode, ChatFocus, DiffRestoreTarget};
use crate::presentation::prompt::{
    PendingImagePaste, PromptAtMentionState, PromptGraphState, PromptSlashStage, PromptSlashState,
    prompt_slash_option_count,
};
use crate::test_support::{SessionFixtureBuilder, session_fixture};
use crate::ui::RenderCacheStore;

#[test]
fn test_prompt_edit_prunes_attachment_after_undo_revision_eviction() {
    // Arrange
    let mut mode = new_test_prompt_mode("");
    mode.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::DeleteBackward),
        [] as [PromptAttachment; 0]
    );

    // Act
    let mut cleanup = Vec::new();
    for _ in 0..INPUT_HISTORY_LIMIT {
        cleanup.extend(apply_prompt_input_to_mode(
            &mut mode,
            InputCommand::Insert('x'),
        ));
    }

    // Assert
    assert_eq!(cleanup.len(), 1);
    assert_eq!(
        cleanup[0].local_image_path,
        PathBuf::from("/tmp/image-1.png")
    );
    assert!(matches!(
        &mode,
        AppMode::Prompt {
            attachment_state, ..
        } if attachment_state.attachments.is_empty()
            && attachment_state.archived_attachments.is_empty()
    ));
}

#[tokio::test]
async fn test_fast_slash_submit_toggles_speed_and_preserves_compatible_model() {
    for (initial_agent, fast_agent) in [
        (
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeFable52),
            AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
        ),
        (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt53CodexSpark),
            model_fixture::codex_selection(),
        ),
    ] {
        // Arrange
        let (mut app, _base_dir) = new_test_prompt_app("/fast", None).await;
        app.sessions.sessions_mut()[0].agent = initial_agent;
        app.sessions.sessions_mut()[0].speed_mode = SpeedMode::Normal;
        let context = prompt_context(&mut app).expect("expected prompt context");

        for expected_speed in [SpeedMode::Fast, SpeedMode::Normal] {
            if let AppMode::Prompt { input, .. } = &mut app.mode {
                *input = InputState::with_text("/fast".to_string());
            }

            // Act
            press_prompt_key(&mut app, KeyCode::Enter).await;
            app.process_pending_app_events().await;
            let persisted_speed_mode = app
                .services
                .db()
                .sessions()
                .load_session_speed_mode(&context.session_id)
                .await
                .expect("speed mode should load");

            // Assert
            assert!(matches!(
                &app.mode,
                AppMode::Prompt { input, slash_state, .. }
                    if input.is_empty() && *slash_state == PromptSlashState::default()
            ));
            assert_eq!(app.sessions.sessions()[0].speed_mode, expected_speed);
            assert_eq!(persisted_speed_mode, expected_speed);
            assert_eq!(app.sessions.sessions()[0].agent, fast_agent);
        }
    }
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

#[test]
fn test_q_in_input_focus_edits_prompt() {
    // Arrange
    let mut mode = new_test_prompt_mode("draft");

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Char('q'));

    // Assert
    assert_eq!(mode_input(&mode).0, "draftq");
    assert!(effects.is_empty());
}

#[test]
fn test_prompt_input_mode_routes_mentions_slash_commands_and_queued_text() {
    for (at_mention_state, input_text, slash_state, queues_messages, expected_input_mode) in [
        (
            Some(PromptAtMentionState::new(Vec::new())),
            "@read",
            PromptSlashState::default(),
            false,
            PromptInputMode::AtMention,
        ),
        (
            Some(PromptAtMentionState::new(Vec::new())),
            "email@test",
            PromptSlashState::default(),
            false,
            PromptInputMode::Text,
        ),
        (
            None,
            "/model",
            PromptSlashState::default(),
            false,
            PromptInputMode::SlashCommand,
        ),
        (
            None,
            "/model",
            PromptSlashState::default(),
            true,
            PromptInputMode::Text,
        ),
        (
            None,
            "/mode is part of my task",
            PromptSlashState {
                graph: PromptGraphState::Armed,
                ..PromptSlashState::default()
            },
            false,
            PromptInputMode::Text,
        ),
    ] {
        // Arrange
        let input = InputState::with_text(input_text.to_string());

        // Act
        let input_mode = prompt_input_mode(
            at_mention_state.as_ref(),
            &input,
            &slash_state,
            queues_messages,
        );

        // Assert
        assert_eq!(input_mode, expected_input_mode, "{input_text}");
    }
}

/// Verifies that only queueing statuses demote a leading `/` to plain text:
/// `InProgress` and `Rebasing` queue the prompt behind the active operation,
/// while `Review` keeps the slash submit path.
#[tokio::test]
async fn test_prompt_context_demotes_slash_commands_only_while_session_queues_messages() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;

    for (status, expected_input_mode) in [
        (Status::InProgress, PromptInputMode::Text),
        (Status::Rebasing, PromptInputMode::Text),
        (Status::Review, PromptInputMode::SlashCommand),
    ] {
        app.sessions.sessions_mut()[0].status = status;

        // Act
        let context = prompt_context(&mut app).expect("expected prompt context");

        // Assert
        assert_eq!(context.input_mode, expected_input_mode, "{status:?}");
    }
}

#[test]
fn test_prompt_session_mode_routes_new_draft_regular_and_existing_sessions() {
    // Arrange
    let mut preparing_session = session_fixture("preparing", Status::Draft);
    preparing_session
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Plain(String::new()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::WorkspacePreparation,
            turn_position: None,
        });
    let cases = [
        (
            SessionFixtureBuilder::new()
                .status(Status::Draft)
                .draft(true)
                .build(),
            PromptSessionMode::NewDraft,
        ),
        (preparing_session, PromptSessionMode::NewRegular),
        (
            session_fixture("blank", Status::Draft),
            PromptSessionMode::NewDeletable,
        ),
        (
            session_fixture("started", Status::Review),
            PromptSessionMode::Existing,
        ),
    ];

    for (session, expected_session_mode) in cases {
        // Act
        let session_mode = prompt_session_mode(&session);

        // Assert
        assert_eq!(session_mode, expected_session_mode);
    }
}

#[test]
fn test_cancel_keys_in_chat_focus_keep_the_prompt() {
    for key in [
        KeyEvent::new(KeyCode::Char('c'), event::KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Esc, event::KeyModifiers::NONE),
    ] {
        // Arrange
        let mut mode = new_test_prompt_mode("draft text");
        reduce_prompt_code(&mut mode, KeyCode::Tab);

        // Act
        let effects = reduce_prompt_event(&mut mode, key);

        // Assert
        assert_eq!(mode_focus(&mode), ChatFocus::Chat);
        assert_eq!(mode_input(&mode).0, "draft text");
        assert!(effects.is_empty());
    }
}

#[test]
fn test_canceling_slash_input_discards_pasted_attachment() {
    // Arrange
    let image_path = PathBuf::from("/tmp/nonexistent-test-attachment.png");
    let mut mode = new_test_prompt_mode("/model");
    mode.insert_pasted_image_placeholder(image_path.clone());

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Esc);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt {
            attachment_state,
            input,
            ..
        } if input.is_empty() && attachment_state.attachments.is_empty()
    ));
    assert!(matches!(
        effects.as_slice(),
        [PromptEffect::CleanupAttachments(attachments)]
            if attachments.len() == 1 && attachments[0].local_image_path == image_path
    ));
}

#[test]
fn test_manually_entered_image_placeholder_does_not_restore_attachment() {
    // Arrange
    let mut mode = new_test_prompt_mode("Review ");
    mode.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::DeleteBackward),
        [] as [PromptAttachment; 0]
    );

    // Act
    assert_eq!(
        apply_prompt_paste_to_mode(&mut mode, "[Image #1]"),
        Some(Vec::new())
    );

    // Assert
    assert!(matches!(
        &mode,
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
        vec!["/apply", "/mode", "/model", "/reasoning", "/style", "/fast"]
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
        &PromptSlashState {
            stage: PromptSlashStage::Agent,
            ..PromptSlashState::default()
        },
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
        &PromptSlashState {
            stage: PromptSlashStage::Model,
            selected_agent: Some(AgentKind::Claude),
            ..PromptSlashState::default()
        },
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
        &PromptSlashState {
            stage: PromptSlashStage::Agent,
            available_agent_kinds: available_agent_kinds.to_vec(),
            ..PromptSlashState::default()
        },
        AgentKind::Codex,
        true,
    );

    // Assert
    assert_eq!(count, 1);
}

/// Verifies slash navigation leaves selection unchanged when the current
/// command text matches no slash-command options.
#[test]
fn test_move_prompt_slash_selection_ignores_empty_command_matches() {
    // Arrange
    let mut mode = new_test_prompt_mode("/x");
    if let AppMode::Prompt { slash_state, .. } = &mut mode {
        slash_state.selected_index = 2;
    }

    // Act
    move_prompt_slash_selection(&mut mode, &new_test_prompt_context(), true);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { slash_state, .. } if slash_state.selected_index == 2
    ));
}

#[test]
fn test_take_prompt_snapshot_keeps_non_prompt_mode_untouched() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    let snapshot = take_prompt_snapshot(&mut mode);

    // Assert — nothing to capture, and the active mode is restored.
    assert!(snapshot.is_none());
    assert!(matches!(mode, AppMode::List));
}

#[test]
fn test_d_key_in_chat_focus_opens_diff_with_prompt_snapshot() {
    // Arrange
    let mut mode = new_test_prompt_mode("draft text");
    reduce_prompt_code(&mut mode, KeyCode::Tab);

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Char('d'));

    // Assert — the composer moved into the diff request's restore snapshot.
    assert!(matches!(mode, AppMode::List));
    assert!(
        matches!(
            effects.as_slice(),
            [PromptEffect::OpenDiff {
                restore: Some(DiffRestoreTarget::Prompt(snapshot)),
            }] if snapshot.input.text() == "draft text"
        ),
        "expected a diff request carrying a prompt restore snapshot of the draft"
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
    let snapshot = take_prompt_snapshot(&mut app.mode).expect("expected AppMode::Prompt");
    assert_eq!(snapshot.input.text(), "draft text");
}

#[test]
fn test_d_key_in_input_focus_inserts_character() {
    // Arrange — composer focused, so `d` is ordinary draft text.
    let mut mode = new_test_prompt_mode("draft");

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Char('d'));

    // Assert — the character was inserted and the composer kept prompt
    // mode.
    assert_eq!(mode_input(&mode).0, "draftd");
    assert!(effects.is_empty());
}

/// Builds a prompt with `input_text` and an open mention dropdown.
fn mention_mode(input_text: &str, at_mention_state: PromptAtMentionState) -> AppMode {
    let mut mode = new_test_prompt_mode(input_text);
    if let AppMode::Prompt {
        at_mention_state: state,
        ..
    } = &mut mode
    {
        *state = Some(at_mention_state);
    }

    mode
}

#[test]
fn test_handle_at_mention_select_dismisses_stale_mention_state() {
    // Arrange
    let mut mode = mention_mode(
        "email@test",
        PromptAtMentionState::new(vec![FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }]),
    );
    let mut effects = Vec::new();

    // Act
    select_at_mention(&mut mode, &new_test_prompt_context(), &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { at_mention_state, input, .. }
            if at_mention_state.is_none() && input.text() == "email@test"
    ));
    assert!(effects.is_empty());
}

#[test]
fn test_handle_at_mention_key_supports_enter_tab_and_unhandled_keys() {
    // Arrange
    let entry = FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    };
    let mut enter_mode = mention_mode("@src", PromptAtMentionState::new(vec![entry.clone()]));
    let mut tab_mode = mention_mode("@src", PromptAtMentionState::new(vec![entry]));
    let mut ignored_mode = mention_mode("@src", PromptAtMentionState::new(Vec::new()));
    let prompt_context = new_test_prompt_context();
    let mut effects = Vec::new();

    // Act
    let enter_handled = reduce_at_mention_key(
        &mut enter_mode,
        &prompt_context,
        KeyEvent::new(KeyCode::Enter, event::KeyModifiers::NONE),
        &mut effects,
    );
    let tab_handled = reduce_at_mention_key(
        &mut tab_mode,
        &prompt_context,
        KeyEvent::new(KeyCode::Tab, event::KeyModifiers::NONE),
        &mut effects,
    );
    let character_handled = reduce_at_mention_key(
        &mut ignored_mode,
        &prompt_context,
        KeyEvent::new(KeyCode::Char('x'), event::KeyModifiers::NONE),
        &mut effects,
    );

    // Assert
    assert!(enter_handled);
    assert!(tab_handled);
    assert!(!character_handled);
    assert_eq!(mode_input(&enter_mode).0, "@src/main.rs ");
    assert_eq!(mode_input(&tab_mode).0, "@src/main.rs ");
}

#[test]
fn test_handle_at_mention_select_inserts_directory_with_trailing_slash() {
    // Arrange
    let mut mode = mention_mode(
        "@src",
        PromptAtMentionState::new(vec![FileEntry {
            is_dir: true,
            path: "src".to_string(),
        }]),
    );
    let mut effects = Vec::new();

    // Act
    select_at_mention(&mut mode, &new_test_prompt_context(), &mut effects);

    // Assert
    assert_eq!(mode_input(&mode).0, "@src/ ");
}

/// Verifies stale at-mention selections are clamped to the filtered entry
/// list before insertion.
#[test]
fn test_handle_at_mention_select_clamps_stale_selected_index() {
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
    let mut mode = mention_mode("@src/main", state);
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.cursor = "@src/ma".chars().count();
    }
    let mut effects = Vec::new();

    // Act
    select_at_mention(&mut mode, &new_test_prompt_context(), &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { at_mention_state, input, .. }
            if at_mention_state.is_none() && input.text() == "@src/main.rs "
    ));
}

#[test]
fn test_prompt_undo_restores_slash_command_context() {
    // Arrange
    let mut mode = new_test_prompt_mode("");
    let mut prompt_context = new_test_prompt_context();
    let mut effects = Vec::new();
    reduce_prompt_input_command(
        &mut mode,
        &prompt_context,
        InputCommand::Insert('/'),
        &mut effects,
    );
    reduce_prompt_input_command(
        &mut mode,
        &prompt_context,
        InputCommand::Insert('x'),
        &mut effects,
    );

    // Act
    reduce_prompt_input_command(&mut mode, &prompt_context, InputCommand::Undo, &mut effects);

    // Assert
    refresh_test_input_mode(&mode, &mut prompt_context);
    assert!(prompt_context.is_slash_command());
    assert_eq!(mode_input(&mode).0, "/");
}

#[test]
fn test_chat_focus_key_ignores_non_prompt_mode() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    let step = reduce_chat_focus_key(
        &mut mode,
        KeyEvent::new(KeyCode::Tab, event::KeyModifiers::NONE),
    );

    // Assert
    assert!(step.is_none());
    assert!(matches!(mode, AppMode::List));
}

#[test]
fn test_chat_focus_key_leaves_input_panel_keys_unclaimed() {
    // Arrange
    let mut mode = new_test_prompt_mode("draft text");

    // Act
    let step = reduce_chat_focus_key(
        &mut mode,
        KeyEvent::new(KeyCode::Char('q'), event::KeyModifiers::NONE),
    );

    // Assert
    assert!(step.is_none());
    assert_eq!(mode_focus(&mode), ChatFocus::Input);
}

#[tokio::test]
async fn test_handle_paste_is_ignored_while_chat_is_focused_or_outside_prompt() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    press_prompt_key(&mut app, KeyCode::Tab).await;

    // Act
    handle_paste(&mut app, "pasted").await;
    let chat_focused_text = mode_input(&app.mode).0;
    app.mode = AppMode::List;
    handle_paste(&mut app, "pasted").await;

    // Assert
    assert_eq!(chat_focused_text, "draft text");
    assert!(matches!(app.mode, AppMode::List));
}

#[test]
fn test_handle_paste_inserts_multiline_content_with_normalized_newlines() {
    // Arrange
    let mut mode = new_test_prompt_mode("prefix ");

    // Act
    assert_eq!(
        apply_prompt_paste_to_mode(&mut mode, "line 1\r\nline 2\rline 3"),
        Some(Vec::new())
    );

    // Assert
    assert_eq!(apply_prompt_paste_to_mode(&mut mode, ""), None);
    if let AppMode::Prompt { input, .. } = &mode {
        assert_eq!(input.text(), "prefix line 1\nline 2\nline 3");
        assert_eq!(
            input.cursor,
            "prefix line 1\nline 2\nline 3".chars().count()
        );
    }
}

#[tokio::test]
async fn test_model_command_in_harness_session_opens_refreshed_harness_models() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("/model", None).await;
    crate::test_support::set_test_harness_models(&mut app.services, true, Some(AgentModel::KimiK3));
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        *slash_state = PromptSlashState::with_available_agent_kinds(vec![AgentKind::Harness]);
    }
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    run_prompt_slash_submit(&mut app, &prompt_context).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let event = app.next_app_event().await.expect("refresh event");
            app.apply_app_events(event).await;
            if matches!(&app.mode, AppMode::Prompt { slash_state, .. }
                if slash_state.harness_models.iter().any(|option| option.model == AgentModel::KimiK3)) {
                break;
            }
        }
    }).await.expect("harness model refresh should finish");

    // Assert
    let AppMode::Prompt { slash_state, .. } = &app.mode else {
        unreachable!("expected AppMode::Prompt");
    };
    assert_eq!(slash_state.stage, PromptSlashStage::Model);
    assert_eq!(slash_state.selected_agent, Some(AgentKind::Harness));
    assert_eq!(
        slash_state
            .harness_models
            .iter()
            .map(|option| option.model)
            .collect::<Vec<_>>(),
        vec![AgentModel::KimiK3]
    );
}

#[test]
fn test_model_slash_submit_on_sign_in_row_opens_the_sign_in_popup() {
    // Arrange
    let chatgpt_option =
        crate::test_support::sign_in_required_harness_model(AgentModel::ChatGptGpt6Sol);
    let mut mode = new_test_prompt_mode("/model");
    if let AppMode::Prompt { slash_state, .. } = &mut mode {
        *slash_state = PromptSlashState {
            harness_models: vec![chatgpt_option],
            selected_agent: Some(AgentKind::Harness),
            selected_index: 1,
            stage: PromptSlashStage::Model,
            ..PromptSlashState::with_available_agent_kinds(vec![AgentKind::Harness])
        };
    }

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Enter);

    // Assert — the cleared composer waits behind the popup and no model
    // change is persisted.
    assert!(matches!(mode, AppMode::List));
    let [
        PromptEffect::OpenChatGptSignIn {
            restore,
            selected_agent,
        },
    ] = effects.as_slice()
    else {
        unreachable!("expected only the sign-in popup request");
    };
    assert_eq!(
        *selected_agent,
        AgentSelection::new(AgentKind::Harness, AgentModel::ChatGptGpt6Sol)
    );
    assert!(restore.input.is_empty());
    assert_eq!(restore.slash_state.stage, PromptSlashStage::Command);
}

#[tokio::test]
async fn test_prompt_submit_on_signed_out_chatgpt_model_opens_sign_in_and_keeps_draft() {
    // Arrange
    let chatgpt = AgentSelection::new(AgentKind::Harness, AgentModel::ChatGptGpt6Sol);
    let (mut app, _base_dir) = new_test_prompt_app("explain the bug", None).await;
    app.services.replace_harness_model_options(vec![
        crate::test_support::sign_in_required_harness_model(AgentModel::ChatGptGpt6Sol),
    ]);
    app.sessions.sessions_mut()[0].agent = chatgpt;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    run_prompt_submit(&mut app, &prompt_context).await;

    // Assert
    let AppMode::ChatGptSignIn { dialog } = &app.mode else {
        unreachable!("expected the sign-in popup");
    };
    assert_eq!(dialog.selection, chatgpt);
    assert_eq!(dialog.restore.input.text(), "explain the bug");
}

/// Verifies `Enter` on `/model` in a CLI session opens the agent list
/// without refreshing harness models or editing the command text.
#[test]
fn test_model_command_submit_opens_the_agent_list() {
    // Arrange
    let mut mode = new_test_prompt_mode("/model");

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Enter);

    // Assert
    let AppMode::Prompt {
        input, slash_state, ..
    } = &mode
    else {
        unreachable!("expected AppMode::Prompt");
    };
    assert_eq!(input.text(), "/model");
    assert_eq!(slash_state.stage, PromptSlashStage::Agent);
    assert_eq!(slash_state.selected_agent, None);
    assert_eq!(slash_state.selected_index, 0);
    assert!(effects.is_empty());
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
    run_prompt_slash_submit(&mut app, &prompt_context).await;
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

#[test]
fn test_vertical_slash_selection_wraps_around_the_agent_list() {
    let last_index = AgentKind::ALL.len().saturating_sub(1);

    for (code, initial_index, expected_index) in
        [(KeyCode::Down, last_index, 0), (KeyCode::Up, 0, last_index)]
    {
        // Arrange
        let mut mode = new_test_prompt_mode("/model");
        if let AppMode::Prompt { slash_state, .. } = &mut mode {
            slash_state.stage = PromptSlashStage::Agent;
            slash_state.selected_index = initial_index;
        }

        // Act
        reduce_prompt_code(&mut mode, code);

        // Assert
        assert!(matches!(
            &mode,
            AppMode::Prompt { slash_state, .. }
                if slash_state.stage == PromptSlashStage::Agent
                    && slash_state.selected_index == expected_index
        ));
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

#[test]
fn test_prompt_input_command_ignores_non_prompt_mode() {
    // Arrange
    let mut mode = AppMode::List;

    // Act
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::Insert('x')),
        [] as [PromptAttachment; 0]
    );

    // Assert
    assert!(matches!(mode, AppMode::List));
}

#[test]
fn test_backtab_cycles_permission_modes_and_preserves_input() {
    for (current_permission_mode, expected_permission_mode) in [
        (
            PermissionMode::AutoEdit,
            PermissionMode::AutoEditAddressComments,
        ),
        (
            PermissionMode::AutoEditAddressComments,
            PermissionMode::ReadOnly,
        ),
        (PermissionMode::ReadOnly, PermissionMode::AutoEdit),
    ] {
        // Arrange
        let mut mode = new_test_prompt_mode("draft text");
        let mut prompt_context = new_test_prompt_context();
        prompt_context.permission_mode = current_permission_mode;

        // Act
        let effects = expect_prompt_effects(reduce_test_prompt_key(
            &mut mode,
            &mut prompt_context,
            KeyEvent::new(KeyCode::BackTab, event::KeyModifiers::NONE),
        ));

        // Assert
        assert_eq!(mode_input(&mode).0, "draft text");
        assert!(matches!(
            effects.as_slice(),
            [PromptEffect::PersistPermissionMode(permission_mode)]
                if *permission_mode == expected_permission_mode
        ));
    }
}

#[test]
fn test_navigate_prompt_history_up_stays_on_first_entry() {
    // Arrange
    let mut mode = new_test_prompt_mode("draft");
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &mut mode
    {
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(0);
        *input = InputState::with_text("first".to_string());
    }

    // Act
    navigate_prompt_history_up(&mut mode);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { history_state, input, .. }
            if input.text() == "first"
                && history_state.selected_index == Some(0)
                && history_state.draft_text.is_none()
    ));
}

#[test]
fn test_navigate_prompt_history_down_selects_next_entry() {
    // Arrange
    let mut mode = new_test_prompt_mode("first");
    if let AppMode::Prompt { history_state, .. } = &mut mode {
        history_state.entries = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        history_state.selected_index = Some(0);
    }

    // Act
    navigate_prompt_history_down(&mut mode);

    // Assert
    assert!(matches!(
        &mode,
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
    assert_eq!(app.sessions.sessions()[0].prompt, "/model");
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
    run_prompt_slash_submit(&mut app, &prompt_context).await;
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
    run_prompt_submit(&mut app, &prompt_context).await;
    press_prompt_key(&mut app, KeyCode::Down).await;
    run_prompt_submit(&mut app, &prompt_context).await;

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

#[test]
fn test_apply_prompt_apply_outcome_keeps_composer_for_retry() {
    // Arrange
    let mut mode = new_test_prompt_mode("/apply");
    if let AppMode::Prompt { slash_state, .. } = &mut mode {
        slash_state.stage = PromptSlashStage::Model;
        slash_state.selected_agent = Some(AgentKind::Codex);
        slash_state.selected_index = 2;
    }

    // Act
    let attachments = apply_prompt_apply_outcome(&mut mode, PromptApplyOutcome::KeepComposer);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt {
            input, slash_state, ..
        } if input.text() == "/apply" && *slash_state == PromptSlashState::default()
    ));
    assert_eq!(attachments, [] as [PromptAttachment; 0]);
}

#[tokio::test]
async fn test_open_diff_effect_restores_composer_when_session_is_missing() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft text", None).await;
    let mut prompt_context = prompt_context(&mut app).expect("expected prompt context");
    prompt_context.session_id = "missing-session".into();
    let restore = take_prompt_snapshot(&mut app.mode).map(DiffRestoreTarget::Prompt);

    // Act
    apply_prompt_effects(
        &mut app,
        &prompt_context,
        vec![PromptEffect::OpenDiff { restore }],
    )
    .await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draft text"
    ));
}

#[test]
fn test_model_slash_submit_discards_pasted_image_before_normal_submission() {
    // Arrange
    let image_path = PathBuf::from("/tmp/nonexistent-test-attachment.png");
    let mut mode = new_test_prompt_mode("/model");
    mode.insert_pasted_image_placeholder(image_path.clone());
    if let AppMode::Prompt { slash_state, .. } = &mut mode {
        slash_state.stage = PromptSlashStage::Model;
        slash_state.selected_agent = Some(AgentKind::Claude);
        slash_state.selected_index = 0;
    }
    let mut effects = Vec::new();

    // Act
    reduce_prompt_slash_submit(&mut mode, &new_test_prompt_context(), &mut effects);
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.reset_text("normal prompt".to_string());
    }
    let prompt = mode.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, "normal prompt");
    assert_eq!(
        prompt.attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
    assert!(matches!(
        effects.as_slice(),
        [
            PromptEffect::CleanupAttachments(attachments),
            PromptEffect::UpdateModel(selected_agent),
        ] if attachments.len() == 1
            && attachments[0].local_image_path == image_path
            && selected_agent.kind() == AgentKind::Claude
    ));
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
async fn test_ctrl_v_pastes_image_through_injected_clipboard_image_client() {
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
    let mut terminal = test_terminal();

    // Act
    handle_with_cache(
        &mut app,
        &RenderCacheStore::default(),
        &mut terminal,
        KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::CONTROL),
    )
    .await
    .expect("prompt key handling failed");
    let was_pasting_image = matches!(
        &app.mode,
        AppMode::Prompt { attachment_state, .. } if attachment_state.is_pasting_image()
    );
    let has_tick_driven_ui = app.has_visible_tick_driven_ui();
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    assert!(was_pasting_image);
    assert!(has_tick_driven_ui);
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &app.mode
    {
        assert!(!attachment_state.is_pasting_image());
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
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    apply_next_prompt_image_paste(&mut app).await;

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
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    apply_next_prompt_image_paste(&mut app).await;

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

/// Verifies repeated paste shortcuts do not request a second clipboard
/// capture while the first one is still running.
#[test]
fn test_handle_prompt_image_paste_ignores_repeat_while_capture_runs() {
    // Arrange
    let mut mode = new_test_prompt_mode("Review ");
    let paste_key = KeyEvent::new(KeyCode::Char('v'), event::KeyModifiers::CONTROL);

    // Act
    let first_effects = reduce_prompt_event(&mut mode, paste_key);
    mark_prompt_image_paste_pending(&mut mode, 1);
    let repeated_effects = reduce_prompt_event(&mut mode, paste_key);

    // Assert
    assert!(matches!(
        first_effects.as_slice(),
        [PromptEffect::StartImagePaste {
            attachment_number: 1
        }]
    ));
    assert!(repeated_effects.is_empty());
}

/// Verifies a capture completing after the user kept editing inserts its
/// placeholder where the paste shortcut was pressed and keeps the cursor on
/// the text it was on.
#[tokio::test]
async fn test_prompt_image_paste_completion_lands_at_paste_position() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    install_pasted_image_clipboard(&mut app);
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    press_prompt_key(&mut app, KeyCode::Char('!')).await;
    press_prompt_key(&mut app, KeyCode::Home).await;
    press_prompt_key(&mut app, KeyCode::Char('>')).await;

    // Act
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { attachment_state, input, .. }
            if input.text() == ">Review [Image #1]!"
                && input.cursor == 1
                && attachment_state.attachments.len() == 1
    ));
}

/// Verifies typing right after a pasted image placeholder separates the new
/// word from the placeholder with one automatic space.
#[test]
fn test_prompt_typing_after_image_placeholder_inserts_space() {
    // Arrange
    let mut mode = new_test_prompt_mode("Review ");
    mode.insert_pasted_image_placeholder(PathBuf::from("/tmp/pasted.png"));

    // Act
    reduce_prompt_code(&mut mode, KeyCode::Char('w'));
    reduce_prompt_code(&mut mode, KeyCode::Char('h'));

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { attachment_state, input, .. }
            if input.text() == "Review [Image #1] wh"
                && input.cursor == 20
                && attachment_state.attachments.len() == 1
    ));
}

/// Verifies a capture completing after the user typed at the paste position
/// separates the placeholder from that text with one space.
#[tokio::test]
async fn test_prompt_image_paste_completion_separates_text_typed_meanwhile() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    install_pasted_image_clipboard(&mut app);
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    press_prompt_key(&mut app, KeyCode::Char('w')).await;
    press_prompt_key(&mut app, KeyCode::Char('h')).await;

    // Act
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { attachment_state, input, .. }
            if input.text() == "Review [Image #1] wh"
                && input.cursor == 20
                && attachment_state.attachments.len() == 1
    ));
}

/// Builds a prompt whose clipboard-image capture is still running.
fn pasting_image_mode(input_text: &str) -> AppMode {
    let mut mode = new_test_prompt_mode(input_text);
    mark_prompt_image_paste_pending(&mut mode, 1);

    mode
}

/// Verifies `Enter` is ignored while a capture runs, so the pasted image is
/// not dropped from the submitted turn.
#[test]
fn test_prompt_submit_is_ignored_while_image_paste_runs() {
    // Arrange
    let mut mode = pasting_image_mode("Review ");

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Enter);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { attachment_state, input, .. }
            if attachment_state.is_pasting_image() && input.text() == "Review "
    ));
    assert!(effects.is_empty());
}

/// Verifies prompt-history navigation pauses while a capture runs, so the
/// image lands in the draft instead of replacing it inside a history entry.
#[test]
fn test_prompt_history_navigation_is_ignored_while_image_paste_runs() {
    // Arrange
    let mut mode = pasting_image_mode("Review ");
    if let AppMode::Prompt { history_state, .. } = &mut mode {
        history_state.entries = vec!["earlier prompt".to_string()];
    }

    // Act
    reduce_prompt_code(&mut mode, KeyCode::Up);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { history_state, input, .. }
            if input.text() == "Review " && history_state.selected_index.is_none()
    ));
}

#[test]
fn test_navigate_prompt_history_down_is_ignored_while_image_paste_runs() {
    // Arrange
    let mut mode = new_test_prompt_mode("first");
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        ..
    } = &mut mode
    {
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(0);
        attachment_state.pending_image_paste = Some(PendingImagePaste {
            anchor: Some(0),
            request_id: 1,
        });
    }

    // Act
    navigate_prompt_history_down(&mut mode);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { history_state, input, .. }
            if input.text() == "first" && history_state.selected_index == Some(0)
    ));
}

/// Installs a clipboard client whose single capture returns one image path.
fn install_pasted_image_clipboard(app: &mut crate::app::App) {
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Ok(crate::infra::clipboard_image::PersistedClipboardImage {
                    local_image_path: PathBuf::from("/tmp/pasted.png"),
                })
            })
        });
    install_mock_clipboard_image_client(app, clipboard_image_client);
}

/// Verifies graceful shutdown waits for a running capture and removes its
/// image when the completion was never applied to the composer.
#[tokio::test]
async fn test_shutdown_removes_unapplied_prompt_image_paste() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let image_path = crate::infra::home::agentty_home()
        .join("tmp")
        .join(prompt_context.session_id.as_str())
        .join("images")
        .join("image-001.png");
    let mock_image_path = image_path.clone();
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .returning(move |_, _| {
            let local_image_path = mock_image_path.clone();

            Box::pin(async move {
                tokio::task::yield_now().await;

                Ok(crate::infra::clipboard_image::PersistedClipboardImage { local_image_path })
            })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);
    let expected_image_directory = image_path
        .parent()
        .expect("managed image path should have a parent")
        .to_path_buf();
    let mut mock_fs_client = crate::infra::fs::MockFsClient::new();
    mock_fs_client
        .expect_remove_file()
        .once()
        .withf(move |path| path == &image_path)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_remove_dir()
        .once()
        .withf(move |path| path == &expected_image_directory)
        .returning(|_| Box::pin(async { Ok(()) }));
    install_mock_fs_client(&mut app, mock_fs_client);
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);

    // Act
    app.wait_for_background_cleanup_tasks().await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { attachment_state, input, .. }
            if attachment_state.is_pasting_image() && input.text() == "Review "
    ));
}

/// Verifies a capture finishing after its composer was reset removes the
/// orphaned image instead of inserting a placeholder.
#[tokio::test]
async fn test_prompt_image_paste_completion_after_reset_removes_orphaned_image() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let image_path = crate::infra::home::agentty_home()
        .join("tmp")
        .join(prompt_context.session_id.as_str())
        .join("images")
        .join("image-001.png");
    let mock_image_path = image_path.clone();
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .once()
        .returning(move |_, _| {
            let local_image_path = mock_image_path.clone();

            Box::pin(async move {
                Ok(crate::infra::clipboard_image::PersistedClipboardImage { local_image_path })
            })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);
    let expected_image_directory = image_path
        .parent()
        .expect("managed image path should have a parent")
        .to_path_buf();
    let mut mock_fs_client = crate::infra::fs::MockFsClient::new();
    mock_fs_client
        .expect_remove_file()
        .once()
        .withf(move |path| path == &image_path)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_remove_dir()
        .once()
        .withf(move |path| path == &expected_image_directory)
        .returning(|_| Box::pin(async { Ok(()) }));
    install_mock_fs_client(&mut app, mock_fs_client);
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    if let AppMode::Prompt {
        attachment_state, ..
    } = &mut app.mode
    {
        attachment_state.reset();
    }

    // Act
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "Review ");
        assert_eq!(attachment_state.attachments, [] as [PromptAttachment; 0]);
    }
}

/// Verifies a capture failing after the user left the composer still
/// reports the failure in the session transcript.
#[tokio::test]
async fn test_prompt_image_paste_failure_after_leaving_prompt_reports_error() {
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
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    app.mode = AppMode::List;

    // Act
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    app.sessions.sync_from_handles();
    assert!(matches!(app.mode, AppMode::List));
    assert!(
        session_replay_text(&app.sessions.sessions()[0])
            .contains("[Paste Image Error] Clipboard does not contain an image.")
    );
}

/// Queues an unrelated event ahead of a paste completion to reproduce the
/// ordering that previously let the wait helper drain its target unseen.
#[tokio::test]
async fn test_prompt_image_paste_completion_behind_unrelated_event_reports_error() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let session_id = app.sessions.sessions()[0].id.clone();
    app.mode = AppMode::List;
    let event_sender = app.services.event_sender();
    event_sender
        .send(AppEvent::VersionAvailabilityUpdated {
            latest_available_version: None,
        })
        .expect("app event channel should remain open");
    event_sender
        .send(AppEvent::PromptImagePasted {
            update: PromptImagePasteUpdate {
                request_id: 1,
                result: Err("Clipboard does not contain an image.".to_string()),
                session_id,
            },
        })
        .expect("app event channel should remain open");

    // Act
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    app.sessions.sync_from_handles();
    assert!(matches!(app.mode, AppMode::List));
    assert!(
        session_replay_text(&app.sessions.sessions()[0])
            .contains("[Paste Image Error] Clipboard does not contain an image.")
    );
}

/// Verifies snapshotting the composer for diff preview abandons a running
/// capture, so the restored composer is idle and accepts a new paste.
#[tokio::test]
async fn test_prompt_snapshot_abandons_running_image_paste() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .times(2)
        .returning(|_, _| {
            Box::pin(async {
                Ok(crate::infra::clipboard_image::PersistedClipboardImage {
                    local_image_path: PathBuf::from("/tmp/pasted.png"),
                })
            })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);

    // Act
    let snapshot = take_prompt_snapshot(&mut app.mode).expect("expected AppMode::Prompt");
    apply_next_prompt_image_paste(&mut app).await;
    app.mode = snapshot.into_prompt_mode();
    let has_tick_driven_ui = app.has_visible_tick_driven_ui();
    let restored_text = match &app.mode {
        AppMode::Prompt {
            attachment_state,
            input,
            ..
        } if !attachment_state.is_pasting_image() => Some(input.text().to_string()),
        _ => None,
    };
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    assert!(!has_tick_driven_ui);
    assert_eq!(restored_text.as_deref(), Some("Review "));
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { attachment_state, input, .. }
            if input.text() == "Review [Image #1]" && attachment_state.attachments.len() == 1
    ));
}

/// Verifies leaving for the sessions list while a capture runs restores an
/// idle composer that accepts a new paste.
#[tokio::test]
async fn test_saved_prompt_progress_abandons_running_image_paste() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    let mut clipboard_image_client = crate::infra::clipboard_image::MockClipboardImageClient::new();
    clipboard_image_client
        .expect_persist_clipboard_image()
        .times(2)
        .returning(|_, _| {
            Box::pin(async {
                Ok(crate::infra::clipboard_image::PersistedClipboardImage {
                    local_image_path: PathBuf::from("/tmp/pasted.png"),
                })
            })
        });
    install_mock_clipboard_image_client(&mut app, clipboard_image_client);
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    press_prompt_key(&mut app, KeyCode::Tab).await;
    press_prompt_key(&mut app, KeyCode::Char('q')).await;

    // Act
    apply_next_prompt_image_paste(&mut app).await;
    let is_restored = app
        .restore_prompt_progress(prompt_context.session_id.as_str())
        .await;
    let is_pasting_image = matches!(
        &app.mode,
        AppMode::Prompt { attachment_state, .. } if attachment_state.is_pasting_image()
    );
    paste_image_into_active_prompt(&mut app, &prompt_context.session_id);
    apply_next_prompt_image_paste(&mut app).await;

    // Assert
    assert!(is_restored);
    assert!(!is_pasting_image);
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { attachment_state, input, .. }
            if input.text() == "Review [Image #1]" && attachment_state.attachments.len() == 1
    ));
}

#[test]
fn test_handle_prompt_slash_submit_ignores_non_prompt_mode() {
    // Arrange
    let mut mode = AppMode::List;
    let mut effects = Vec::new();

    // Act
    reduce_prompt_slash_submit(&mut mode, &new_test_prompt_context(), &mut effects);

    // Assert
    assert!(matches!(mode, AppMode::List));
    assert!(effects.is_empty());
}

#[test]
fn test_handle_prompt_slash_submit_maps_filtered_first_command_to_mode() {
    // Arrange
    let mut mode = new_test_prompt_mode("/");
    let mut effects = Vec::new();

    // Act
    reduce_prompt_slash_submit(&mut mode, &new_test_prompt_context(), &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { slash_state, .. }
            if slash_state.stage == PromptSlashStage::Mode
                && slash_state.selected_agent.is_none()
                && slash_state.selected_index == 0
    ));
    assert!(effects.is_empty());
}

#[test]
fn test_handle_prompt_slash_submit_selects_agent_and_advances_to_model_stage() {
    // Arrange
    let mut mode = new_test_prompt_mode("/model");
    let selected_index = AgentKind::ALL
        .iter()
        .position(|agent_kind| *agent_kind == AgentKind::Claude)
        .expect("expected Claude agent");
    if let AppMode::Prompt { slash_state, .. } = &mut mode {
        slash_state.stage = PromptSlashStage::Agent;
        slash_state.selected_index = selected_index;
    }
    let mut effects = Vec::new();

    // Act
    reduce_prompt_slash_submit(&mut mode, &new_test_prompt_context(), &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { slash_state, .. }
            if slash_state.stage == PromptSlashStage::Model
                && slash_state.selected_agent == Some(AgentKind::Claude)
                && slash_state.selected_index == 0
    ));
    assert!(effects.is_empty());
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
    run_prompt_slash_submit(&mut app, &prompt_context).await;
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

/// Verifies `/reasoning` and `/style` preselect the session's current value,
/// and that a stale command highlight is clamped before resolving.
#[test]
fn test_handle_prompt_slash_submit_prefills_stage_selection_from_session_values() {
    for (
        input_text,
        stale_index,
        reasoning_level,
        response_style,
        expected_stage,
        expected_index,
    ) in [
        (
            "/reasoning",
            0,
            ReasoningLevel::Low,
            ResponseStyle::default(),
            PromptSlashStage::Reasoning,
            0,
        ),
        (
            "/reasoning",
            0,
            ReasoningLevel::High,
            ResponseStyle::default(),
            PromptSlashStage::Reasoning,
            2,
        ),
        (
            "/reasoning",
            99,
            ReasoningLevel::default(),
            ResponseStyle::default(),
            PromptSlashStage::Reasoning,
            2,
        ),
        (
            "/style",
            0,
            ReasoningLevel::default(),
            ResponseStyle::Detailed,
            PromptSlashStage::Style,
            2,
        ),
    ] {
        // Arrange
        let mut mode = new_test_prompt_mode(input_text);
        if let AppMode::Prompt { slash_state, .. } = &mut mode {
            slash_state.selected_index = stale_index;
        }
        let mut prompt_context = new_test_prompt_context();
        prompt_context.reasoning_level = reasoning_level;
        prompt_context.response_style = response_style;
        let mut effects = Vec::new();

        // Act
        reduce_prompt_slash_submit(&mut mode, &prompt_context, &mut effects);

        // Assert
        assert!(
            matches!(
                &mode,
                AppMode::Prompt { slash_state, .. }
                    if slash_state.stage == expected_stage
                        && slash_state.selected_agent.is_none()
                        && slash_state.selected_index == expected_index
            ),
            "{input_text} with {reasoning_level:?} and {response_style:?}"
        );
        assert!(effects.is_empty());
    }
}

/// Verifies slash submit ignores unmatched commands and preserves the
/// prompt state.
#[test]
fn test_handle_prompt_slash_submit_ignores_unknown_command() {
    // Arrange
    let mut mode = new_test_prompt_mode("/x");
    let mut effects = Vec::new();

    // Act
    reduce_prompt_slash_submit(&mut mode, &new_test_prompt_context(), &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { input, slash_state, .. }
            if input.text() == "/x" && *slash_state == PromptSlashState::default()
    ));
    assert!(effects.is_empty());
}

#[test]
fn test_handle_prompt_left_with_alt_moves_cursor_to_previous_word_start() {
    // Arrange
    let mut mode = new_test_prompt_mode("hello brave world");
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.cursor = "hello brave world".chars().count();
    }

    // Act
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::MoveWordLeft),
        [] as [PromptAttachment; 0]
    );

    // Assert
    if let AppMode::Prompt { input, .. } = &mode {
        assert_eq!(input.cursor, "hello brave ".chars().count());
    }
}

#[test]
fn test_handle_prompt_right_with_alt_moves_cursor_to_next_word_start() {
    // Arrange
    let mut mode = new_test_prompt_mode("hello brave world");
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.cursor = 0;
    }

    // Act
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::MoveWordRight),
        [] as [PromptAttachment; 0]
    );

    // Assert
    if let AppMode::Prompt { input, .. } = &mode {
        assert_eq!(input.cursor, "hello ".chars().count());
    }
}

#[test]
fn test_handle_prompt_left_with_super_moves_cursor_to_line_start() {
    // Arrange
    let mut mode = new_test_prompt_mode("first\nsecond\nthird");
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.cursor = "first\nseco".chars().count();
    }

    // Act
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::MoveLineStart),
        [] as [PromptAttachment; 0]
    );

    // Assert
    if let AppMode::Prompt { input, .. } = &mode {
        assert_eq!(input.cursor, "first\n".chars().count());
    }
}

#[test]
fn test_handle_prompt_right_with_super_moves_cursor_to_line_end() {
    // Arrange
    let mut mode = new_test_prompt_mode("first\nsecond\nthird");
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.cursor = "first\nse".chars().count();
    }

    // Act
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::MoveLineEnd),
        [] as [PromptAttachment; 0]
    );

    // Assert
    if let AppMode::Prompt { input, .. } = &mode {
        assert_eq!(input.cursor, "first\nsecond".chars().count());
    }
}

#[test]
fn test_handle_prompt_backspace_resets_history_navigation() {
    // Arrange
    let mut mode = new_test_prompt_mode("second");
    if let AppMode::Prompt { history_state, .. } = &mut mode {
        history_state.draft_text = Some("draft".to_string());
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(1);
    }

    // Act
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::DeleteBackward),
        [] as [PromptAttachment; 0]
    );

    // Assert
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &mode
    {
        assert_eq!(input.text(), "secon");
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[test]
fn test_handle_prompt_backspace_on_empty_input_is_noop() {
    // Arrange
    let mut mode = new_test_prompt_mode("");

    // Act
    assert_eq!(
        apply_prompt_input_to_mode(&mut mode, InputCommand::DeleteBackward),
        [] as [PromptAttachment; 0]
    );

    // Assert
    if let AppMode::Prompt { input, .. } = &mode {
        assert!(input.is_empty());
        assert_eq!(input.cursor, 0);
    }
}

#[test]
fn test_handle_prompt_left_reactivates_existing_at_mention_without_cached_state() {
    // Arrange
    let input_text = "@src/main.rs more";
    let mut mode = new_test_prompt_mode(input_text);
    let prompt_context = new_test_prompt_context();
    let mut effects = Vec::new();
    let moves_back_into_mention = " more".chars().count();

    // Act
    for _ in 0..moves_back_into_mention {
        reduce_prompt_input_command(
            &mut mode,
            &prompt_context,
            InputCommand::MoveLeft,
            &mut effects,
        );
    }

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { at_mention_state, input, .. }
            if input.cursor == "@src/main.rs".chars().count() && at_mention_state.is_some()
    ));
    assert!(matches!(
        effects.as_slice(),
        [PromptEffect::LoadAtMentionEntries]
    ));
}

#[tokio::test]
async fn test_handle_prompt_submit_key_ignores_empty_prompt() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    run_prompt_submit(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::Prompt { .. }));
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(app.sessions.sessions()[0].prompt, "");
}

#[test]
fn graph_subcommands_do_not_bypass_the_mode_selector() {
    // Arrange
    let mut mode = new_test_prompt_mode("/graph query");

    // Act
    let effects = reduce_prompt_code(&mut mode, KeyCode::Enter);

    // Assert
    assert!(matches!(mode, AppMode::Prompt { .. }));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, PromptEffect::SubmitPrompt(_)))
    );
}

#[tokio::test]
async fn graph_slash_command_arms_and_escape_preserves_the_draft() {
    // Arrange
    let (mut app, _root) = new_test_prompt_app("/graph", None).await;
    app.experimental_graph = true;
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.graph = PromptGraphState::Available;
    }
    let context = prompt_context(&mut app).expect("context");

    // Act
    run_prompt_submit(&mut app, &context).await;

    // Assert
    assert!(
        matches!(&app.mode, AppMode::Prompt { input, slash_state, .. }
        if input.text().is_empty() && slash_state.graph.is_armed())
    );

    // Arrange
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        *input = InputState::with_text("/mode is part of my task".into());
    }
    let context = prompt_context(&mut app).expect("context");
    assert!(!context.is_slash_command());

    // Act
    app.experimental_graph = false;
    run_prompt_submit(&mut app, &context).await;

    // Assert: rejected graph requests preserve the armed draft.
    assert!(
        matches!(&app.mode, AppMode::Prompt { input, slash_state, .. }
        if input.text() == "/mode is part of my task" && slash_state.graph.is_armed())
    );

    // Act
    run_prompt_cancel(&mut app, &context).await;

    // Assert
    assert!(
        matches!(&app.mode, AppMode::Prompt { input, slash_state, .. }
        if input.text() == "/mode is part of my task" && !slash_state.graph.is_armed())
    );
}

#[tokio::test]
async fn test_handle_prompt_submit_key_drains_supported_image_turn() {
    // Arrange
    let (mut app, _base_dir) = new_test_draft_prompt_app("Review ", None).await;
    app.sessions.sessions_mut()[0].agent =
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet55);
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    run_prompt_submit(&mut app, &prompt_context).await;

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
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet55);
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    run_prompt_submit(&mut app, &prompt_context).await;

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
    run_prompt_slash_submit(&mut app, &prompt_context).await;

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
            request_id: uuid::Uuid::nil(),
            diff_hash: 0,
            text: "## Review\n### Suggestions\n- None".to_string(),
        },
    );
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    run_prompt_slash_submit(&mut app, &prompt_context).await;

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

#[test]
fn test_take_submitted_turn_prompt_drains_text_and_attachment_state() {
    // Arrange
    let mut mode = new_test_prompt_mode("Review ");
    mode.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));

    // Act
    let prompt = mode.take_submitted_turn_prompt();

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
    } = &mode
    {
        assert_eq!(input.text(), "");
        assert_eq!(
            attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(attachment_state.next_attachment_number, 1);
    }
}

#[test]
fn test_take_submitted_turn_prompt_sorts_attachments_by_text_position() {
    // Arrange
    let mut mode = new_test_prompt_mode("");
    mode.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.cursor = 0;
    }
    mode.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-2.png"));
    assert_eq!(
        apply_prompt_paste_to_mode(&mut mode, " then "),
        Some(Vec::new())
    );

    // Act
    let prompt = mode.take_submitted_turn_prompt();

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
    paste_image_into_active_prompt(&mut app, &session_id);
    let inserted_attachments =
        app.insert_prompt_image_placeholder(PathBuf::from("/tmp/image-1.png"), None);
    let (prompt, archived_attachments) = take_submitted_turn_prompt(&mut app.mode);
    let cleanup_attachments = take_prompt_attachment_cleanup(&mut app.mode);

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
