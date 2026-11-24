//! Live usage readout tests: every width tier, honesty marks, local and
//! unpriced charges, the absolute context without a window, and gauge tones.

use ratatui::style::Color;

use super::UsageReadout;
use crate::ui::style::palette;
use crate::ui::usage_format::{
    CostFigure, ReadoutContext, ReadoutFigures, ReadoutTurn, UsageFigure,
};

/// Returns a known token figure.
fn tokens(text: &str, is_lower_bound: bool) -> UsageFigure {
    UsageFigure::Known {
        is_lower_bound,
        text: text.to_string(),
    }
}

/// Returns an exact charged estimate.
fn cost(text: &str) -> UsageFigure {
    UsageFigure::Cost(CostFigure {
        is_api_equivalent: false,
        is_lower_bound: false,
        is_upper_bound: false,
        text: text.to_string(),
    })
}

/// Returns a charge with the given bounds and basis.
fn bounded_cost(
    text: &str,
    is_lower_bound: bool,
    is_upper_bound: bool,
    is_api_equivalent: bool,
) -> UsageFigure {
    UsageFigure::Cost(CostFigure {
        is_api_equivalent,
        is_lower_bound,
        is_upper_bound,
        text: text.to_string(),
    })
}

/// Returns a context fill of `percent` with its gauge cells.
fn context(percent: u64, gauge_filled: u64) -> ReadoutContext {
    ReadoutContext {
        gauge_filled: Some(gauge_filled),
        percent: Some(percent),
        tokens: "380k".to_string(),
    }
}

/// Returns the readout of a priced running turn at 38% context.
fn running_figures() -> ReadoutFigures {
    ReadoutFigures {
        context: Some(context(38, 2)),
        session_cost: cost("$3.27"),
        turn: Some(ReadoutTurn {
            cost: cost("$0.24"),
            input: tokens("412k", false),
            output: tokens("3.1k", false),
        }),
    }
}

/// Renders the turn readout at `width`, or `None` when it hides.
fn text(readout: &UsageReadout, width: usize) -> Option<String> {
    readout.turn_line(width).map(|line| line.to_string())
}

#[test]
fn test_session_line_leaves_out_the_running_turn() {
    // Arrange
    let readout = UsageReadout::new(running_figures(), false);

    // Act
    let session = readout.session_line(80).map(|line| line.to_string());
    let narrow = readout.session_line(6);

    // Assert
    assert_eq!(session.as_deref(), Some("ctx 38% · $3.27 session"));
    assert_eq!(narrow, None);
}

#[test]
fn test_running_readout_drops_segments_by_priority_as_width_narrows() {
    // Arrange
    let readout = UsageReadout::new(running_figures(), false);
    let full = "↑412k ↓3.1k · ctx ▰▰▱▱▱ 38% · $0.24 turn · $3.27 session";
    let without_session = "↑412k ↓3.1k · ctx ▰▰▱▱▱ 38% · $0.24 turn";
    let without_tokens = "ctx ▰▰▱▱▱ 38% · $0.24 turn";
    let without_gauge = "ctx 38% · $0.24 turn";
    let context_only = "ctx 38%";
    let width = |text: &str| text.chars().count();

    // Act, Assert
    assert_eq!(text(&readout, 200).as_deref(), Some(full));
    assert_eq!(text(&readout, width(full)).as_deref(), Some(full));
    assert_eq!(
        text(&readout, width(full) - 1).as_deref(),
        Some(without_session)
    );
    assert_eq!(
        text(&readout, width(without_session) - 1).as_deref(),
        Some(without_tokens)
    );
    assert_eq!(
        text(&readout, width(without_tokens) - 1).as_deref(),
        Some(without_gauge)
    );
    assert_eq!(
        text(&readout, width(without_gauge) - 1).as_deref(),
        Some(context_only)
    );
    assert_eq!(
        text(&readout, width(context_only)).as_deref(),
        Some(context_only)
    );
    assert_eq!(text(&readout, width(context_only) - 1), None);
}

