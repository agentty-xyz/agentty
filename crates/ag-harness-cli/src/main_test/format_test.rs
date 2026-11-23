//! Terminal output formatting unit tests for exit reporting, text sanitizing,
//! durations, and usage counts.

use std::process::ExitCode;

use ag_harness::{ModelError, RunError, TurnError, TurnRecord, Usage, UsageTotals};
use serde_json::json;

use crate::{
    CliError, assistant_text, format_duration, format_request_usage, format_summed_tokens,
    format_usd, framed_text, history_text, report_exit, single_line_terminal_text, terminal_text,
    trim_line_ending, usage_totals_text,
};

#[test]
fn exit_reporting_sanitizes_errors_and_preserves_success() {
    // Arrange
    let mut success_output = Vec::new();
    let mut error_output = Vec::new();
    let error = CliError::Run(RunError::from(TurnError::Model(
        ModelError::IncompleteResponse {
            reason: "stop\u{1b}]52;c;Y2xpcGJvYXJk\u{7}".to_string(),
        },
    )));

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
fn dollars_keep_two_decimals_and_every_significant_micro_usd() {
    // Arrange and Act
    let amounts = [0, 12, 75_000, 250_000, 1_500_000, 12_345_678].map(format_usd);

    // Assert
    assert_eq!(
        amounts,
        [
            "$0.00",
            "$0.000012",
            "$0.075",
            "$0.25",
            "$1.50",
            "$12.345678"
        ]
    );
}

#[test]
fn token_totals_name_cached_reasoning_and_unreported_counts() {
    // Arrange
    let plain = Usage::default();
    let mut detailed = Usage::default();
    detailed.input = 9;
    detailed.output = 3;
    detailed.cache_read = 4;
    detailed.reasoning = 2;
    detailed.unreported_requests = 1;

    // Act
    let plain = format_summed_tokens(&plain);
    let detailed = format_summed_tokens(&detailed);

    // Assert
    assert_eq!(plain, "tokens 0 in, 0 out");
    assert_eq!(
        detailed,
        "tokens 9 in, 3 out, 4 cached, 2 reasoning, 1 model calls unreported"
    );
}

#[test]
fn session_usage_names_unpriced_and_unknown_turns() {
    // Arrange
    let mut totals = UsageTotals::default();
    totals.turns = 3;
    totals.unpriced_turns = 1;
    totals.unknown_turns = 1;

    // Act
    let text = usage_totals_text(&totals);

    // Assert
    assert_eq!(
        text,
        "session usage: 3 turns; tokens 0 in, 0 out; cost unavailable; 1 turns unpriced; 1 turns \
         without usage\n"
    );
}

#[test]
fn history_shows_text_prompts_and_answers_but_not_images_or_reasoning() {
    // Arrange
    let turn: TurnRecord = serde_json::from_value(json!({
        "cost": null,
        "error": {"kind": "cancelled"},
        "finished_at": 2,
        "input": [
            {"type": "text", "text": "describe\nthis"},
            {"type": "image", "data": "AA==", "media_type": "image/png"}
        ],
        "instructions": null,
        "items": [
            {"type": "reasoning", "text": "private"},
            {"type": "output", "output": {"type": "json", "value": {"message": "a cat"}}}
        ],
        "started_at": 1,
        "status": "interrupted",
        "turn": 4,
        "usage": null
    }))
    .expect("transcript turn should deserialize");

    // Act
    let text = history_text(&[turn]);

    // Assert
    assert_eq!(
        text,
        "turn 5 (interrupted)\nyou> describe\n     this\nassistant> a cat\n"
    );
}

#[test]
fn framed_text_indents_continuation_lines_to_its_label() {
    // Arrange and Act
    let framed = framed_text("you> ", "first\nsecond\u{1b}");

    // Assert
    assert_eq!(framed, "you> first\n     second\u{fffd}\n");
}

#[test]
fn usage_format_marks_missing_counts() {
    // Arrange
    let usage = ag_harness::model::CompletionUsage::default().with_output_tokens(Some(4));
    let missing_output_usage = ag_harness::model::CompletionUsage::default()
        .with_input_tokens(Some(3))
        .with_total_tokens(Some(7));

    // Act
    let formatted = format_request_usage(&usage);
    let missing_output_formatted = format_request_usage(&missing_output_usage);

    // Assert
    assert_eq!(formatted, "tokens ? in, 4 out, ? total");
    assert_eq!(missing_output_formatted, "tokens 3 in, ? out, 7 total");
}
