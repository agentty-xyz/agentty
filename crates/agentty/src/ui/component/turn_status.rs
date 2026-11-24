//! Status line shown above the composer while a turn runs, such as
//! `⠹ Running cargo test 12s  ·  step 4  ·  turn 1m 5s`.
//!
//! The line names what the turn does now and for how long, the step count,
//! and the whole turn's time, which reads the same source as the composer's
//! `Timer`. A caller may pass a telemetry line for the slot on its right; as
//! the pane narrows, the slot, the turn time, the step count, and the
//! action timer give way in that order before the action is truncated. Key
//! hints stay in the footer, which knows what each key does in the current
//! mode.

use ag_tui_text::text_util;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::domain::turn_timeline::CurrentAction;
use crate::ui::component::turn_timeline;
use crate::ui::style;

/// Action shown before the turn reports what it does.
const DEFAULT_ACTION: &str = "Working";
/// Narrowest truncated action.
const MIN_ACTION_WIDTH: usize = 12;
/// Separator between the line's segments.
const SEPARATOR: &str = "  ·  ";
/// Columns the rest of the line keeps before the telemetry slot.
const SLOT_RESERVED_WIDTH: u16 = 80;
/// Widest telemetry slot.
const SLOT_MAX_WIDTH: u16 = 40;

/// Values one status line shows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TurnStatusInput<'a> {
    /// What the turn does now, or `None` before it reports anything.
    pub(crate) current: Option<&'a CurrentAction>,
    /// Frame time in Unix milliseconds, which times the current action.
    pub(crate) now_millis: u128,
    /// Animation frame of the spinner.
    pub(crate) spinner_frame: usize,
    /// Steps the turn took so far.
    pub(crate) step_count: usize,
    /// Seconds the turn has run, as the composer `Timer` counts them.
    pub(crate) turn_seconds: i64,
}

/// Returns the widest telemetry line the status line has room for at
/// `width` columns; `0` means a slot would not fit.
pub(crate) const fn telemetry_max_width(width: u16) -> u16 {
    let room = width.saturating_sub(SLOT_RESERVED_WIDTH);
    if room > SLOT_MAX_WIDTH {
        SLOT_MAX_WIDTH
    } else {
        room
    }
}

/// Builds the status line at `width` columns, with `telemetry` in the slot
/// on its right when it is no wider than [`telemetry_max_width`] and the
/// line has room; `None` leaves the slot empty.
pub(crate) fn turn_status_line(
    input: TurnStatusInput<'_>,
    telemetry: Option<Line<'static>>,
    width: u16,
) -> Line<'static> {
    let segments = StatusSegments::new(input, telemetry, width);

    segments.into_line(usize::from(width))
}

/// The status line's segments, before they are fitted to a width.
struct StatusSegments {
    /// The current action on one line, as the session's progress text
    /// shows it.
    action: String,
    /// Columns of `action` that name the action rather than its object.
    label_width: usize,
    spinner: &'static str,
    step: Option<String>,
    telemetry: Option<Line<'static>>,
    timer: Option<String>,
    turn: Option<String>,
}

impl StatusSegments {
    /// Formats every segment the input supports.
    fn new(input: TurnStatusInput<'_>, telemetry: Option<Line<'static>>, width: u16) -> Self {
        let timer = input.current.map(|current| {
            let elapsed_millis = input.now_millis.saturating_sub(u128::from(current.since));
            let elapsed_seconds = i64::try_from(elapsed_millis / 1000).unwrap_or(i64::MAX);

            text_util::format_duration_compact(elapsed_seconds)
        });
        let (action, label_width) = input.current.map_or_else(
            || (DEFAULT_ACTION.to_string(), DEFAULT_ACTION.width()),
            |current| (current.text.clone(), current.label_width),
        );

        Self {
            action,
            label_width,
            spinner: turn_timeline::spinner_glyph(input.spinner_frame),
            step: (input.step_count > 0).then(|| format!("step {}", input.step_count)),
            telemetry: telemetry.filter(|line| {
                line.width() > 0 && line.width() <= usize::from(telemetry_max_width(width))
            }),
            timer,
            turn: Some(format!(
                "turn {}",
                text_util::format_duration_compact(input.turn_seconds)
            )),
        }
    }

    /// Drops segments in priority order until the line fits `width`, then
    /// truncates the action.
    fn into_line(mut self, width: usize) -> Line<'static> {
        while self.width() > width {
            if self.telemetry.take().is_some()
                || self.turn.take().is_some()
                || self.step.take().is_some()
                || self.timer.take().is_some()
            {
                continue;
            }
            break;
        }
        let overflow = self.width().saturating_sub(width);
        let action_width = self
            .action
            .width()
            .saturating_sub(overflow)
            .max(MIN_ACTION_WIDTH);
        let action = text_util::truncate_with_ellipsis(&self.action, action_width);
        let warning = Style::default().fg(style::palette::warning());
        let muted = Style::default().fg(style::palette::text_muted());

        let mut line = Line::from(vec![Span::styled(format!("{} ", self.spinner), warning)]);
        let (label, detail) = split_at_width(&action, self.label_width);
        line.spans.push(Span::styled(label, warning));
        line.spans.push(Span::styled(
            detail,
            Style::default().fg(style::palette::text()),
        ));
        if let Some(timer) = self.timer.take() {
            line.spans.push(Span::styled(format!(" {timer}"), warning));
        }
        for segment in [self.step.take(), self.turn.take()].into_iter().flatten() {
            line.spans
                .push(Span::styled(format!("{SEPARATOR}{segment}"), muted));
        }
        if let Some(telemetry) = self.telemetry.take() {
            let gap = width
                .saturating_sub(line.width() + telemetry.width())
                .max(2);
            line.spans.push(Span::raw(" ".repeat(gap)));
            line.spans.extend(telemetry.spans);
        }

        line
    }

    /// Returns the line's width with its current segments.
    fn width(&self) -> usize {
        let left = 2
            + self.action.width()
            + self.timer.as_ref().map_or(0, |timer| timer.width() + 1)
            + [&self.step, &self.turn]
                .into_iter()
                .flatten()
                .map(|segment| SEPARATOR.width() + segment.width())
                .sum::<usize>();

        left + self.telemetry.as_ref().map_or(0, |line| line.width() + 2)
    }
}

/// Splits `text` after its first `width` columns.
fn split_at_width(text: &str, width: usize) -> (String, String) {
    let mut head_width = 0;
    let split = text
        .char_indices()
        .find(|(_, character)| {
            head_width += character.width().unwrap_or(0);

            head_width > width
        })
        .map_or(text.len(), |(index, _)| index);

    (text[..split].to_string(), text[split..].to_string())
}

#[cfg(test)]
#[path = "turn_status_test.rs"]
mod tests;
