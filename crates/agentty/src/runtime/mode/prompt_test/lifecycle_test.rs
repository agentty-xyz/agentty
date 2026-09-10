use std::path::PathBuf;

use super::super::{
    apply_prompt_input_command, handle_paste, handle_prompt_cancel_key, handle_prompt_submit_key,
    navigate_prompt_history_down, navigate_prompt_history_up, prompt_context,
    take_submitted_turn_prompt,
};
use super::support::{
    PromptTestAppExt, new_test_draft_prompt_app, new_test_prompt_app,
    wait_for_at_mention_entries_event,
};
use crate::app::prompt_intent::PromptSessionMode;
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputCommand;
use crate::domain::session::SessionId;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn test_prompt_undo_restores_deleted_image_attachment() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let image_path = PathBuf::from("/tmp/image-1.png");
    app.insert_pasted_image_placeholder(image_path.clone());

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;
    apply_prompt_input_command(&mut app, InputCommand::Undo).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            attachment_state,
            input,
            ..
        } if input.text() == "Review [Image #1]"
            && attachment_state.attachments.len() == 1
            && attachment_state.attachments[0].local_image_path == image_path
            && attachment_state.archived_attachments.is_empty()
    ));
}

#[tokio::test]
async fn test_navigate_prompt_history_up_selects_latest_entry_and_saves_draft() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.entries = vec!["first".to_string(), "second".to_string()];
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
        assert_eq!(input.text(), "second");
        assert_eq!(history_state.selected_index, Some(1));
        assert_eq!(history_state.draft_text.as_deref(), Some("draft"));
    }
}

#[tokio::test]
async fn test_navigate_prompt_history_down_restores_draft_after_latest_entry() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("draft", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.entries = vec!["first".to_string(), "second".to_string()];
    }
    navigate_prompt_history_up(&mut app);

    // Act
    navigate_prompt_history_down(&mut app);

    // Assert
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "draft");
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_navigate_prompt_history_down_restores_draft_without_attachment_revision() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("earlier", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.draft_text = Some("draft".to_string());
        history_state.entries = vec!["earlier".to_string()];
        history_state.selected_index = Some(0);
    }

    // Act
    navigate_prompt_history_down(&mut app);

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt {
            attachment_state,
            history_state,
            input,
            ..
        } if input.text() == "draft"
            && history_state.selected_index.is_none()
            && attachment_state.attachments.is_empty()
    ));
}

#[tokio::test]
async fn test_prompt_shared_commands_insert_text_and_delete_to_line_end() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("first\nsecond", None).await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "first\nse".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::InsertText("X".to_string())).await;
    apply_prompt_input_command(&mut app, InputCommand::DeleteToLineEnd).await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "first\nseX"
    ));
}

#[tokio::test]
async fn test_prompt_history_round_trip_restores_image_draft_attachment() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let image_path = PathBuf::from("/tmp/image-1.png");
    app.insert_pasted_image_placeholder(image_path.clone());
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.entries = vec!["Earlier prompt".to_string()];
    }

    // Act
    navigate_prompt_history_up(&mut app);
    navigate_prompt_history_down(&mut app);
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, "Review [Image #1]");
    assert_eq!(prompt.attachments.len(), 1);
    assert_eq!(prompt.attachments[0].local_image_path, image_path);
}

#[tokio::test]
async fn test_handle_prompt_backspace_removes_whole_image_token_and_attachment() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &mut app.mode
    {
        history_state.selected_index = Some(0);
        history_state.draft_text = Some("draft".to_string());
        input.cursor = input.text().chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;

    // Assert
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "Review ");
        assert_eq!(
            attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(attachment_state.next_attachment_number, 2);
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_handle_prompt_delete_removes_whole_image_token_and_attachment() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &mut app.mode
    {
        history_state.selected_index = Some(0);
        history_state.draft_text = Some("draft".to_string());
        input.cursor = "Review ".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteForward).await;

    // Assert
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "Review ");
        assert_eq!(
            attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(attachment_state.next_attachment_number, 2);
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_handle_prompt_delete_keeps_new_image_number_unique_for_undo() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-2.png"));
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-3.png"));
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "[Image #1][Image #2]".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteForward).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-4.png"));

    // Assert
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "[Image #1][Image #2][Image #4]");
        assert_eq!(attachment_state.attachments.len(), 3);
        assert_eq!(attachment_state.next_attachment_number, 5);
        assert_eq!(attachment_state.attachments[2].placeholder, "[Image #4]");
        assert_eq!(
            attachment_state.attachments[2].local_image_path,
            PathBuf::from("/tmp/image-4.png")
        );
    }
}

