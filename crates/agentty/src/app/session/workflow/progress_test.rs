use super::{REASONING_BUFFER_BYTE_LIMIT, REASONING_PREVIEW_CHAR_LIMIT, TurnProgress};

#[test]
fn message_is_none_before_any_progress() {
    // Arrange
    let progress = TurnProgress::default();

    // Act
    let message = progress.message();

    // Assert
    assert_eq!(message, None);
}

#[test]
fn observe_label_ignores_blank_labels() {
    // Arrange
    let mut progress = TurnProgress::default();
    progress.observe_label("Thinking");

    // Act
    let observed = progress.observe_label(" \n ");

    // Assert
    assert!(!observed);
    assert_eq!(progress.message().as_deref(), Some("Thinking"));
}

#[test]
fn reasoning_fragments_concatenate_into_the_newest_line() {
    // Arrange
    let mut progress = TurnProgress::default();
    progress.observe_label("Thinking");

    // Act
    for fragment in [
        "**Inspecting",
        " files**",
        "\n\n",
        "Reading the",
        " parser now",
    ] {
        progress.observe_reasoning(fragment);
    }

    // Assert
    assert_eq!(
        progress.message().as_deref(),
        Some("Reading the parser now")
    );
}

#[test]
fn trailing_blank_reasoning_keeps_the_completed_line_visible() {
    // Arrange
    let mut progress = TurnProgress::default();

    // Act
    progress.observe_reasoning("**Inspecting   files**");
    progress.observe_reasoning("\n\n");

    // Assert
    assert_eq!(progress.message().as_deref(), Some("Inspecting files"));
}

#[test]
fn whitespace_only_reasoning_falls_back_to_the_label() {
    // Arrange
    let mut progress = TurnProgress::default();
    progress.observe_label("Thinking");

    // Act
    progress.observe_reasoning(" \n");

    // Assert
    assert_eq!(progress.message().as_deref(), Some("Thinking"));
}

#[test]
fn new_label_replaces_earlier_reasoning() {
    // Arrange
    let mut progress = TurnProgress::default();
    progress.observe_reasoning("Planning the edit");

    // Act
    let observed = progress.observe_label("Running a command");

    // Assert
    assert!(observed);
    assert_eq!(progress.message().as_deref(), Some("Running a command"));
}

#[test]
fn long_reasoning_line_keeps_its_newest_characters() {
    // Arrange
    let mut progress = TurnProgress::default();
    let line = format!("{}é end", "word ".repeat(1_000));

    // Act
    progress.observe_reasoning(&line);

    // Assert
    let message = progress.message().expect("reasoning preview");
    assert!(progress.reasoning.len() <= REASONING_BUFFER_BYTE_LIMIT);
    assert!(message.starts_with('…'));
    assert!(message.ends_with("word é end"));
    assert!(message.chars().count() <= REASONING_PREVIEW_CHAR_LIMIT);
}

#[test]
fn reasoning_buffer_trims_on_a_character_boundary() {
    // Arrange
    let mut progress = TurnProgress::default();
    let line = format!("{}a", "é".repeat(3_000));

    // Act
    progress.observe_reasoning(&line);

    // Assert
    assert_eq!(progress.reasoning.len(), REASONING_BUFFER_BYTE_LIMIT - 1);
    assert!(progress.reasoning.ends_with("éa"));
}