#[test]
fn test_idle_readout_shows_context_and_session_cost_without_a_gauge() {
    // Arrange
    let readout = UsageReadout::new(
        ReadoutFigures {
            turn: None,
            ..running_figures()
        },
        false,
    );

    // Act, Assert
    assert_eq!(
        text(&readout, 80).as_deref(),
        Some("ctx 38% · $3.27 session")
    );
    assert_eq!(text(&readout, 10).as_deref(), Some("ctx 38%"));
    assert_eq!(text(&readout, 6), None);
}

#[test]
fn test_idle_readout_without_context_shows_session_cost_until_it_does_not_fit() {
    // Arrange
    let readout = UsageReadout::new(
        ReadoutFigures {
            context: None,
            session_cost: cost("<$0.01"),
            turn: None,
        },
        false,
    );

    // Act, Assert
    assert_eq!(text(&readout, 80).as_deref(), Some("<$0.01 session"));
    assert_eq!(text(&readout, 13), None);
}

#[test]
fn test_lower_bound_marks_tokens_and_costs() {
    // Arrange
    let readout = UsageReadout::new(
        ReadoutFigures {
            context: Some(context(38, 2)),
            session_cost: bounded_cost("$0.00", true, false, false),
            turn: Some(ReadoutTurn {
                cost: bounded_cost("$0.24", true, false, false),
                input: tokens("412k", true),
                output: tokens("3.1k", true),
            }),
        },
        false,
    );

    // Act, Assert
    assert_eq!(
        text(&readout, 200).as_deref(),
        Some("↑≥412k ↓≥3.1k · ctx ▰▰▱▱▱ 38% · ≥$0.24 turn · ≥$0.00 session")
    );
}

#[test]
fn test_upper_and_both_bounds_mark_charges() {
    // Arrange
    let readout = UsageReadout::new(
        ReadoutFigures {
            context: Some(context(20, 1)),
            session_cost: bounded_cost("$1,230", true, true, false),
            turn: Some(ReadoutTurn {
                cost: bounded_cost("$3.10", false, true, false),
                input: tokens("30.8M", false),
                output: tokens("80.9k", false),
            }),
        },
        false,
    );

    // Act, Assert
    assert_eq!(
        text(&readout, 200).as_deref(),
        Some("↑30.8M ↓80.9k · ctx ▰▱▱▱▱ 20% · ≤$3.10 turn · ~$1,230 session")
    );
}

#[test]
fn test_api_equivalent_charges_read_api_eq_and_pairs_show_both_sums() {
    // Arrange
    let api_readout = UsageReadout::new(
        ReadoutFigures {
            context: Some(context(51, 3)),
            session_cost: bounded_cost("$2.05", false, false, true),
            turn: Some(ReadoutTurn {
                cost: bounded_cost("$0.31", false, true, true),
                input: tokens("286k", false),
                output: tokens("4.4k", false),
            }),
        },
        false,
    );
    let pair_readout = UsageReadout::new(
        ReadoutFigures {
            turn: None,
            session_cost: UsageFigure::CostPair {
                charged: CostFigure {
                    is_api_equivalent: false,
                    is_lower_bound: false,
                    is_upper_bound: false,
                    text: "$1.00".to_string(),
                },
                equivalent: CostFigure {
                    is_api_equivalent: true,
                    is_lower_bound: false,
                    is_upper_bound: false,
                    text: "$2.00".to_string(),
                },
            },
            ..running_figures()
        },
        false,
    );

    // Act, Assert
    assert_eq!(
        text(&api_readout, 200).as_deref(),
        Some("↑286k ↓4.4k · ctx ▰▰▰▱▱ 51% · ≈$0.31 API-eq · ≈$2.05 session")
    );
    assert_eq!(
        text(&pair_readout, 200).as_deref(),
        Some("ctx 38% · $1.00 + ≈$2.00 session")
    );
}

