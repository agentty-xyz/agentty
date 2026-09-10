use std::path::PathBuf;

use super::super::{apply_prompt_input_command, handle_at_mention_select};
use super::support::{PromptTestAppExt, new_test_prompt_app};
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputCommand;
use crate::presentation::app_mode::AppMode;
use crate::presentation::prompt::PromptAtMentionState;

#[tokio::test]
async fn test_prompt_input_edit_and_undo_resync_at_mention_state() {
    // Arrange
    let (mut app, _base_dir) = new_test_prompt_app("", None).await;

    // Act
    apply_prompt_input_command(&mut app, InputCommand::Insert('@')).await;

    // Assert
    assert!(matches!(app.mode, AppMode::Prompt { .. }));
    if let AppMode::Prompt {
        at_mention_state, ..
    } = &app.mode
    {
        assert!(at_mention_state.is_some());
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::Insert(' ')).await;

    // Assert
    assert!(matches!(app.mode, AppMode::Prompt { .. }));
    if let AppMode::Prompt {
        at_mention_state, ..
    } = &app.mode
    {
        assert!(at_mention_state.is_none());
    }

    // Act
    apply_prompt_input_command(&mut app, InputCommand::Undo).await;

    // Assert
    if let AppMode::Prompt {
        at_mention_state,
        input,
        ..
    } = &app.mode
    {
        assert_eq!(input.text(), "@");
        assert!(at_mention_state.is_some());
    }
}

#[tokio::test]
async fn test_at_mention_completion_keeps_following_image_occurrence_synchronized() {
    // Arrange
    let selected_path = "very/long/path/to/main.rs";
    let at_mention_state = PromptAtMentionState::new(vec![FileEntry {
        is_dir: false,
        path: selected_path.to_string(),
    }]);
    let (mut app, _base_dir) = new_test_prompt_app("@v", None).await;
    app.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    if let AppMode::Prompt {
        at_mention_state: state,
        input,
        ..
    } = &mut app.mode
    {
        *state = Some(at_mention_state);
        input.cursor = "@v".chars().count();
    }

    // Act
    handle_at_mention_select(&mut app).await;
    let completed_mention = format!("@{selected_path} ");
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        input.cursor = completed_mention.chars().count();
    }
    apply_prompt_input_command(&mut app, InputCommand::DeleteForward).await;
    let prompt = app.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, completed_mention);
    assert_eq!(
        prompt.attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
}
