//! Rows of a running turn's live timeline: narration steps with their
//! reasoning and tool rows, laid out in columns.
//!
//! A step renders as a `•` narration row with its start time on the right,
//! a dim `✻ Thought` line, and one row per call or folded exploration: a
//! status glyph, a verb, the target, and a right-hand result column. Rows
//! of running calls carry a spinner glyph and leave room on their right for
//! the elapsed time, both of which the session output paints each frame.

use ag_tui_text::text_util;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::domain::turn_timeline::{
    ExploreRow, Narration, RowResult, RowState, Thought, TimelineRow, TimelineStep, ToolRow,
};
use crate::ui::{markdown, style};

/// Column of the status glyph in a row, which the spinner animation paints.
pub(crate) const ROW_GLYPH_COLUMN: u16 = 2;
/// Frames of the one-column spinner shown on running rows.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Width of the verb column.
const VERB_WIDTH: usize = 10;
/// Column where a row's target starts.
const TARGET_COLUMN: usize = 4 + VERB_WIDTH;
/// Width reserved for a slow call's duration on the right.
const DURATION_WIDTH: usize = 6;
/// Distance of the result column from the right edge on wide panes.
const RESULT_FROM_RIGHT: usize = 60;
/// Narrowest pane whose results line up in one column; narrower panes
/// right-align each result so targets keep the room.
const RESULT_COLUMN_MIN_WIDTH: usize = 120;
/// Narrowest target kept before a long result is truncated.
const MIN_TARGET_WIDTH: usize = 20;
/// Narrowest pane that still shows step times.
const STEP_TIME_MIN_WIDTH: usize = 60;
/// Width reserved for a step time such as `  12:34`.
const STEP_TIME_WIDTH: usize = 8;
/// Most rows one thought line takes.
const THOUGHT_ROW_LIMIT: usize = 2;

/// Returns the row that counts steps left out above the timeline.
pub(crate) fn hidden_steps_line(hidden_steps: usize) -> Line<'static> {
    let noun = if hidden_steps == 1 { "step" } else { "steps" };

    Line::styled(
        format!("+{hidden_steps} earlier {noun}"),
        Style::default().fg(style::palette::text_subtle()),
    )
}

/// Renders a step's narration and thought at `width` columns.
pub(crate) fn header_lines(
    step: &TimelineStep,
    width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(narration) = &step.narration {
        append_narration(
            &mut lines,
            narration,
            step.at_seconds,
            width,
            markdown_render_cache,
        );
    }
    if let Some(thought) = &step.thought {
        append_thought(&mut lines, thought, width);
    }

    lines
}

/// Renders one row at `width` columns: its line, then a failed call's last
/// output lines. A running row's first line carries the spinner glyph.
pub(crate) fn row_lines(row: &TimelineRow, width: usize) -> Vec<Line<'static>> {
    match row {
        TimelineRow::Explored(explore) => vec![explore_line(explore, width)],
        TimelineRow::Tool(tool) => {
            let mut lines = vec![tool_line(tool, width)];
            append_failure_tail(&mut lines, &tool.tail, width);

            lines
        }
    }
}

/// Returns the elapsed time a running row shows on its right, such as
/// `12s`, for a call that started at `since` Unix milliseconds, with its
/// style.
pub(crate) fn running_time(since: u64, now_millis: u128) -> Span<'static> {
    let elapsed_millis = now_millis.saturating_sub(u128::from(since));
    let elapsed_seconds = i64::try_from(elapsed_millis / 1000).unwrap_or(i64::MAX);

    Span::styled(
        text_util::format_duration_compact(elapsed_seconds),
        Style::default().fg(style::palette::warning()),
    )
}

/// Returns the spinner glyph for animation frame `frame`.
pub(crate) fn spinner_glyph(frame: usize) -> &'static str {
    SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
}

/// Appends a narration message: a `•` row wrapped with a hanging indent
/// and the step time on its first row, or the validated answer as
/// Markdown.
fn append_narration(
    lines: &mut Vec<Line<'static>>,
    narration: &Narration,
    at_seconds: u64,
    width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    let text = narration.text.trim();
    if text.is_empty() {
        return;
    }
    if narration.is_answer {
        let rendered = match markdown_render_cache {
            Some(cache) => cache.render(text, width),
            None => markdown::render_markdown(text, width).into(),
        };
        lines.extend(rendered.iter().filter(|line| line.width() > 0).cloned());

        return;
    }
    let time_width = if width >= STEP_TIME_MIN_WIDTH {
        STEP_TIME_WIDTH
    } else {
        0
    };
    let text_width = width.saturating_sub(2 + time_width).max(1);
    let text_style = Style::default().fg(style::palette::text());
    let mut is_first = true;
    for paragraph in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let spans = markdown::themed_inline_spans(paragraph, text_style);
        for wrapped in text_util::wrap_styled_line(spans, text_width) {
            let prefix = if is_first { "• " } else { "  " };
            let mut spans = vec![Span::styled(
                prefix,
                Style::default().fg(style::palette::text_muted()),
            )];
            spans.extend(wrapped.spans);
            let mut line = Line::from(spans);
            if is_first && time_width > 0 {
                let time = format_step_time(at_seconds);
                pad_to(&mut line, width.saturating_sub(time.width()));
                line.spans.push(Span::styled(
                    time,
                    Style::default().fg(style::palette::text_subtle()),
                ));
            }
            lines.push(line);
            is_first = false;
        }
    }
}

