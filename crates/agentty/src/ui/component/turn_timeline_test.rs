//! Live-timeline rendering tests for step headers, the call block's totals
//! line, and call rows at narrow, medium, and wide panes.

use std::time::Duration;

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityFailure, ActivityKind, ActivityStatus, DiffStat,
    OutputEvent, OutputKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::{header_lines, hidden_steps_line, row_lines, running_time, spinner_glyph, stats_line};
use crate::domain::turn_timeline::{CallStats, TimelineSnapshot, ToolRow, TurnTimeline};
use crate::test_support::activity_event;
use crate::ui::{markdown, style};

/// Builds the timeline the design shows: narration with reasoning,
/// explorations, a rejected and a successful edit, a slow command, a failed
/// piped command with its output, and a running command.
fn design_timeline() -> TimelineSnapshot {
    let mut timeline = TurnTimeline::default();
    add_exploration_step(&mut timeline);
    add_edit_step(&mut timeline);
    add_command_step(&mut timeline);

    timeline.snapshot()
}

/// Streams one complete message item `id` at `at` milliseconds.
fn message(timeline: &mut TurnTimeline, id: u64, text: &str, at: u64) {
    timeline.observe_output(
        OutputEvent::Started {
            id,
            kind: OutputKind::Message,
        },
        at,
    );
    timeline.observe_output(
        OutputEvent::Completed {
            id,
            text: text.into(),
        },
        at,
    );
}

/// Returns facts naming only `target`.
fn facts(target: &str) -> ActivityFacts {
    ActivityFacts {
        target: Some(target.into()),
        ..ActivityFacts::default()
    }
}

/// Adds a step that reasons and then explores the repository.
fn add_exploration_step(timeline: &mut TurnTimeline) {
    message(
        timeline,
        0,
        "Reading the `/model` docs and the composer's picker stages.",
        0,
    );
    timeline.observe_output(
        OutputEvent::Started {
            id: 1,
            kind: OutputKind::Reasoning,
        },
        1_000,
    );
    timeline.observe_output(
        OutputEvent::Completed {
            id: 1,
            text: "**Planning**\nHarness models already carry a provider id, so a Provider stage \
                   between Agent and Model can filter rows."
                .into(),
        },
        7_000,
    );
    for (id, kind, target) in [
        (
            "r1",
            ActivityKind::Read,
            "crates/agentty/src/domain/model.rs",
        ),
        ("r2", ActivityKind::Read, "composer.rs"),
        ("s1", ActivityKind::Search, "Choosing a Model"),
        ("l1", ActivityKind::List, "."),
    ] {
        timeline.observe_activity(
            ActivityEvent {
                facts: Box::new(facts(target)),
                kind,
                status: ActivityStatus::Completed,
                ..activity_event(id)
            },
            8_000,
        );
    }
}

/// Adds a step whose first edit is rejected and whose read-then-edit
/// succeeds.
fn add_edit_step(timeline: &mut TurnTimeline) {
    message(timeline, 2, "Adding a Provider stage.", 192_000);
    let path = "crates/agentty/src/domain/composer.rs";
    timeline.observe_activity(
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                failure: Some(ActivityFailure::Rejected),
                reason: Some("hunk 1 context mismatch at line 1216".into()),
                ..facts(path)
            }),
            kind: ActivityKind::FileChange,
            output: Some("expected: fn model_rows".into()),
            status: ActivityStatus::Failed,
            ..activity_event("e1")
        },
        193_000,
    );
    timeline.observe_activity(
        ActivityEvent {
            facts: Box::new(facts(path)),
            kind: ActivityKind::Read,
            status: ActivityStatus::Completed,
            ..activity_event("r3")
        },
        194_000,
    );
    timeline.observe_activity(edit_event(path), 195_000);
}

/// Returns a successful edit of `path` that added 38 lines and removed 4.
fn edit_event(path: &str) -> ActivityEvent {
    ActivityEvent {
        facts: Box::new(ActivityFacts {
            diff_stat: Some(DiffStat {
                added: 38,
                removed: 4,
            }),
            ..facts(path)
        }),
        kind: ActivityKind::FileChange,
        status: ActivityStatus::Completed,
        ..activity_event("e2")
    }
}

