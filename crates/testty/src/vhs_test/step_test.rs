use std::path::Path;

use super::super::{
    compile_step, escape_shell_single_quote, escape_vhs_double_quote, escape_vhs_regex,
    format_vhs_duration, key_to_vhs_command,
};
use crate::step::Step;

#[test]
fn key_to_vhs_command_maps_common_keys() {
    // Arrange / Act / Assert
    assert_eq!(key_to_vhs_command("Enter"), "Enter");
    assert_eq!(key_to_vhs_command("tab"), "Tab");
    assert_eq!(key_to_vhs_command("BackTab"), "Shift+Tab");
    assert_eq!(key_to_vhs_command("shift+tab"), "Shift+Tab");
    assert_eq!(key_to_vhs_command("escape"), "Escape");
    assert_eq!(key_to_vhs_command("up"), "Up");
    assert_eq!(key_to_vhs_command("ctrl+c"), "Ctrl+C");
}

#[test]
fn compile_step_wait_for_text_emits_wait_screen_with_regex() {
    // Arrange
    let step = Step::wait_for_text("Loading", 5000);
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(tape.contains("Wait+Screen@5s /Loading/"));
}

#[test]
fn compile_step_wait_for_text_formats_fractional_timeout_as_milliseconds() {
    // Arrange
    let step = Step::wait_for_text("Startup", 1500);
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(tape.contains("Wait+Screen@1500ms /Startup/"));
}

#[test]
fn compile_step_wait_for_text_escapes_regex_metacharacters() {
    // Arrange
    let step = Step::wait_for_text("[test] foo.bar", 3000);
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert — brackets and dot are escaped.
    assert!(tape.contains(r"Wait+Screen@3s /\[test\] foo\.bar/"));
}

#[test]
fn compile_step_viewing_pause_emits_sleep_seconds() {
    // Arrange
    let step = Step::viewing_pause_ms(2000);
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(tape.contains("Sleep 2s"));
}

#[test]
fn compile_step_viewing_pause_emits_sleep_milliseconds() {
    // Arrange
    let step = Step::viewing_pause_ms(1500);
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(tape.contains("Sleep 1500ms"));
}

/// Verifies `Step::Eventually` emits a fallback `Sleep` for the full
/// timeout in seconds when the timeout is an even multiple of one
/// second, so VHS playback waits at least the upper bound the PTY
/// executor would have observed before the next step fires.
#[test]
fn compile_step_eventually_emits_sleep_seconds_for_even_timeout() {
    // Arrange
    let step = Step::eventually(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(50),
        |_frame| Ok(()),
    );
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(
        tape.contains("Sleep 5s"),
        "expected Sleep 5s fallback, got: {tape}"
    );
}

/// Verifies `Step::Eventually` falls back to a millisecond `Sleep` for
/// fractional timeouts so the upper bound stays accurate for short
/// predicate windows.
#[test]
fn compile_step_eventually_emits_sleep_milliseconds_for_fractional_timeout() {
    // Arrange
    let step = Step::eventually(
        std::time::Duration::from_millis(750),
        std::time::Duration::from_millis(25),
        |_frame| Ok(()),
    );
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(
        tape.contains("Sleep 750ms"),
        "expected Sleep 750ms fallback, got: {tape}"
    );
}

#[test]
fn compile_step_sleep_uses_seconds_when_even() {
    // Arrange
    let step = Step::sleep_ms(3000);
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(tape.contains("Sleep 3s"));
}

#[test]
fn compile_step_sleep_uses_milliseconds_when_fractional() {
    // Arrange
    let step = Step::sleep_ms(500);
    let mut tape = String::new();

    // Act
    compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

    // Assert
    assert!(tape.contains("Sleep 500ms"));
}

#[test]
fn escape_vhs_double_quote_escapes_quotes_and_backslashes() {
    // Arrange / Act / Assert
    assert_eq!(escape_vhs_double_quote(r#"hello"world"#), r#"hello\"world"#);
    assert_eq!(escape_vhs_double_quote(r"back\slash"), r"back\\slash");
    assert_eq!(escape_vhs_double_quote("clean"), "clean");
}

#[test]
fn escape_shell_single_quote_wraps_internal_quotes() {
    // Arrange / Act / Assert
    assert_eq!(escape_shell_single_quote("it's"), "it'\\''s");
    assert_eq!(escape_shell_single_quote("clean"), "clean");
}

#[test]
fn escape_vhs_regex_escapes_metacharacters() {
    // Arrange / Act / Assert
    assert_eq!(escape_vhs_regex("plain"), "plain");
    assert_eq!(escape_vhs_regex("a.b"), r"a\.b");
    assert_eq!(escape_vhs_regex("[x]"), r"\[x\]");
    assert_eq!(escape_vhs_regex("a/b"), r"a\/b");
    assert_eq!(escape_vhs_regex("a+b*c?"), r"a\+b\*c\?");
    assert_eq!(escape_vhs_regex(r"back\slash"), r"back\\slash");
}

#[test]
fn format_vhs_duration_uses_seconds_for_even_multiples() {
    // Arrange / Act / Assert
    assert_eq!(format_vhs_duration(1000), "1s");
    assert_eq!(format_vhs_duration(5000), "5s");
    assert_eq!(format_vhs_duration(30000), "30s");
}

#[test]
fn format_vhs_duration_uses_milliseconds_for_fractional() {
    // Arrange / Act / Assert
    assert_eq!(format_vhs_duration(500), "500ms");
    assert_eq!(format_vhs_duration(1500), "1500ms");
    assert_eq!(format_vhs_duration(100), "100ms");
}
