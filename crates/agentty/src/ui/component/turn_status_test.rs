//! Turn status-line tests covering its segments, the telemetry slot, and
//! the order segments give way as the pane narrows.

use ratatui::text::{Line, Span};

use super::{TurnStatusInput, telemetry_max_width, turn_status_line};
use crate::domain::turn_timeline::CurrentAction;
use crate::ui::component::turn_timeline::spinner_glyph;
use crate::ui::style;

/// Returns a running-command action that started at Unix second 100.
fn running() -> CurrentAction {
    CurrentAction {
        label: "Running a command".into(),
        since: 100_000,
        text: "Running cargo test -p agentty --lib domain::composer".into(),
    }
}

/// Returns an action with a long label, such as a reasoning title, that
/// started at Unix second 100.
fn long_action() -> CurrentAction {
    let label = "Reading cargo test -p agentty --lib domain::composer";

    CurrentAction {
        label: label.into(),
        since: 100_000,
        text: label.into(),
    }
}

/// Returns the input for `current` at Unix second 112 on step 12 of a turn
/// that ran 10m 55s.
fn input(current: Option<&CurrentAction>) -> TurnStatusInput<'_> {
    TurnStatusInput {
        current,
        now_millis: 112_400,
        spinner_frame: 2,
        step_count: 12,
        turn_seconds: 655,
    }
}

/// Returns a telemetry line reading `text`.
fn telemetry(text: &str) -> Line<'static> {
    Line::from(Span::raw(text.to_string()))
}

#[test]
fn wide_lines_show_every_segment_and_the_telemetry_slot_without_key_hints() {
    // Arrange
    let current = running();

    // Act
    let with_slot = turn_status_line(input(Some(&current)), Some(telemetry("↑118k ctx 45%")), 170);
    let without_slot = turn_status_line(input(Some(&current)), None, 170);

    // Assert
    // The running row in the timeline names the command, so the status
    // line names only the kind of action.
    let text = with_slot.to_string();
    assert!(text.starts_with(&format!(
        "{} Running a command 12s  ·  step 12  ·  turn 10m 55s",
        spinner_glyph(2)
    )));
    assert!(text.ends_with("  ↑118k ctx 45%"));
    assert!(!text.contains("cargo test"));
    assert_eq!(with_slot.width(), 170);
    assert_eq!(
        without_slot.to_string(),
        format!(
            "{} Running a command 12s  ·  step 12  ·  turn 10m 55s",
            spinner_glyph(2)
        )
    );
    assert!(!text.contains("Ctrl+c"));
    assert_eq!(with_slot.spans[1].content, "Running a command");
    assert_eq!(with_slot.spans[1].style.fg, Some(style::palette::warning()));
}

#[test]
fn narrowing_drops_the_slot_turn_step_and_timer_before_truncating_the_action() {
    // Arrange
    let current = long_action();
    let render = |width| {
        turn_status_line(input(Some(&current)), Some(telemetry("↑118k")), width).to_string()
    };

    // Act
    let rendered = [100, 90, 80, 69, 57, 40].map(|width| (width, render(width)));

    // Assert
    let [at_100, at_90, at_80, at_69, at_57, at_40] = &rendered;
    assert!(at_100.1.contains("↑118k"));
    assert!(!at_90.1.contains("↑118k"));
    assert!(at_90.1.contains("turn 10m 55s"));
    assert!(!at_80.1.contains("turn"));
    assert!(at_80.1.contains("step 12"));
    assert!(!at_69.1.contains("step"));
    assert!(at_69.1.contains(" 12s"));
    assert!(!at_57.1.contains(" 12s"));
    assert!(at_57.1.contains("domain::composer"));
    assert!(at_40.1.ends_with("..."));
    for (width, text) in &rendered {
        assert!(
            Line::raw(text.as_str()).width() <= usize::from(*width),
            "{width}: {text}"
        );
    }
}

#[test]
fn turns_without_an_action_read_working_and_hide_a_zero_step_count() {
    // Arrange
    let reasoning = CurrentAction {
        label: "Weighing the options".into(),
        since: 112_000,
        text: "Weighing the options".into(),
    };

    // Act
    let idle = turn_status_line(
        TurnStatusInput {
            step_count: 0,
            ..input(None)
        },
        Some(Line::raw("")),
        120,
    );
    let thinking = turn_status_line(input(Some(&reasoning)), None, 120);

    // Assert
    assert_eq!(
        idle.to_string(),
        format!("{} Working  ·  turn 10m 55s", spinner_glyph(2))
    );
    assert!(thinking.to_string().contains("Weighing the options 0s"));
}

#[test]
fn telemetry_slots_never_exceed_their_maximum_width() {
    // Arrange
    let current = running();

    // Act
    let oversized = turn_status_line(input(Some(&current)), Some(telemetry(&"x".repeat(41))), 170);

    // Assert
    assert_eq!(telemetry_max_width(60), 0);
    assert_eq!(telemetry_max_width(100), 20);
    assert_eq!(telemetry_max_width(170), 40);
    assert!(!oversized.to_string().contains("xxx"));
}