/// Adds a step with a slow command, a failed piped command, and a running
/// one.
fn add_command_step(timeline: &mut TurnTimeline) {
    message(
        timeline,
        3,
        "Building and running the composer tests.",
        482_000,
    );
    timeline.observe_activity(
        ActivityEvent {
            exit_code: Some(0),
            facts: Box::new(ActivityFacts {
                duration: Some(Duration::from_secs(36)),
                ..ActivityFacts::default()
            }),
            input: Some("cargo check -p agentty --tests".into()),
            status: ActivityStatus::Completed,
            ..activity_event("c1")
        },
        483_000,
    );
    timeline.observe_activity(
        ActivityEvent {
            exit_code: Some(0),
            facts: Box::new(ActivityFacts {
                pipe_status: vec![101, 0],
                ..ActivityFacts::default()
            }),
            input: Some(
                "cargo test -p agentty --lib domain::composer::tests::model_rows 2>&1 | tail -20"
                    .into(),
            ),
            output: Some(
                "running 58 tests\ntest domain::composer::tests::model_rows_for_cli ... \
                 FAILED\ntest result: FAILED. 56 passed; 2 failed"
                    .into(),
            ),
            status: ActivityStatus::Completed,
            ..activity_event("c2")
        },
        520_000,
    );
    timeline.observe_activity(
        ActivityEvent {
            input: Some("cargo test -p agentty --lib domain::composer".into()),
            ..activity_event("c3")
        },
        580_000,
    );
}

/// Rendered rows of a timeline, with the indices of running rows.
#[derive(Default)]
struct TimelineLines {
    lines: Vec<Line<'static>>,
    spinner_rows: Vec<usize>,
}

/// Renders the timeline at `width` columns the way the session output
/// assembles it: each step's header, then the call block.
fn render(snapshot: &TimelineSnapshot, width: usize) -> TimelineLines {
    let mut lines = TimelineLines::default();
    for step in &snapshot.steps {
        lines.lines.extend(header_lines(step, width, None));
    }
    lines.lines.extend(stats_line(&snapshot.calls.stats, width));
    for row in snapshot.calls.latest.iter().chain(&snapshot.calls.running) {
        if row.running_since.is_some() {
            lines.spinner_rows.push(lines.lines.len());
        }
        lines.lines.extend(row_lines(row, width));
    }

    lines
}

/// Returns the row of the only call `event` describes, finished or not.
fn call_row(event: ActivityEvent) -> ToolRow {
    let mut timeline = TurnTimeline::default();
    timeline.observe_activity(event, 0);
    let calls = timeline.snapshot().calls;

    calls
        .latest
        .or_else(|| calls.running.into_iter().next())
        .expect("call row")
}

/// Paints the rows of call `row` at `width` columns.
fn painted_call(row: &ToolRow, width: u16) -> Vec<String> {
    painted_rows(&row_lines(row, usize::from(width)), width)
}

/// Paints `lines` into a `width`-column test terminal and returns its rows.
fn painted_rows(lines: &[Line<'static>], width: u16) -> Vec<String> {
    let height = u16::try_from(lines.len()).expect("row count");
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines.to_vec()), frame.area()))
        .expect("draw");
    let buffer = terminal.backend().buffer();

    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Returns the terminal column where `needle` starts in `row`.
fn column(row: &str, needle: &str) -> Option<usize> {
    row.find(needle)
        .map(|index| UnicodeWidthStr::width(&row[..index]))
}