#[tokio::test]
async fn test_handle_prompt_backspace_with_alt_removes_whole_word() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("hello brave world", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.draft_text = Some("draft".to_string());
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(1);
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteWordBackward).await;

    // Assert
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "hello brave");
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_handle_prompt_backspace_with_super_deletes_full_line() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("first line\nsecond line", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.draft_text = Some("draft".to_string());
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(1);
    }
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "first line\nsecond".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteCurrentLine).await;

    // Assert
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "first line");
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_handle_prompt_line_delete_with_ctrl_u_deletes_full_line() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("first line\nsecond line", None).await;
    if let AppMode::Prompt { history_state, .. } = &mut app.mode {
        history_state.draft_text = Some("draft".to_string());
        history_state.entries = vec!["first".to_string(), "second".to_string()];
        history_state.selected_index = Some(1);
    }
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "first line\nsecond".chars().count();
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteCurrentLine).await;

    // Assert
    if let AppMode::Prompt {
        history_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "first line");
        assert_eq!(history_state.selected_index, None);
        assert_eq!(history_state.draft_text, None);
    }
}

#[tokio::test]
async fn test_handle_prompt_char_loads_at_mention_entries_from_project_root_for_draft_session() {
    // Arrange
    let (mut app, base_dir) = new_test_draft_prompt_app("", None).await;
    let expected_path = "draft_lookup_target.txt";
    std::fs::write(base_dir.path().join(expected_path), "draft")
        .expect("failed to write project file");
    assert!(!app.sessions.sessions()[0].folder.exists());

    // Act
    apply_prompt_input_command(&mut app, InputCommand::Insert('@')).await;
    let next_event = wait_for_at_mention_entries_event(&mut app).await;

    // Assert
    match next_event {
        crate::app::AppEvent::AtMentionEntriesLoaded {
            entries,
            session_id,
        } => {
            assert_eq!(session_id, app.sessions.sessions()[0].id.as_str());
            assert!(entries.contains(&FileEntry {
                is_dir: false,
                path: expected_path.to_string(),
            }));
        }
        _ => unreachable!("expected at-mention entries event"),
    }
}

#[tokio::test]
async fn test_handle_prompt_char_loads_parent_worktree_entries_for_stacked_draft() {
    // Arrange
    let (mut app, base_dir) = new_test_draft_prompt_app("", None).await;
    let parent_session_id = SessionId::from("parent-session");
    let parent_folder = base_dir.path().join("parent-worktree");
    let expected_path = "parent_lookup_target.txt";
    std::fs::create_dir_all(&parent_folder).expect("failed to create parent worktree");
    std::fs::write(parent_folder.join(expected_path), "parent")
        .expect("failed to write parent worktree file");
    app.sessions.sessions_mut()[0].parent_session_id = Some(parent_session_id.clone());
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id(parent_session_id)
            .folder(parent_folder)
            .build(),
    );

    // Act
    apply_prompt_input_command(&mut app, InputCommand::Insert('@')).await;
    let next_event = wait_for_at_mention_entries_event(&mut app).await;

    // Assert
    match next_event {
        crate::app::AppEvent::AtMentionEntriesLoaded { entries, .. } => {
            assert!(entries.contains(&FileEntry {
                is_dir: false,
                path: expected_path.to_string(),
            }));
        }
        _ => unreachable!("expected at-mention entries event"),
    }
}

#[tokio::test]
async fn test_handle_prompt_cancel_key_deletes_blank_session() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    assert_ne!(prompt_context.session_mode, PromptSessionMode::Existing);
    assert_eq!(app.sessions.sessions().len(), 1);

    // Act
    handle_prompt_cancel_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert!(app.sessions.sessions().is_empty());
}

