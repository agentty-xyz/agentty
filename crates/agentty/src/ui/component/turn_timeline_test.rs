//! Live-timeline row rendering tests at narrow, medium, and wide panes.

use std::time::Duration;

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityFailure, ActivityKind, ActivityStatus, DiffStat,
    LineRange, OutputEvent, OutputKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::{header_lines, hidden_steps_line, row_lines, running_time, spinner_glyph};
use crate::domain::turn_timeline::{TimelineSnapshot, TimelineStep, TurnTimeline};
use crate::test_support::activity_event;
use crate::ui::{markdown, style};

/// Builds the timeline the design shows: narration with reasoning, folded
/// explorations, a rejected and a successful edit with its read, a slow
/// command, a failed piped command with its output, and a running command.
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
            facts: Box::new(ActivityFacts {
                lines: Some(LineRange {
                    first: 1129,
                    last: Some(1228),
                }),
                ..facts(path)
            }),
            kind: ActivityKind::Read,
            status: ActivityStatus::Completed,
            ..activity_event("r3")
        },
        194_000,
    );
    timeline.observe_activity(
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
        },
        195_000,
    );
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

/// Renders the timeline at `width` columns.
/// Rendered rows of a timeline part, with the indices of running rows.
#[derive(Default)]
struct TimelineLines {
    lines: Vec<Line<'static>>,
    spinner_rows: Vec<usize>,
}

impl TimelineLines {
    /// Appends `other`, shifting its running rows.
    fn append(&mut self, other: &Self) {
        let offset = self.lines.len();
        self.lines.extend(other.lines.iter().cloned());
        self.spinner_rows
            .extend(other.spinner_rows.iter().map(|row| row + offset));
    }
}

