//! Live usage readout beside the session timer, such as
//! `↑412k ↓3.1k · ctx ▰▰▱▱▱ 38% · $0.24 turn · $3.27 session`.
//!
//! While a session turn runs, the readout shows the turn's input and output
//! tokens, the latest context fill with its gauge, the turn's estimated
//! charge, and the session's; between turns it shows
//! `ctx 38% · $3.27 session`. Figures keep
//! the usage panel's marks: `≥` for a lower bound, `≤` for a charge whose
//! cached input was priced at the input price, `~` for both, and `≈` for an
//! API equivalent. A charge without a price reads `$?`, or `local` for a
//! session on a local model. As the width narrows, the session charge, the
//! tokens, the gauge cells (the percentage stays), and the turn charge give
//! way in that order; the readout hides when even the context does not fit.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::ui::style::palette;
use crate::ui::usage_format::{
    self, CostFigure, ReadoutContext, ReadoutFigures, ReadoutTurn, UsageFigure,
};

/// Separator between readout segments.
const SEGMENT_SEPARATOR: &str = " · ";

/// Readout spelling of a charge without a price.
const UNPRICED_TEXT: &str = "$?";

/// Readout spelling of a token count no request reported.
const UNREPORTED_TEXT: &str = "?";

/// Readout spelling of a charge on a local model, which has no marginal
/// charge.
const LOCAL_TEXT: &str = "local";

/// Live usage readout of one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageReadout {
    figures: ReadoutFigures,
    is_local: bool,
}

impl UsageReadout {
    /// Returns the readout of `figures`; `is_local` marks a session whose
    /// model runs on a local server, so an unpriced charge reads `local`.
    pub(crate) fn new(figures: ReadoutFigures, is_local: bool) -> Self {
        Self { figures, is_local }
    }

    /// Returns the readout fitted to `max_width` columns, dropping segments
    /// by priority, or `None` when even the context does not fit.
    pub(crate) fn line(&self, max_width: usize) -> Option<Line<'static>> {
        Narrowing::ALL
            .into_iter()
            .map(|narrowing| self.compose(narrowing))
            .find(|line| line.width() <= max_width)
            .filter(|line| line.width() > 0)
    }

    /// Joins the segments `narrowing` keeps with separators.
    fn compose(&self, narrowing: Narrowing) -> Line<'static> {
        let turn = self.figures.turn.as_ref();
        let turn_cost = turn
            .filter(|_| narrowing < Narrowing::ContextOnly)
            .and_then(|turn| self.turn_cost_spans(turn));
        // A local session reads `local` once rather than for both charges.
        let is_turn_cost_local = turn_cost.is_some()
            && self.is_local_cost(turn.map(|turn| &turn.cost))
            && self.is_local_cost(Some(&self.figures.session_cost));
        let show_gauge = turn.is_some() && narrowing < Narrowing::NoGauge;
        let segments = [
            turn.filter(|_| narrowing < Narrowing::NoTokens)
                .map(token_spans),
            self.figures
                .context
                .as_ref()
                .map(|context| context_spans(context, show_gauge)),
            turn_cost,
            if narrowing < Narrowing::NoSessionCost && !is_turn_cost_local {
                self.session_cost_spans()
            } else {
                None
            },
        ];
        let mut spans = Vec::new();
        for segment in segments.into_iter().flatten() {
            if !spans.is_empty() {
                spans.push(muted(SEGMENT_SEPARATOR));
            }
            spans.extend(segment);
        }

        Line::from(spans)
    }

    /// Returns the turn charge followed by `turn`, or by `API-eq` for the
    /// API equivalent of plan-billed usage.
    fn turn_cost_spans(&self, turn: &ReadoutTurn) -> Option<Vec<Span<'static>>> {
        let label = match &turn.cost {
            UsageFigure::Cost(CostFigure {
                is_api_equivalent: true,
                ..
            }) => " API-eq",
            _ => " turn",
        };

        self.cost_spans(&turn.cost, label)
    }

    /// Returns the session charge followed by `session`.
    fn session_cost_spans(&self) -> Option<Vec<Span<'static>>> {
        self.cost_spans(&self.figures.session_cost, " session")
    }

    /// Returns a charge and its label; `local` stands alone, and a charge
    /// the backend did not record shows nothing.
    fn cost_spans(&self, cost: &UsageFigure, label: &'static str) -> Option<Vec<Span<'static>>> {
        if self.is_local_cost(Some(cost)) {
            return Some(vec![Span::styled(LOCAL_TEXT, value_style())]);
        }
        let value = figure_span(cost)?;

        Some(vec![value, muted(label)])
    }

    /// Returns whether `cost` reads `local`: an unpriced charge of a session
    /// on a local model.
    fn is_local_cost(&self, cost: Option<&UsageFigure>) -> bool {
        self.is_local && matches!(cost, Some(UsageFigure::Unpriced))
    }
}

/// How far the readout has narrowed: each step drops one more segment, in
/// priority order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Narrowing {
    /// Every segment.
    Full,
    /// Without the session charge.
    NoSessionCost,
    /// Also without the turn's tokens.
    NoTokens,
    /// Also without the gauge cells; the percentage stays.
    NoGauge,
    /// Only the context fill.
    ContextOnly,
}

impl Narrowing {
    /// Every step, widest first.
    const ALL: [Self; 5] = [
        Self::Full,
        Self::NoSessionCost,
        Self::NoTokens,
        Self::NoGauge,
        Self::ContextOnly,
    ];
}

/// Returns `↑input ↓output` of the running turn.
fn token_spans(turn: &ReadoutTurn) -> Vec<Span<'static>> {
    let mut spans = vec![muted("↑")];
    spans.extend(figure_span(&turn.input));
    spans.push(muted(" ↓"));
    spans.extend(figure_span(&turn.output));

    spans
}

/// Returns `ctx`, the gauge when shown and the window is known, and the
/// share of the window or, without a window, the absolute prompt size.
fn context_spans(context: &ReadoutContext, show_gauge: bool) -> Vec<Span<'static>> {
    let mut spans = vec![muted("ctx ")];
    if let (true, Some(filled)) = (show_gauge, context.gauge_filled) {
        spans.push(Span::styled(
            format!("{} ", usage_format::context_gauge(filled)),
            Style::default().fg(usage_format::context_tone(context.percent)),
        ));
    }
    let fill = context
        .percent
        .map_or_else(|| context.tokens.clone(), |percent| format!("{percent}%"));
    spans.push(Span::styled(fill, value_style()));

    spans
}

/// Returns one figure in the readout's spelling, or `None` for a figure the
/// backend did not record.
fn figure_span(figure: &UsageFigure) -> Option<Span<'static>> {
    let span = match figure {
        UsageFigure::Known { .. } | UsageFigure::Cost(_) | UsageFigure::CostPair { .. } => {
            Span::styled(figure.panel_text(), value_style())
        }
        UsageFigure::Unreported => subtle(UNREPORTED_TEXT),
        UsageFigure::Unpriced => subtle(UNPRICED_TEXT),
        UsageFigure::NotRecorded => return None,
    };

    Some(span)
}

/// Returns the style of a known readout value.
fn value_style() -> Style {
    Style::default().fg(palette::text())
}

/// Returns a label or separator span.
fn muted(text: &'static str) -> Span<'static> {
    Span::styled(text, Style::default().fg(palette::text_muted()))
}

/// Returns an unknown value's span.
fn subtle(text: &'static str) -> Span<'static> {
    Span::styled(text, Style::default().fg(palette::text_subtle()))
}

#[cfg(test)]
#[path = "usage_readout_test.rs"]
mod tests;