#[tokio::test]
async fn test_handle_prompt_cancel_key_keeps_empty_draft_session() {
    // Arrange
    let (mut app, _base_dir) = new_test_draft_prompt_app("", None).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");
    assert_ne!(prompt_context.session_mode, PromptSessionMode::Existing);
    assert_ne!(prompt_context.session_mode, PromptSessionMode::NewDeletable);
    assert_eq!(app.sessions.sessions().len(), 1);

    // Act
    handle_prompt_cancel_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(app.sessions.sessions().len(), 1);
    assert_eq!(
        app.sessions.sessions()[0].status,
        crate::domain::session::Status::Draft
    );
    assert_eq!(app.sessions.sessions()[0].prompt, "");
}

#[tokio::test]
async fn test_handle_prompt_submit_key_cleans_archived_attachment() {
    // Arrange
    let (mut app, _base_dir) = new_test_draft_prompt_app("Review ", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/nonexistent-test-attachment.png"));
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_submit_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(app.sessions.sessions()[0].prompt, "Review ");
    assert_eq!(
        app.sessions.sessions()[0].draft_attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
}

#[tokio::test]
async fn test_handle_prompt_cancel_key_keeps_new_session_with_staged_drafts() {
    // Arrange
    let (mut app, _base_dir) = new_test_draft_prompt_app("Another draft", None).await;
    app.sessions.sessions_mut()[0].prompt = "First draft".to_string();
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_cancel_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert_eq!(app.sessions.sessions().len(), 1);
}

#[tokio::test]
async fn test_handle_prompt_cancel_key_resets_existing_session_draft_attachments() {
    // Arrange
    let (mut app, base_dir) = new_test_prompt_app("Review ", None).await;
    app.sessions.sessions_mut()[0].prompt = "Earlier prompt".to_string();
    app.sessions.sessions_mut()[0].status = crate::domain::session::Status::Review;
    let image_directory = base_dir.path().join("images");
    std::fs::create_dir_all(&image_directory).expect("image directory should exist");
    let image_path = image_directory.join("image-1.png");
    std::fs::write(&image_path, b"png").expect("image file should be written");
    app.insert_pasted_image_placeholder(image_path.clone());
    let prompt_context = prompt_context(&mut app).expect("expected prompt context");

    // Act
    handle_prompt_cancel_key(&mut app, &prompt_context).await;

    // Assert
    assert!(matches!(app.mode, AppMode::View { .. }));
    assert!(image_path.exists());
    assert!(image_directory.exists());
}

#[tokio::test]
async fn test_deleting_original_duplicate_placeholder_does_not_submit_image() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    handle_paste(&mut app, "[Image #1]").await;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = 0;
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteForward).await;
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, "[Image #1]");
    assert_eq!(
        prompt.attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
}

#[tokio::test]
async fn test_take_submitted_turn_prompt_filters_deleted_attachment_placeholders() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-2.png"));
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = "Review ".chars().count();
    }
    apply_prompt_input_command(&mut app, InputCommand::DeleteForward).await;

    // Act
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, "Review [Image #2]");
    assert_eq!(prompt.attachments.len(), 1);
    assert_eq!(prompt.attachments[0].placeholder, "[Image #2]");
    assert_eq!(
        prompt.attachments[0].local_image_path,
        PathBuf::from("/tmp/image-2.png")
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
async fn test_take_submitted_turn_prompt_returns_archived_attachments_for_cleanup() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("Review ", None).await;
    let image_path = PathBuf::from("/tmp/image-1.png");
    app.insert_pasted_image_placeholder(image_path.clone());
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;

    // Act
    let (prompt, archived_attachments) = take_submitted_turn_prompt(&mut app);

    // Assert
    assert_eq!(prompt.text, "Review ");
    assert_eq!(
        prompt.attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
    assert_eq!(archived_attachments.len(), 1);
    assert_eq!(archived_attachments[0].local_image_path, image_path);
    assert_eq!(archived_attachments[0].placeholder, "[Image #1]");
}

#[tokio::test]
async fn test_deleting_duplicate_lookalike_keeps_original_image() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;
    let image_path = PathBuf::from("/tmp/image-1.png");
    app.insert_pasted_image_placeholder(image_path.clone());
    handle_paste(&mut app, "[Image #1]").await;

    // Act
    apply_prompt_input_command(&mut app, InputCommand::DeleteBackward).await;
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, "[Image #1]");
    assert_eq!(prompt.attachments.len(), 1);
    assert_eq!(prompt.attachments[0].local_image_path, image_path);
}
