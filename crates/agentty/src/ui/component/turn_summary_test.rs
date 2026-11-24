//! Turn-summary layout tests at narrow and wide panes, with format
//! detection, per-section columns, and styling.

use ratatui::style::Modifier;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use super::{is_turn_summary, turn_summary_lines};
use crate::ui::style;

/// Persisted summary of the design's turn.
const DESIGN_SUMMARY: &str = "turn-summary v1\n14 steps · 51 tools · 20m \
                              23s\nEdited\t✓\tcrates/agentty/src/domain/composer.rs\t+79 −14\t7 \
                              edits\n\t✓\tcrates/agentty/src/runtime/mode/prompt.rs\t+24 −6\t2 \
                              edits\nFailed\t✓\tcargo test -p agentty --lib domain::composer 2>&1 \
                              | tail -20\texit 101\t→ rerun exit 0\n\t✓\t2 edits to composer.rs \
                              rejected (hunk context mismatch)\t\t→ retried after \
                              re-reading\n\t✗\tcargo clippy\texit 1\t\nExplored\t25 reads · 7 \
                              searches · ran 5 commands\n─ Worked 20m 23s · 14 steps · 51 tools · \
                              4 failed (2 rejected edits, 2 nonzero exits), 3 recovered";

/// Returns each line's text without trailing spaces.
fn texts(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect()
}

/// Returns the span of `line` whose text is `content`.
fn span<'a>(line: &'a Line<'static>, content: &str) -> &'a ratatui::text::Span<'static> {
    line.spans
        .iter()
        .find(|span| span.content.trim() == content)
        .expect("span")
}

#[test]
fn summaries_are_told_apart_by_their_format_marker() {
    // Arrange, Act, Assert
    assert!(is_turn_summary(DESIGN_SUMMARY));
    assert!(is_turn_summary("turn-summary v1\n─ Worked 1 tool"));
    assert!(!is_turn_summary("turn-summary v2\n─ Worked 1 tool"));
    assert!(!is_turn_summary("─ Worked 1 tool"));
    assert!(!is_turn_summary("Tools: Read ×2\nSkills: review ×1"));
    assert!(!is_turn_summary(""));
}

#[test]
fn wide_pane_aligns_each_section_in_its_own_columns() {
    // Arrange, Act
    let lines = turn_summary_lines(DESIGN_SUMMARY, 170);

    // Assert
    let divider = "─ Worked 20m 23s · 14 steps · 51 tools · 4 failed (2 rejected edits, 2 nonzero \
                   exits), 3 recovered";
    assert_eq!(
        texts(&lines),
        [
            "14 steps · 51 tools · 20m 23s",
            "",
            "Edited    ✓ crates/agentty/src/domain/composer.rs      +79 −14  7 edits",
            "          ✓ crates/agentty/src/runtime/mode/prompt.rs  +24 −6   2 edits",
            "Failed    ✓ cargo test -p agentty --lib domain::composer 2>&1 | tail -20  exit 101  \
             → rerun exit 0",
            "          ✓ 2 edits to composer.rs rejected (hunk context mismatch)       → retried \
             after re-reading",
            "          ✗ cargo clippy                                                  exit 1",
            "Explored  25 reads · 7 searches · ran 5 commands",
            "",
            &format!("{divider} {}", "─".repeat(170 - divider.width() - 1)),
        ]
    );
}

#[test]
fn medium_pane_truncates_failed_targets_without_moving_edit_counts() {
    // Arrange, Act
    let lines = turn_summary_lines(DESIGN_SUMMARY, 80);

    // Assert
    let texts = texts(&lines);
    assert_eq!(
        texts[2],
        "Edited    ✓ crates/agentty/src/domain/composer.rs      +79 −14  7 edits"
    );
    assert_eq!(
        texts[4],
        "Failed    ✓ cargo test -p agentty --lib...  exit 101  → rerun exit 0"
    );
    assert_eq!(
        texts[5],
        "          ✓ 2 edits to composer.rs reje...  → retried after re-reading"
    );
    assert_eq!(
        texts[9],
        "─ Worked 20m 23s · 14 steps · 51 tools · 4 failed (2 rejected edits, 2 nonzero"
    );
    assert!(texts[10].starts_with("exits), 3 recovered ─"));
    assert!(lines.iter().all(|line| line.width() <= 80));
}

