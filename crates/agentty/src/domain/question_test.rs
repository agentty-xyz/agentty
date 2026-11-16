//! Clarification question answer pairing, progress validation, and answer
//! attachment tests.

use std::path::PathBuf;

use ag_session::{QuestionAnswer, QuestionItem};

use super::{QuestionAttachmentState, QuestionProgress, question_answer_message};
use crate::domain::composer::{
    PromptAttachment, apply_attachment_input_command, insert_local_image_at,
};
use crate::domain::input::{InputCommand, InputState};

#[test]
fn question_answer_message_pairs_each_question_with_its_answer() {
    // Arrange
    let answers = [
        QuestionAnswer {
            answer: "main".to_string(),
            question: "Which target?".to_string(),
        },
        QuestionAnswer {
            answer: "yes".to_string(),
            question: "Run tests?".to_string(),
        },
    ];

    // Act
    let message = question_answer_message(&answers);

    // Assert
    assert_eq!(
        message,
        "Clarifications:\n1. Q: Which target?\n   A: main\n2. Q: Run tests?\n   A: yes"
    );
}

#[test]
fn test_applies_to_accepts_matching_progress() {
    // Arrange
    let questions = vec![
        QuestionItem::with_options("Continue?", vec!["Yes".to_string()]),
        QuestionItem::with_options("Scope?", vec!["Small".to_string(), "Large".to_string()]),
    ];
    let progress = QuestionProgress {
        attachment_state: QuestionAttachmentState::default(),
        current_index: 1,
        input: InputState::default(),
        responses: vec!["Yes".to_string()],
        selected_option_index: Some(1),
    };

    // Act & Assert
    assert!(progress.applies_to(&questions));
}

#[test]
fn test_applies_to_rejects_response_count_mismatch() {
    // Arrange — one response recorded but the index says none answered.
    let questions = vec![QuestionItem::new("First?"), QuestionItem::new("Second?")];
    let progress = QuestionProgress {
        attachment_state: QuestionAttachmentState::default(),
        current_index: 0,
        input: InputState::default(),
        responses: vec!["Yes".to_string()],
        selected_option_index: None,
    };

    // Act & Assert
    assert!(!progress.applies_to(&questions));
}

#[test]
fn test_applies_to_rejects_index_past_question_list() {
    // Arrange — the saved index points past a shrunken question list.
    let questions = vec![QuestionItem::new("Only question?")];
    let progress = QuestionProgress {
        attachment_state: QuestionAttachmentState::default(),
        current_index: 1,
        input: InputState::default(),
        responses: vec!["Yes".to_string()],
        selected_option_index: None,
    };

    // Act & Assert
    assert!(!progress.applies_to(&questions));
}

#[test]
fn test_applies_to_rejects_out_of_range_option_index() {
    // Arrange — the highlighted option no longer exists on the current
    // question.
    let questions = vec![QuestionItem::with_options(
        "Continue?",
        vec!["Yes".to_string()],
    )];
    let progress = QuestionProgress {
        attachment_state: QuestionAttachmentState::default(),
        current_index: 0,
        input: InputState::default(),
        responses: Vec::new(),
        selected_option_index: Some(1),
    };

    // Act & Assert
    assert!(!progress.applies_to(&questions));
}

/// Pastes one image into `input` through the attachment-aware composer.
fn paste_answer_image(
    attachment_state: &mut QuestionAttachmentState,
    input: &mut InputState,
    file_name: &str,
) {
    let anchor = input.cursor;
    insert_local_image_at(
        &mut attachment_state.composer,
        input,
        PathBuf::from(format!("/tmp/{file_name}")),
        anchor,
    );
}

/// Returns the file names owned by `attachments`, in order.
fn attachment_file_names(attachments: &[PromptAttachment]) -> Vec<String> {
    attachments
        .iter()
        .map(|attachment| {
            attachment
                .local_image_path
                .file_name()
                .map_or_default(|name| name.to_string_lossy().into_owned())
        })
        .collect()
}