/// Appends one dim italic `✻ Thought Ns · summary` line of at most two
/// rows.
fn append_thought(lines: &mut Vec<Line<'static>>, thought: &Thought, width: usize) {
    let header = match thought.seconds {
        Some(seconds) => format!("  ✻ Thought {seconds}s · "),
        None => "  ✻ Thinking · ".to_string(),
    };
    let header_width = header.width();
    let summary_width = width.saturating_sub(header_width).max(1);
    let summary_style = Style::default()
        .fg(style::palette::text_muted())
        .add_modifier(Modifier::ITALIC);
    let mut rows = text_util::wrap_styled_line(
        vec![Span::styled(thought.summary.clone(), summary_style)],
        summary_width,
    );
    if rows.len() > THOUGHT_ROW_LIMIT {
        rows.truncate(THOUGHT_ROW_LIMIT);
        let last = &mut rows[THOUGHT_ROW_LIMIT - 1];
        let mut spans = std::mem::take(&mut last.spans);
        spans.push(Span::styled(" ".repeat(summary_width), summary_style));
        last.spans = text_util::truncate_spans_with_ellipsis(spans, summary_width);
    }
    for (index, row) in rows.into_iter().enumerate() {
        let lead = if index == 0 {
            Span::styled(
                header.clone(),
                Style::default()
                    .fg(style::palette::text_subtle())
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            Span::raw(" ".repeat(header_width))
        };
        let mut spans = vec![lead];
        spans.extend(row.spans);
        lines.push(Line::from(spans));
    }
}

/// Returns a folded exploration row.
fn explore_line(explore: &ExploreRow, width: usize) -> Line<'static> {
    let target = explore
        .parts
        .iter()
        .map(|part| format!("{} {}", part.label, part.items.join(", ")))
        .collect::<Vec<_>>()
        .join(" · ");
    let noun = if explore.calls == 1 { "call" } else { "calls" };
    let result = vec![Span::styled(
        format!("{} {noun}", explore.calls),
        Style::default().fg(style::palette::text_subtle()),
    )];
    let (glyph, verb) = if explore.running_since.is_some() {
        (running_glyph(), "Exploring")
    } else {
        (success_glyph(), "Explored")
    };

    row_line(RowParts {
        duration: None,
        glyph,
        result,
        target: Span::styled(target, Style::default().fg(style::palette::text_muted())),
        target_is_path: false,
        verb: Span::styled(verb, verb_style(style::palette::text())),
        width,
    })
}

/// Returns one call's row.
fn tool_line(tool: &ToolRow, width: usize) -> Line<'static> {
    let (glyph, verb_color) = match tool.state {
        RowState::Running => (running_glyph(), style::palette::text()),
        RowState::Succeeded => (success_glyph(), style::palette::text()),
        RowState::Failed => (
            Span::styled("✗", Style::default().fg(style::palette::danger())),
            style::palette::danger(),
        ),
        RowState::Interrupted => (
            Span::styled("!", Style::default().fg(style::palette::warning())),
            style::palette::warning(),
        ),
    };
    let target_color = if tool.target_is_command {
        style::palette::text()
    } else {
        style::palette::text_muted()
    };
    let mut result = result_spans(&tool.result);
    if let Some(range) = &tool.read_first {
        if !result.is_empty() {
            result.push(Span::raw("   "));
        }
        let read = if range.is_empty() {
            "read first".to_string()
        } else {
            format!("read {range} first")
        };
        result.push(Span::styled(
            read,
            Style::default().fg(style::palette::text_subtle()),
        ));
    }

    row_line(RowParts {
        duration: tool.duration_seconds,
        glyph,
        result,
        target: Span::styled(tool.target.clone(), Style::default().fg(target_color)),
        target_is_path: !tool.target_is_command,
        verb: Span::styled(tool.verb, verb_style(verb_color)),
        width,
    })
}