/// Renders one step the way the session output assembles it: its header,
/// then each row.
fn step_lines(
    step: &TimelineStep,
    width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> TimelineLines {
    let mut lines = TimelineLines {
        lines: header_lines(step, width, markdown_render_cache),
        spinner_rows: Vec::new(),
    };
    for row in &step.rows {
        if row.running_since().is_some() {
            lines.spinner_rows.push(lines.lines.len());
        }
        lines.lines.extend(row_lines(row, width));
    }

    lines
}

fn render(snapshot: &TimelineSnapshot, width: usize) -> TimelineLines {
    let mut lines = TimelineLines::default();
    for step in &snapshot.steps {
        lines.append(&step_lines(step, width, None));
    }

    lines
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
fn wide_panes_align_results_in_a_column_with_times_on_the_right() {
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
    assert!(rows[2].starts_with(
        "  ✓ Explored  read model.rs, composer.rs · search \"Choosing a Model\" · list ."
    ));
    assert_eq!(column(&rows[2], "4 calls"), Some(110));
    assert!(rows[3].ends_with("03:12"));
    assert!(rows[4].starts_with("  ✗ Edit      crates/agentty/src/domain/composer.rs"));
    assert!(rows[4].ends_with("rejected · hunk 1 context mismatch at line 1216"));
    assert_eq!(rows[5], "    │ expected: fn model_rows");
    assert!(rows[6].ends_with("+38 −4   read :1129-1228 first"));
    assert_eq!(column(&rows[6], "+38"), Some(110));
    assert!(rows[7].ends_with("08:02"));
    assert!(rows[8].starts_with("  ✓ Ran       cargo check -p agentty --tests"));
    assert_eq!(column(&rows[8], "exit 0"), Some(110));
    assert!(rows[8].ends_with("36s"));
    assert!(rows[9].contains("2>&1 | tail -20"));
    assert!(rows[9].ends_with("exit 101"));
    assert_eq!(rows[10], "    │ running 58 tests");
    assert_eq!(rows[12], "    │ test result: FAILED. 56 passed; 2 failed");
    assert_eq!(
        rows[13],
        format!(
            "  {} Running   cargo test -p agentty --lib domain::composer",
            spinner_glyph(0)
        )
    );
    assert_eq!(lines.spinner_rows, [13]);
}

#[test]
fn medium_and_narrow_panes_shorten_targets_and_keep_rows_within_the_width() {
    // Arrange
    let snapshot = design_timeline();

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
        let edit = rows
            .iter()
            .find(|row| row.starts_with("  ✓ Edit"))
            .expect("edit row");
        if width >= 80 {
            assert!(edit.contains("composer.rs"), "{width}: {edit}");
            assert!(edit.contains("+38 −4"), "{width}: {edit}");
        }
        assert_eq!(
            rows[0].contains("00:00"),
            width >= 60,
            "{width}: {}",
            rows[0]
        );
    }
    let rows = painted_rows(&render(&snapshot, 80).lines, 80);
    assert!(
        rows.iter().any(|row| row
            == "  ✓ Edit      crates/agentty/s…composer.rs  +38 −4   read :1129-1228 first"),
        "{rows:#?}"
    );
}

#[test]
fn rows_use_semantic_styles() {
    // Arrange
    let snapshot = design_timeline();

    // Act
    let lines = render(&snapshot, 170).lines;

    // Assert
    let span = |row: usize, text: &str| {
        lines[row]
            .spans
            .iter()
            .find(|span| span.content.contains(text))
            .unwrap_or_else(|| unreachable!("span {text} in row {row}"))
            .clone()
    };
    assert_eq!(
        span(0, "00:00").style.fg,
        Some(style::palette::text_subtle())
    );
    assert!(
        span(1, "Thought")
            .style
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert_eq!(span(2, "✓").style.fg, Some(style::palette::success()));
    assert!(
        span(2, "Explored")
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(span(4, "✗").style.fg, Some(style::palette::danger()));
    assert_eq!(span(4, "Edit").style.fg, Some(style::palette::danger()));
    assert_eq!(span(4, "rejected").style.fg, Some(style::palette::danger()));
    assert_eq!(
        span(4, "crates/").style.fg,
        Some(style::palette::text_muted())
    );
    assert_eq!(span(6, "+38").style.fg, Some(style::palette::success()));
    assert_eq!(span(6, "−4").style.fg, Some(style::palette::danger()));
    assert_eq!(
        span(8, "cargo check").style.fg,
        Some(style::palette::text())
    );
    assert_eq!(span(8, "exit 0").style.fg, Some(style::palette::success()));
    assert_eq!(
        span(13, spinner_glyph(0)).style.fg,
        Some(style::palette::warning())
    );
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
    assert!(first[2].contains("Running"));
    assert_eq!(second[0], "  ✻ Thinking · Only a title");
}

/// Renders step `index` at `width` columns as plain rows.
fn render_step(snapshot: &TimelineSnapshot, index: usize, width: u16) -> Vec<String> {
    let lines = step_lines(&snapshot.steps[index], usize::from(width), None).lines;

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
    let empty = step_lines(&snapshot.steps[1], 40, None).lines;
    let answer = step_lines(&snapshot.steps[2], 40, None).lines;
    let cached_answer = step_lines(&snapshot.steps[2], 40, Some(&cache)).lines;

    // Assert
    assert_eq!(narration[0], "• First paragraph that is long enough to");
    assert_eq!(narration[1], "  wrap onto a second row.");
    assert_eq!(narration[2], "  Second");
    assert_eq!(empty, []);
    assert!(answer.iter().any(|line| line.to_string().contains("Done")));
    assert_eq!(answer, cached_answer);
}

#[test]
fn interrupted_signaled_and_preparing_rows_and_hidden_steps_render() {
    // Arrange
    let mut timeline = TurnTimeline::default();
    timeline.observe_output(
        OutputEvent::Started {
            id: 0,
            kind: OutputKind::ToolCall,
        },
        0,
    );
    for event in [
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
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                lines: None,
                target: Some("b.rs".into()),
                ..ActivityFacts::default()
            }),
            kind: ActivityKind::Read,
            status: ActivityStatus::Completed,
            ..activity_event("read")
        },
        ActivityEvent {
            facts: Box::new(ActivityFacts {
                target: Some("b.rs".into()),
                ..ActivityFacts::default()
            }),
            kind: ActivityKind::FileChange,
            status: ActivityStatus::Completed,
            ..activity_event("edit")
        },
    ] {
        timeline.observe_activity(event, 0);
    }
    let snapshot = timeline.snapshot();

    // Act
    let rows = render_step(&snapshot, 0, 80);

    // Assert
    assert!(rows[0].starts_with(&format!("  {} Preparing a tool call", spinner_glyph(0))));
    assert!(rows[1].starts_with("  ! Ran"));
    assert!(rows[1].ends_with("interrupted"));
    assert!(rows[2].ends_with("signal 9"));
    assert!(rows[3].ends_with("failed"));
    assert!(rows[4].ends_with("rejected"));
    assert!(rows[5].ends_with("read first"));
    assert_eq!(hidden_steps_line(1).to_string(), "+1 earlier step");
    assert_eq!(hidden_steps_line(3).to_string(), "+3 earlier steps");
    assert_eq!(spinner_glyph(10), spinner_glyph(0));
}

#[test]
fn running_explorations_spin_and_paths_keep_their_file_name_when_cut() {
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
    let lines = step_lines(&snapshot.steps[0], 60, None);
    let rows = painted_rows(&lines.lines, 60);

    // Assert
    assert_eq!(lines.spinner_rows, [0]);
    assert!(rows[0].starts_with(&format!("  {} Exploring read a.rs", spinner_glyph(0))));
    assert!(
        rows[1].starts_with("  ✗ Edit      a_file_name_longer_than_the..."),
        "{rows:#?}"
    );
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