#[test]
fn test_unpriced_and_unreported_figures_read_question_marks() {
    // Arrange
    let readout = UsageReadout::new(
        ReadoutFigures {
            context: None,
            session_cost: UsageFigure::Unpriced,
            turn: Some(ReadoutTurn {
                cost: UsageFigure::Unpriced,
                input: UsageFigure::Unreported,
                output: UsageFigure::Unreported,
            }),
        },
        false,
    );

    // Act, Assert
    assert_eq!(
        text(&readout, 200).as_deref(),
        Some("↑? ↓? · $? turn · $? session")
    );
}

#[test]
fn test_local_model_reads_local_once_instead_of_a_price() {
    // Arrange
    let running = UsageReadout::new(
        ReadoutFigures {
            context: Some(context(63, 3)),
            session_cost: UsageFigure::Unpriced,
            turn: Some(ReadoutTurn {
                cost: UsageFigure::Unpriced,
                input: tokens("48.2k", false),
                output: tokens("1.9k", false),
            }),
        },
        true,
    );
    let idle = UsageReadout::new(
        ReadoutFigures {
            context: Some(context(63, 3)),
            session_cost: UsageFigure::Unpriced,
            turn: None,
        },
        true,
    );
    let priced_session = UsageReadout::new(
        ReadoutFigures {
            context: None,
            session_cost: cost("$0.50"),
            turn: Some(ReadoutTurn {
                cost: UsageFigure::Unpriced,
                input: tokens("1k", false),
                output: tokens("10", false),
            }),
        },
        true,
    );

    // Act, Assert
    assert_eq!(
        text(&running, 200).as_deref(),
        Some("↑48.2k ↓1.9k · ctx ▰▰▰▱▱ 63% · local")
    );
    assert_eq!(text(&idle, 200).as_deref(), Some("ctx 63% · local"));
    assert_eq!(
        text(&priced_session, 200).as_deref(),
        Some("↑1k ↓10 · local · $0.50 session")
    );
}

#[test]
fn test_unknown_window_shows_absolute_context_without_a_gauge() {
    // Arrange
    let readout = UsageReadout::new(
        ReadoutFigures {
            context: Some(ReadoutContext {
                gauge_filled: None,
                percent: None,
                tokens: "71.2k".to_string(),
            }),
            session_cost: UsageFigure::Unpriced,
            turn: Some(ReadoutTurn {
                cost: UsageFigure::Unpriced,
                input: tokens("18.4k", false),
                output: tokens("0.9k", false),
            }),
        },
        false,
    );

    // Act, Assert
    assert_eq!(
        text(&readout, 200).as_deref(),
        Some("↑18.4k ↓0.9k · ctx 71.2k · $? turn · $? session")
    );
    assert_eq!(text(&readout, 20).as_deref(), Some("ctx 71.2k · $? turn"));
}

#[test]
fn test_charge_the_backend_did_not_record_shows_nothing() {
    // Arrange
    let readout = UsageReadout::new(
        ReadoutFigures {
            context: Some(context(38, 2)),
            session_cost: UsageFigure::NotRecorded,
            turn: None,
        },
        false,
    );

    // Act, Assert
    assert_eq!(text(&readout, 200).as_deref(), Some("ctx 38%"));
}

#[test]
fn test_gauge_tone_rises_with_context_fill() {
    // Arrange
    let gauge_color = |percent: u64, filled: u64| -> Option<Color> {
        let readout = UsageReadout::new(
            ReadoutFigures {
                context: Some(context(percent, filled)),
                ..running_figures()
            },
            false,
        );
        let line = readout.turn_line(200)?;

        line.spans
            .iter()
            .find(|span| span.content.contains('▰') || span.content.contains('▱'))
            .and_then(|span| span.style.fg)
    };

    // Act, Assert
    assert_eq!(gauge_color(74, 4), Some(palette::text_muted()));
    assert_eq!(gauge_color(75, 4), Some(palette::warning()));
    assert_eq!(gauge_color(90, 5), Some(palette::danger()));
}