#[test]
fn question_attachments_number_images_across_answers() {
    // Arrange
    let mut attachment_state = QuestionAttachmentState::default();
    let mut input = InputState::with_text("See ".to_string());
    input.cursor = 4;
    paste_answer_image(&mut attachment_state, &mut input, "first.png");
    let edit_start = input.cursor;
    let removed = apply_attachment_input_command(
        &mut attachment_state.composer,
        &mut input,
        InputCommand::DeleteBackward,
    );

    // Act
    paste_answer_image(&mut attachment_state, &mut input, "second.png");
    let (first_answer, released) = attachment_state.take_answer(&mut input);
    paste_answer_image(&mut attachment_state, &mut input, "third.png");
    let (second_answer, _) = attachment_state.take_answer(&mut input);

    // Assert
    assert_eq!(edit_start, 14);
    assert_eq!(removed, Some(Vec::new()));
    assert_eq!(first_answer, "See [Image #2]");
    assert_eq!(attachment_file_names(&released), vec!["first.png"]);
    assert_eq!(second_answer, "[Image #3]");
    assert_eq!(
        attachment_file_names(&attachment_state.answered),
        vec!["second.png", "third.png"]
    );
    assert_eq!(attachment_state.composer.next_attachment_number, 4);
}

#[test]
fn question_attachments_discard_and_reopen_the_current_answer() {
    // Arrange
    let mut attachment_state = QuestionAttachmentState::default();
    let mut input = InputState::default();
    paste_answer_image(&mut attachment_state, &mut input, "earlier.png");
    attachment_state.take_answer(&mut input);
    paste_answer_image(&mut attachment_state, &mut input, "unused.png");

    // Act
    let discarded = attachment_state.discard_answer();
    let next_after_discard = attachment_state.composer.next_attachment_number;
    input = InputState::default();
    paste_answer_image(&mut attachment_state, &mut input, "final.png");
    let (final_answer, _) = attachment_state.take_answer(&mut input);
    let restored_input = InputState::with_text(final_answer);
    attachment_state.reopen_answer(&restored_input);
    let owned = attachment_state.clone().into_attachments();

    // Assert
    assert_eq!(attachment_file_names(&discarded), vec!["unused.png"]);
    assert_eq!(next_after_discard, 2);
    assert_eq!(restored_input.text(), "[Image #2]");
    assert_eq!(
        attachment_file_names(&attachment_state.answered),
        vec!["earlier.png"]
    );
    assert_eq!(
        attachment_file_names(&attachment_state.composer.attachments),
        vec!["final.png"]
    );
    assert_eq!(attachment_state.composer.next_attachment_number, 3);
    assert_eq!(
        attachment_file_names(&owned),
        vec!["earlier.png", "final.png"]
    );
}

#[test]
fn question_attachments_reopen_keeps_earlier_images_whose_placeholder_repeats() {
    // Arrange
    let mut attachment_state = QuestionAttachmentState::default();
    let mut input = InputState::default();
    paste_answer_image(&mut attachment_state, &mut input, "earlier.png");
    let (earlier_answer, _) = attachment_state.take_answer(&mut input);
    input = InputState::with_text(format!("Same as {earlier_answer} and "));
    paste_answer_image(&mut attachment_state, &mut input, "final.png");
    let (final_answer, _) = attachment_state.take_answer(&mut input);
    let mut restored_input = InputState::with_text(final_answer);

    // Act
    attachment_state.reopen_answer(&restored_input);
    let reopened = attachment_file_names(&attachment_state.composer.attachments);
    apply_attachment_input_command(
        &mut attachment_state.composer,
        &mut restored_input,
        InputCommand::DeleteCurrentLine,
    );
    let (resubmitted_answer, released) = attachment_state.take_answer(&mut restored_input);

    // Assert
    assert_eq!(earlier_answer, "[Image #1]");
    assert_eq!(reopened, vec!["final.png"]);
    assert_eq!(resubmitted_answer, "");
    assert_eq!(attachment_file_names(&released), vec!["final.png"]);
    assert_eq!(
        attachment_file_names(&attachment_state.answered),
        vec!["earlier.png"]
    );
}