/// Returns the spans of a row's result.
fn result_spans(result: &RowResult) -> Vec<Span<'static>> {
    let success = Style::default().fg(style::palette::success());
    let danger = Style::default().fg(style::palette::danger());
    match result {
        RowResult::DiffStat { added, removed } => vec![
            Span::styled(format!("+{added}"), success),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), danger),
        ],
        RowResult::Exit(0) => vec![Span::styled("exit 0", success)],
        RowResult::Exit(code) => vec![Span::styled(format!("exit {code}"), danger)],
        RowResult::Signal(signal) => vec![Span::styled(format!("signal {signal}"), danger)],
        RowResult::Failed {
            label,
            reason: Some(reason),
        } => vec![Span::styled(format!("{label} · {reason}"), danger)],
        RowResult::Failed {
            label,
            reason: None,
        } => vec![Span::styled(*label, danger)],
        RowResult::Interrupted => vec![Span::styled(
            "interrupted",
            Style::default().fg(style::palette::warning()),
        )],
        RowResult::None => Vec::new(),
    }
}

/// Appends a failed call's last output lines under its row.
fn append_failure_tail(lines: &mut Vec<Line<'static>>, tail: &[String], width: usize) {
    for line in tail {
        let text_width = width.saturating_sub(6);
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("│ ", Style::default().fg(style::palette::danger())),
            Span::styled(
                text_util::truncate_with_ellipsis(line, text_width),
                Style::default().fg(style::palette::text_muted()),
            ),
        ]));
    }
}

/// Pieces of one column-aligned row.
struct RowParts {
    duration: Option<u64>,
    glyph: Span<'static>,
    result: Vec<Span<'static>>,
    target: Span<'static>,
    /// Whether the target is a path, whose file name survives truncation.
    target_is_path: bool,
    verb: Span<'static>,
    width: usize,
}

/// Lays out a row: glyph, verb, target, result column, and duration.
fn row_line(parts: RowParts) -> Line<'static> {
    let RowParts {
        duration,
        glyph,
        result,
        target,
        target_is_path,
        verb,
        width,
    } = parts;
    let result_width = Line::from(result.clone()).width();
    let content_end = width.saturating_sub(DURATION_WIDTH);
    let right_aligned = content_end.saturating_sub(result_width);
    let result_column = if result_width == 0 {
        content_end
    } else if width >= RESULT_COLUMN_MIN_WIDTH {
        width.saturating_sub(RESULT_FROM_RIGHT).min(right_aligned)
    } else {
        right_aligned
    }
    .max(TARGET_COLUMN + MIN_TARGET_WIDTH + 2);
    let target_width = result_column.saturating_sub(TARGET_COLUMN + 2);
    let target_text = if target_is_path {
        truncate_path(&target.content, target_width)
    } else {
        text_util::truncate_with_ellipsis(&target.content, target_width)
    };

    let mut line = Line::from(vec![Span::raw("  "), glyph, Span::raw(" ")]);
    let verb_text = text_util::truncate_with_ellipsis(&verb.content, VERB_WIDTH - 1);
    line.spans.push(Span::styled(verb_text, verb.style));
    pad_to(&mut line, TARGET_COLUMN);
    line.spans.push(Span::styled(target_text, target.style));
    if result_width > 0 {
        pad_to(&mut line, result_column);
        let room = content_end.saturating_sub(result_column);
        line.spans
            .extend(text_util::truncate_spans_with_ellipsis(result, room));
    }
    if let Some(seconds) = duration {
        let label = text_util::format_duration_compact(i64::try_from(seconds).unwrap_or(i64::MAX));
        pad_to(&mut line, width.saturating_sub(label.width()));
        line.spans.push(Span::styled(
            label,
            Style::default().fg(style::palette::text_subtle()),
        ));
    }

    line
}

/// Cuts a path to `width` columns, keeping its file name and eliding the
/// middle.
pub(crate) fn truncate_path(path: &str, width: usize) -> String {
    if path.width() <= width {
        return path.to_string();
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let name_width = name.width();
    if name_width + 2 > width {
        return text_util::truncate_with_ellipsis(name, width);
    }
    let head_width = width - name_width - 1;
    let mut head = String::new();
    for character in path.chars() {
        let mut candidate = head.clone();
        candidate.push(character);
        if candidate.width() > head_width {
            break;
        }
        head = candidate;
    }

    format!("{head}…{name}")
}

/// Pads `line` with spaces up to `column`.
fn pad_to(line: &mut Line<'static>, column: usize) {
    let padding = column.saturating_sub(line.width());
    line.spans.push(Span::raw(" ".repeat(padding)));
}

/// Formats seconds since the turn started as `MM:SS`, or `H:MM:SS` after
/// an hour.
fn format_step_time(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Returns the glyph of a running row, which the session output animates.
fn running_glyph() -> Span<'static> {
    Span::styled(
        spinner_glyph(0),
        Style::default().fg(style::palette::warning()),
    )
}

/// Returns the glyph of a successful row.
fn success_glyph() -> Span<'static> {
    Span::styled("✓", Style::default().fg(style::palette::success()))
}

/// Returns the bold style of a row's verb in `color`.
fn verb_style(color: ratatui::style::Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
#[path = "turn_timeline_test.rs"]
mod tests;