#[test]
fn a_long_failure_reason_leaves_edit_rows_alone() {
    // Arrange
    let summary = "turn-summary v1\n2 tools\nEdited\t✓\tsrc/lib.rs\t+3 −1\t1 \
                   edit\nFailed\t✗\tdeploy\tdenied · the approval policy blocks deploys from this \
                   host\t\n─ Worked 2 tools";

    // Act
    let lines = turn_summary_lines(summary, 80);

    // Assert
    assert_eq!(texts(&lines)[2], "Edited    ✓ src/lib.rs  +3 −1  1 edit");
}

#[test]
fn narrow_pane_drops_notes_and_wraps_lines_under_their_heading() {
    // Arrange, Act
    let lines = turn_summary_lines(DESIGN_SUMMARY, 30);

    // Assert
    let texts = texts(&lines);
    assert_eq!(texts[2], "Edited    ✓ crat…composer.r...");
    assert_eq!(texts[5], "          ✓ 2 edits to co...");
    assert_eq!(
        &texts[7..10],
        [
            "Explored  25 reads · 7",
            "          searches · ran 5",
            "          commands"
        ]
    );
    assert!(lines.iter().all(|line| line.width() <= 30));
}

#[test]
fn rows_color_glyphs_results_and_every_recovery_alike() {
    // Arrange, Act
    let lines = turn_summary_lines(DESIGN_SUMMARY, 170);

    // Assert
    let heading = span(&lines[2], "Edited");
    assert!(heading.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(
        span(&lines[2], "✓").style.fg,
        Some(style::palette::success())
    );
    assert_eq!(
        span(&lines[2], "+79").style.fg,
        Some(style::palette::success())
    );
    assert_eq!(
        span(&lines[2], "−14").style.fg,
        Some(style::palette::danger())
    );
    assert_eq!(
        span(&lines[2], "7 edits").style.fg,
        Some(style::palette::text_subtle())
    );
    assert_eq!(
        span(&lines[4], "exit 101").style.fg,
        Some(style::palette::danger())
    );
    assert_eq!(
        span(&lines[4], "→ rerun exit 0").style.fg,
        Some(style::palette::success())
    );
    assert_eq!(
        span(&lines[5], "→ retried after re-reading").style.fg,
        Some(style::palette::success())
    );
    assert_eq!(
        span(&lines[6], "✗").style.fg,
        Some(style::palette::danger())
    );
}

#[test]
fn divider_alone_and_unknown_rows_still_render() {
    // Arrange, Act
    let alone = turn_summary_lines("turn-summary v1\n─ Worked 1 tool", 20);
    let exact = turn_summary_lines("turn-summary v1\n─ Worked 1 tool", 15);
    let odd = turn_summary_lines(
        "turn-summary v1\n1 tool\n\tloose\nloose text\n─ Worked 1 tool",
        20,
    );
    let other = turn_summary_lines(
        "turn-summary v1\n1 tool\nOther\t✓\tthing\t+1 lines\t\n─ Worked 1 tool",
        40,
    );

    // Assert
    assert_eq!(texts(&alone), ["─ Worked 1 tool ────"]);
    assert_eq!(texts(&exact), ["─ Worked 1 tool"]);
    assert_eq!(
        texts(&turn_summary_lines("turn-summary v1", 20)),
        Vec::<String>::new()
    );
    assert_eq!(texts(&other)[2], "Other     ✓ thing  +1 lines");
    assert_eq!(
        span(&other[2], "lines").style.fg,
        Some(style::palette::text_subtle())
    );
    assert_eq!(
        texts(&odd),
        [
            "1 tool",
            "",
            "          loose",
            "loose text",
            "",
            "─ Worked 1 tool ────"
        ]
    );
}