#[test]
fn wide_panes_show_steps_then_totals_the_last_finished_call_and_the_running_one() {
    // Arrange
    let snapshot = design_timeline();

    // Act
    let lines = render(&snapshot, 170);
    let rows = painted_rows(&lines.lines, 170);

    // Assert
    assert_eq!(
        rows[0],
        format!(
            "• Reading the /model docs and the composer's picker stages.{}00:00",
            " ".repeat(106)
        )
    );
    assert!(rows[1].starts_with("  ✻ Thought 6s · Harness models already carry"));
    assert!(rows[2].ends_with("03:12"));
    assert!(rows[3].ends_with("08:02"));
    assert_eq!(
        rows[4],
        "  10 tools · 2 failed · 1 running · 3 commands · 2 edits · 3 reads · 1 search · 1 listing"
    );
    assert!(rows[5].starts_with("  ✗ Ran       cargo test -p agentty"));
    assert!(rows[5].ends_with("exit 101"));
    assert_eq!(column(&rows[5], "exit 101"), Some(110));
    assert_eq!(rows[6], "    │ running 58 tests");
    assert_eq!(rows[8], "    │ test result: FAILED. 56 passed; 2 failed");
    assert_eq!(
        rows[9],
        format!(
            "  {} Running   cargo test -p agentty --lib domain::composer",
            spinner_glyph(0)
        )
    );
    assert_eq!(rows.len(), 10);
    assert_eq!(lines.spinner_rows, [9]);
}

#[test]
fn medium_and_narrow_panes_shorten_targets_and_keep_rows_within_the_width() {
    // Arrange
    let snapshot = design_timeline();
    let edit = call_row(edit_event("crates/agentty/src/domain/composer.rs"));

    for width in [100_u16, 80, 40] {
        // Act
        let lines = render(&snapshot, usize::from(width));
        let rows = painted_rows(&lines.lines, width);

        // Assert
        assert!(
            lines
                .lines
                .iter()
                .all(|line| line.width() <= usize::from(width)),
            "{width}: {rows:#?}"
        );
        assert_eq!(
            rows[0].contains("00:00"),
            width >= 60,
            "{width}: {}",
            rows[0]
        );
    }
    let narrow = painted_rows(&render(&snapshot, 40).lines, 40);
    assert!(
        narrow
            .iter()
            .any(|row| row.starts_with("  10 tools") && row.ends_with("...")),
        "{narrow:#?}"
    );
    assert_eq!(
        painted_call(&edit, 80),
        ["  ✓ Edit      crates/agentty/src/domain/composer.rs                 +38 −4"]
    );
}

