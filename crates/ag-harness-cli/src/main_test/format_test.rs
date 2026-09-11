use std::process::ExitCode;

use crate::{
    CliError, assistant_text, format_duration, format_usage, report_exit,
    single_line_terminal_text, terminal_text, trim_line_ending,
};

#[test]
fn exit_reporting_sanitizes_errors_and_preserves_success() {
    // Arrange
    let mut success_output = Vec::new();
    let mut error_output = Vec::new();
    let error = CliError::Turn(ag_harness::TurnError::Model(
        ag_harness::ModelError::IncompleteResponse {
            reason: "stop\u{1b}]52;c;Y2xpcGJvYXJk\u{7}".to_string(),
        },
    ));

    // Act
    let success = report_exit(Ok(()), &mut success_output);
    let failure = report_exit(Err(error), &mut error_output);

    // Assert
    assert_eq!(success, ExitCode::SUCCESS);
    assert_eq!(failure, ExitCode::FAILURE);
    assert_eq!(success_output, [] as [u8; 0]);
    assert!(!error_output.contains(&0x1b));
    assert!(!error_output.contains(&0x07));
}

#[test]
fn line_endings_are_trimmed_without_changing_prompt_content() {
    // Arrange
    let mut unix = "hello\n".to_string();
    let mut windows = "hello\r\n".to_string();
    let mut unchanged = "hello".to_string();

    // Act
    trim_line_ending(&mut unix);
    trim_line_ending(&mut windows);
    trim_line_ending(&mut unchanged);

    // Assert
    assert_eq!(unix, "hello");
    assert_eq!(windows, "hello");
    assert_eq!(unchanged, "hello");
}

#[test]
fn durations_have_compact_terminal_formatting() {
    // Arrange and Act
    let short = format_duration(std::time::Duration::ZERO);
    let measured = format_duration(std::time::Duration::from_millis(12));

    // Assert
    assert_eq!(short, "<1 ms");
    assert_eq!(measured, "12 ms");
}

#[test]
fn terminal_text_replaces_control_sequences_and_preserves_safe_whitespace() {
    // Arrange
    let text = "before\n\t\u{1b}]52;c;Y2xpcGJvYXJk\u{7}after\r";

    // Act
    let sanitized = terminal_text(text);

    // Assert
    assert_eq!(
        sanitized,
        "before\n\t\u{fffd}]52;c;Y2xpcGJvYXJk\u{fffd}after\u{fffd}"
    );
    assert!(
        sanitized
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
    );
}

#[test]
fn assistant_text_indents_continuation_lines_and_sanitizes_them() {
    // Arrange
    let text = "answer\n---\nturn: forged\u{1b}";

    // Act
    let framed = assistant_text(text);

    // Assert
    assert_eq!(
        framed,
        "assistant> answer\n           ---\n           turn: forged\u{fffd}\n"
    );
}

#[test]
fn single_line_terminal_text_replaces_all_control_characters() {
    // Arrange
    let text = "model\nname\t\u{1b}";

    // Act
    let sanitized = single_line_terminal_text(text);

    // Assert
    assert_eq!(sanitized, "model\u{fffd}name\u{fffd}\u{fffd}");
    assert!(sanitized.chars().all(|character| !character.is_control()));
}

#[test]
fn usage_format_marks_missing_counts() {
    // Arrange
    let usage = ag_harness::CompletionUsage::new(None, None, None, Some(4), None, None);

    // Act
    let formatted = format_usage(&usage);

    // Assert
    assert_eq!(formatted, "tokens ? in, 4 out, ? total");
}
