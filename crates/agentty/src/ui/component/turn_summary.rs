//! Rows of a finished turn's persisted summary, laid out in columns above
//! its answer.
//!
//! The summary text is tab-separated (see
//! [`crate::domain::turn_summary`]): after its format marker, a muted
//! header, `Edited` and `Failed` sections whose rows align a glyph, a
//! target, a result, and a note within each section, `Explored` and `Used`
//! lines, and a full-width `─ Worked …` rule.

use ag_tui_text::text_util;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::turn_timeline::truncate_path;
use crate::domain::turn_summary::TurnSummary;
use crate::ui::style;

/// Width of the section heading column, such as `Explored  `.
const HEADING_WIDTH: usize = 10;
/// Width of a row's glyph and the space after it.
const GLYPH_WIDTH: usize = 2;
/// Spaces between a row's target, result, and note.
const COLUMN_GAP: usize = 2;
/// Narrowest target kept before a row's note is dropped.
const MIN_TARGET_WIDTH: usize = 16;

/// Returns whether persisted activity-summary text is a turn summary in the
/// format this version renders, rather than a legacy `Tools:` usage footer.
pub(crate) fn is_turn_summary(summary: &str) -> bool {
    summary.lines().next() == Some(TurnSummary::FORMAT_MARKER)
}

/// Renders persisted turn-summary text at `width` columns, leaving out its
/// format marker line.
pub(crate) fn turn_summary_lines(summary: &str, width: usize) -> Vec<Line<'static>> {
    let lines = summary.lines().skip(1).collect::<Vec<_>>();
    let Some((divider, rest)) = lines.split_last() else {
        return Vec::new();
    };
    let (header, body) = rest
        .split_first()
        .map_or((None, rest), |(header, body)| (Some(*header), body));
    let mut rendered = Vec::new();
    if let Some(header) = header {
        rendered.push(Line::from(Span::styled(
            text_util::truncate_with_ellipsis(header, width),
            Style::default().fg(style::palette::text_muted()),
        )));
        rendered.push(Line::default());
    }
    for (section, rows) in sections(body) {
        let layout = TableLayout::measure(&rows, width);
        for line in rows {
            match line.split('\t').collect::<Vec<_>>().as_slice() {
                [heading, glyph, target, result, note] => {
                    rendered.push(layout.row(
                        &RowColumns {
                            glyph,
                            heading,
                            note,
                            result,
                            target,
                        },
                        section,
                        width,
                    ));
                }
                [heading, text] => rendered.extend(label_lines(heading, text, width)),
                _ => rendered.push(Line::from(Span::styled(
                    text_util::truncate_with_ellipsis(line, width),
                    Style::default().fg(style::palette::text_muted()),
                ))),
            }
        }
    }
    if !body.is_empty() {
        rendered.push(Line::default());
    }
    rendered.extend(divider_lines(divider, width));

    rendered
}

/// Groups `body`'s rows into sections: each row with a heading starts one,
/// and the rows below it with an empty heading join it.
fn sections<'a>(body: &[&'a str]) -> Vec<(&'a str, Vec<&'a str>)> {
    let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in body {
        let heading = line.split('\t').next().unwrap_or_default();
        match sections.last_mut() {
            Some((_, rows)) if heading.is_empty() => rows.push(line),
            _ => sections.push((heading, vec![line])),
        }
    }

    sections
}

/// Columns of one section row.
struct RowColumns<'a> {
    glyph: &'a str,
    heading: &'a str,
    note: &'a str,
    result: &'a str,
    target: &'a str,
}

/// Column widths shared by the rows of one section.
struct TableLayout {
    /// Width of the note column, or zero when notes are dropped.
    note: usize,
    result: usize,
    target: usize,
}

impl TableLayout {
    /// Fits the widest target, result, and note of a section's rows into
    /// `width`, narrowing targets first and dropping notes when targets
    /// would get too narrow.
    fn measure(body: &[&str], width: usize) -> Self {
        let mut widest = [0; 3];
        for line in body {
            if let [_, _, target, result, note] = line.split('\t').collect::<Vec<_>>().as_slice() {
                for (widest, column) in widest.iter_mut().zip([target, result, note]) {
                    *widest = (*widest).max(column.width());
                }
            }
        }
        let [target_max, result_width, note_max] = widest;
        let available = width.saturating_sub(HEADING_WIDTH + GLYPH_WIDTH);
        let columns = |note_width: usize| {
            [result_width, note_width]
                .iter()
                .filter(|width| **width > 0)
                .map(|width| width + COLUMN_GAP)
                .sum::<usize>()
        };
        let mut note_width = note_max;
        let mut target_width = target_max.min(available.saturating_sub(columns(note_width)));
        if target_width < target_max.min(MIN_TARGET_WIDTH) {
            note_width = 0;
            target_width = target_max.min(available.saturating_sub(columns(0)));
        }

        Self {
            note: note_width,
            result: result_width,
            target: target_width.max(target_max.min(MIN_TARGET_WIDTH)),
        }
    }