#[test]
fn rows_and_totals_use_semantic_styles() {
    // Arrange
    let snapshot = design_timeline();
    let edit = row_lines(&call_row(edit_event("a.rs")), 120);

    // Act
    let lines = render(&snapshot, 170).lines;

    // Assert
    let span = |lines: &[Line<'static>], row: usize, text: &str| {
        lines[row]
            .spans
            .iter()
            .find(|span| span.content.contains(text))
            .unwrap_or_else(|| unreachable!("span {text} in row {row}"))
            .clone()
    };
    assert_eq!(
        span(&lines, 0, "00:00").style.fg,
        Some(style::palette::text_subtle())
    );
    assert!(
        span(&lines, 1, "Thought")
            .style
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert_eq!(
        span(&lines, 4, "10 tools").style.fg,
        Some(style::palette::text_subtle())
    );
    assert_eq!(
        span(&lines, 4, "2 failed").style.fg,
        Some(style::palette::danger())
    );
    assert_eq!(
        span(&lines, 4, "1 running").style.fg,
        Some(style::palette::warning())
    );
    assert_eq!(
        span(&lines, 5, "✗").style.fg,
        Some(style::palette::danger())
    );
    assert_eq!(
        span(&lines, 5, "Ran").style.fg,
        Some(style::palette::danger())
    );
    assert!(
        span(&lines, 5, "Ran")
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(
        span(&lines, 5, "cargo test").style.fg,
        Some(style::palette::text())
    );
    assert_eq!(
        span(&lines, 5, "exit 101").style.fg,
        Some(style::palette::danger())
    );
    assert_eq!(
        span(&lines, 9, spinner_glyph(0)).style.fg,
        Some(style::palette::warning())
    );
    assert_eq!(
        span(&edit, 0, "✓").style.fg,
        Some(style::palette::success())
    );
    assert_eq!(
        span(&edit, 0, "a.rs").style.fg,
        Some(style::palette::text_muted())
    );
    assert_eq!(
        span(&edit, 0, "+38").style.fg,
        Some(style::palette::success())
    );
    assert_eq!(
        span(&edit, 0, "−4").style.fg,
        Some(style::palette::danger())
    );
}

#[test]
fn totals_name_tools_and_outcomes_in_singular_or_plural_and_skip_a_turn_without_calls() {
    // Arrange
    let stats = CallStats {
        interrupted: 1,
        total: 4,
        used: vec![("posthog/query".into(), 2)],
        web_searches: 1,
        ..CallStats::default()
    };
    let single = CallStats {
        commands: 1,
        total: 1,
        ..CallStats::default()
    };

    // Act
    let line = stats_line(&stats, 120).map(|line| line.to_string());
    let single_line = stats_line(&single, 120).map(|line| line.to_string());

    // Assert
    assert_eq!(
        line.as_deref(),
        Some("  4 tools · 1 interrupted · 1 web search · posthog/query ×2")
    );
    assert_eq!(single_line.as_deref(), Some("  1 tool · 1 command"));
    assert_eq!(stats_line(&CallStats::default(), 120), None);
}
#[test]
fn long_reasoning_takes_two_rows_and_open_reasoning_reads_thinking() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    for (id, text) in [(0, "word ".repeat(80)), (1, "**Only a title**".to_string())] {
        timeline.observe_output(
            OutputEvent::Started {
                id,
                kind: OutputKind::Reasoning,
            },
            0,
        );
        timeline.observe_activity(activity_event(&format!("call-{id}")), 0);
        timeline.observe_output(OutputEvent::Delta { id, text }, 0);
    }
    let snapshot = timeline.snapshot();

    // Act
    let first = render_step(&snapshot, 0, 60);
    let second = render_step(&snapshot, 1, 60);

    // Assert
    assert!(first[0].starts_with("  ✻ Thinking · word word"));
    assert!(first[1].starts_with("               word"));
    assert!(first[1].ends_with("..."));
    assert_eq!(first.len(), 2);
    assert_eq!(second[0], "  ✻ Thinking · Only a title");
}

/// Renders step `index`'s header at `width` columns as plain rows.
fn render_step(snapshot: &TimelineSnapshot, index: usize, width: u16) -> Vec<String> {
    let lines = header_lines(&snapshot.steps[index], usize::from(width), None);

    painted_rows(&lines, width)
}

#[test]
fn answers_render_as_markdown_and_narration_wraps_with_a_hanging_indent() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    for (id, text) in [
        (
            0,
            "First paragraph that is long enough to wrap onto a second row.\n\nSecond",
        ),
        (1, ""),
    ] {
        timeline.observe_output(
            OutputEvent::Started {
                id,
                kind: OutputKind::Message,
            },
            0,
        );
        timeline.observe_output(
            OutputEvent::Completed {
                id,
                text: text.into(),
            },
            0,
        );
    }
    timeline.observe_output(
        OutputEvent::Answer {
            replaces: None,
            text: "## Done\n\nAll **good**.".into(),
        },
        0,
    );
    let snapshot = timeline.snapshot();
    let cache = markdown::MarkdownRenderCache::default();

    // Act
    let narration = render_step(&snapshot, 0, 40);
    let empty = header_lines(&snapshot.steps[1], 40, None);
    let answer = header_lines(&snapshot.steps[2], 40, None);
    let cached_answer = header_lines(&snapshot.steps[2], 40, Some(&cache));

    // Assert
    assert_eq!(narration[0], "• First paragraph that is long enough to");
    assert_eq!(narration[1], "  wrap onto a second row.");
    assert_eq!(narration[2], "  Second");
    assert_eq!(empty, []);
    assert!(answer.iter().any(|line| line.to_string().contains("Done")));
    assert_eq!(answer, cached_answer);
}

