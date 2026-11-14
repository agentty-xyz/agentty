use std::path::PathBuf;

use super::super::{PromptEffect, reduce_prompt_input_command, select_at_mention};
use super::support::{PromptTestModeExt, new_test_prompt_mode};
use crate::domain::file_entry::FileEntry;
use crate::domain::input::InputCommand;
use crate::presentation::app_mode::AppMode;
use crate::presentation::prompt::PromptAtMentionState;

#[test]
fn test_prompt_input_edit_and_undo_resync_at_mention_state() {
    // Arrange
    let mut mode = new_test_prompt_mode("");
    let mut effects = Vec::new();

    // Act
    reduce_prompt_input_command(&mut mode, InputCommand::Insert('@'), &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { at_mention_state, .. } if at_mention_state.is_some()
    ));
    assert!(matches!(
        effects.as_slice(),
        [PromptEffect::LoadAtMentionEntries]
    ));

    // Act
    reduce_prompt_input_command(&mut mode, InputCommand::Insert(' '), &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { at_mention_state, .. } if at_mention_state.is_none()
    ));

    // Act
    reduce_prompt_input_command(&mut mode, InputCommand::Undo, &mut effects);

    // Assert
    assert!(matches!(
        &mode,
        AppMode::Prompt { at_mention_state, input, .. }
            if input.text() == "@" && at_mention_state.is_some()
    ));
}

#[test]
fn test_at_mention_completion_keeps_following_image_occurrence_synchronized() {
    // Arrange
    let selected_path = "very/long/path/to/main.rs";
    let at_mention_state = PromptAtMentionState::new(vec![FileEntry {
        is_dir: false,
        path: selected_path.to_string(),
    }]);
    let mut mode = new_test_prompt_mode("@v");
    let mut effects = Vec::new();
    mode.insert_pasted_image_placeholder(PathBuf::from("/tmp/image-1.png"));
    if let AppMode::Prompt {
        at_mention_state: state,
        input,
        ..
    } = &mut mode
    {
        *state = Some(at_mention_state);
        input.cursor = "@v".chars().count();
    }

    // Act
    select_at_mention(&mut mode, &mut effects);
    let completed_mention = format!("@{selected_path} ");
    if let AppMode::Prompt { input, .. } = &mut mode {
        input.cursor = completed_mention.chars().count();
    }
    reduce_prompt_input_command(&mut mode, InputCommand::DeleteForward, &mut effects);
    let prompt = mode.take_submitted_turn_prompt();

    // Assert
    assert_eq!(prompt.text, completed_mention);
    assert_eq!(
        prompt.attachments,
        [] as [ag_protocol::TurnPromptAttachment; 0]
    );
}