    /// Lays out one section row of `section`.
    fn row(&self, columns: &RowColumns<'_>, section: &str, width: usize) -> Line<'static> {
        let glyph_color = if columns.glyph == TurnSummary::FAILURE_GLYPH {
            style::palette::danger()
        } else {
            style::palette::success()
        };
        let target = if section == TurnSummary::EDITED_HEADING {
            truncate_path(columns.target, self.target)
        } else {
            text_util::truncate_with_ellipsis(columns.target, self.target)
        };
        let mut line = Line::from(vec![
            heading_span(columns.heading),
            Span::styled(
                format!("{} ", columns.glyph),
                Style::default().fg(glyph_color),
            ),
            Span::styled(target, Style::default().fg(style::palette::text())),
        ]);
        let result_column = HEADING_WIDTH + GLYPH_WIDTH + self.target + COLUMN_GAP;
        if !columns.result.is_empty() {
            pad_to(&mut line, result_column);
            line.spans.extend(result_spans(columns.result, section));
        }
        if self.note > 0 && !columns.note.is_empty() {
            let note_column = if columns.result.is_empty() {
                result_column
            } else {
                result_column + self.result + COLUMN_GAP
            };
            pad_to(&mut line, note_column);
            let note_color = if columns.note.starts_with(TurnSummary::RECOVERY_PREFIX) {
                style::palette::success()
            } else {
                style::palette::text_subtle()
            };
            line.spans.push(Span::styled(
                columns.note.to_string(),
                Style::default().fg(note_color),
            ));
        }
        line.spans = text_util::truncate_spans_with_ellipsis(line.spans, width);

        line
    }
}

/// Returns a row's result: red in the `Failed` section, else green added
/// and red removed line counts.
fn result_spans(result: &str, section: &str) -> Vec<Span<'static>> {
    if section == TurnSummary::FAILED_HEADING {
        return vec![Span::styled(
            result.to_string(),
            Style::default().fg(style::palette::danger()),
        )];
    }
    let mut spans = Vec::new();
    for (index, word) in result.split(' ').enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let color = if word.starts_with('+') {
            style::palette::success()
        } else if word.starts_with('−') {
            style::palette::danger()
        } else {
            style::palette::text_subtle()
        };
        spans.push(Span::styled(word.to_string(), Style::default().fg(color)));
    }

    spans
}

/// Returns a heading followed by muted text, wrapped under the text's
/// column.
fn label_lines(heading: &str, text: &str, width: usize) -> Vec<Line<'static>> {
    let text_width = width.saturating_sub(HEADING_WIDTH).max(1);
    let wrapped = text_util::wrap_styled_line(
        vec![Span::styled(
            text.to_string(),
            Style::default().fg(style::palette::text_muted()),
        )],
        text_width,
    );

    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let lead = if index == 0 {
                heading_span(heading)
            } else {
                Span::raw(" ".repeat(HEADING_WIDTH))
            };
            let mut spans = vec![lead];
            spans.extend(row.spans);
            Line::from(text_util::truncate_spans_with_ellipsis(spans, width))
        })
        .collect()
}

/// Returns `heading` padded to the heading column in bold muted text.
fn heading_span(heading: &str) -> Span<'static> {
    let heading = text_util::truncate_with_ellipsis(heading, HEADING_WIDTH - 1);
    let padding = HEADING_WIDTH.saturating_sub(heading.width());

    Span::styled(
        format!("{heading}{}", " ".repeat(padding)),
        Style::default()
            .fg(style::palette::text_muted())
            .add_modifier(Modifier::BOLD),
    )
}

/// Returns the `─ Worked …` text wrapped to `width`, its last row followed
/// by a rule to the right edge when room is left.
fn divider_lines(divider: &str, width: usize) -> Vec<Line<'static>> {
    let mut rows = text_util::wrap_styled_line(
        vec![Span::styled(
            divider.to_string(),
            Style::default().fg(style::palette::text_subtle()),
        )],
        width.max(1),
    );
    let mut last = rows.pop().unwrap_or_default();
    let room = width.saturating_sub(last.width());
    let rule = if room >= 2 {
        format!(" {}", "─".repeat(room - 1))
    } else {
        String::new()
    };
    last.spans.push(Span::styled(
        rule,
        Style::default().fg(style::palette::border()),
    ));
    rows.push(last);

    rows
}

/// Pads `line` with spaces up to `column`.
fn pad_to(line: &mut Line<'static>, column: usize) {
    let padding = column.saturating_sub(line.width());
    line.spans.push(Span::raw(" ".repeat(padding)));
}

#[cfg(test)]
#[path = "turn_summary_test.rs"]
mod tests;