#[test]
fn interrupted_signaled_failed_and_preparing_rows_and_hidden_steps_render() {
    // Arrange
    let mut preparing = TurnTimeline::default();
    preparing.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::ToolCall,
        },
        0,
    );
    let preparing = preparing.snapshot().calls.running.remove(0);
    let rows = [
        ActivityEvent {
            status: ActivityStatus::Interrupted,
            ..activity_event("interrupted")
        },
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                signal: Some(9),
                ..ActivityFacts::default()
            }),
            status: ActivityStatus::Completed,
            ..activity_event("signal")
        },
        ActivityEvent {
            status: ActivityStatus::Failed,
            ..activity_event("failed")
        },
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                failure: Some(ActivityFailure::Rejected),
                target: Some("a.rs".into()),
                ..ActivityFacts::default()
            }),
            kind: ActivityKind::FileChange,
            status: ActivityStatus::Failed,
            ..activity_event("rejected")
        },
    ]
    .map(call_row);

    // Act
    let preparing = painted_call(&preparing, 80);
    let rows = rows.map(|row| painted_call(&row, 80).remove(0));

    // Assert
    assert!(preparing[0].starts_with(&format!("  {} Preparing a tool call", spinner_glyph(0))));
    assert!(rows[0].starts_with("  ! Ran"));
    assert!(rows[0].ends_with("interrupted"));
    assert!(rows[1].ends_with("signal 9"));
    assert!(rows[2].ends_with("failed"));
    assert!(rows[3].ends_with("rejected"));
    assert_eq!(hidden_steps_line(1).to_string(), "+1 earlier step");
    assert_eq!(hidden_steps_line(3).to_string(), "+3 earlier steps");
    assert_eq!(spinner_glyph(10), spinner_glyph(0));
}

#[test]
fn running_rows_spin_and_paths_keep_their_file_name_when_cut() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_activity(
        ActivityEvent {
            facts: Box::new(facts("src/a.rs")),
            kind: ActivityKind::Read,
            ..activity_event("read")
        },
        0,
    );
    timeline.observe_activity(
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                failure: Some(ActivityFailure::Rejected),
                ..facts("crates/agentty/src/a_file_name_longer_than_the_target_column.rs")
            }),
            kind: ActivityKind::FileChange,
            status: ActivityStatus::Failed,
            ..activity_event("edit")
        },
        0,
    );
    let snapshot = timeline.snapshot();

    // Act
    let lines = render(&snapshot, 60);
    let rows = painted_rows(&lines.lines, 60);

    // Assert
    assert_eq!(
        rows[0],
        "  2 tools · 1 failed · 1 running · 1 edit · 1 read"
    );
    assert!(
        rows[1].starts_with("  ✗ Edit      a_file_name_longer_than_the..."),
        "{rows:#?}"
    );
    assert!(rows[2].starts_with(&format!("  {} Read      src/a.rs", spinner_glyph(0))));
    assert_eq!(lines.spinner_rows, [2]);
}

#[test]
fn narration_styles_inline_code_like_a_rendered_answer() {
    // Arrange
    let _theme = style::scoped_active_theme(crate::domain::theme::ColorTheme::DarkHorizon);
    let mut timeline = TurnTimeline::default();
    message(&mut timeline, 0, "Reading the `/model` docs.", 0);
    let snapshot = timeline.snapshot();
    let answer_code = markdown::render_markdown("`/model`", 40)[0]
        .spans
        .iter()
        .find(|span| span.content == "/model")
        .map(|span| span.style.fg);

    // Act
    let lines = header_lines(&snapshot.steps[0], 60, None);

    // Assert
    let narration_code = lines[0]
        .spans
        .iter()
        .find(|span| span.content == "/model")
        .map(|span| span.style.fg);
    assert_eq!(narration_code, answer_code);
    assert_eq!(narration_code, Some(Some(style::palette::warning())));
}

#[test]
fn running_rows_time_their_call_in_the_warning_tone() {
    // Arrange, Act
    let elapsed = running_time(100_000, 112_400);
    let not_started = running_time(200_000, 112_400);

    // Assert
    assert_eq!(elapsed.content, "12s");
    assert_eq!(elapsed.style.fg, Some(style::palette::warning()));
    assert_eq!(not_started.content, "0s");
}

#[test]
fn hour_long_turns_show_hours_in_step_times() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    for (id, at) in [(0, 0), (1, 3_725_000)] {
        timeline.observe_output(
            OutputEvent::Started {
                id,
                kind: OutputKind::Message,
            },
            at,
        );
        timeline.observe_output(
            OutputEvent::Completed {
                id,
                text: "step".into(),
            },
            at,
        );
    }
    let snapshot = timeline.snapshot();

    // Act
    let rows = render_step(&snapshot, 1, 60);

    // Assert
    assert!(rows[0].ends_with("1:02:05"));
}
