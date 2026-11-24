//! Centered popup showing one session's token usage and estimated cost per
//! model and purpose.

use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::presentation::help_action::HelpAction;
use crate::ui::style::palette;
use crate::ui::usage_format::{
    self, ContextSummary, UsageColumn, UsageFigure, UsagePanelContent, UsageTableRow, context_tone,
};
use crate::ui::{Component, help_format, overlay};

/// Bottom-border shortcut that dismisses the overlay.
const CLOSE_ACTION: HelpAction = HelpAction::new("close", "Esc", "Close");

/// Bottom-border shortcut shown when the panel content overflows the popup.
const SCROLL_ACTION: HelpAction = HelpAction::new("scroll", "j/k", "Scroll");

const SCROLL_X_OFFSET: u16 = 0;

/// Columns added by the overlay frame borders and horizontal padding.
const HORIZONTAL_CHROME_WIDTH: usize = 6;

/// Spaces between table columns.
const COLUMN_GAP_WIDTH: usize = 2;

/// Narrowest model column kept before value columns are dropped.
const MODEL_MIN_WIDTH: usize = 14;

/// Narrowest content width the popup asks for, so notes stay readable.
const MIN_CONTENT_WIDTH: usize = 60;

/// Width of the summary labels, such as `Est. cost`.
const SUMMARY_LABEL_WIDTH: usize = 11;

/// Body shown before the session records any usage.
const EMPTY_USAGE_MESSAGE: &str = "No usage recorded for this session yet.";

/// Header of the model column.
const MODEL_HEADER: &str = "Model";

/// Centered popup overlay with the per-session usage panel.
///
/// When the content is taller than the popup, it scrolls and the bottom
/// border shows the scroll keys.
pub(crate) struct UsageOverlay<'a> {
    content: &'a UsagePanelContent,
    scroll_offset: u16,
    session_title: &'a str,
}

impl<'a> UsageOverlay<'a> {
    /// Creates a usage overlay for `content` of the session titled
    /// `session_title`.
    pub(crate) fn new(content: &'a UsagePanelContent, session_title: &'a str) -> Self {
        Self {
            content,
            scroll_offset: 0,
            session_title,
        }
    }

    /// Sets the vertical scroll offset, clamped to the content when drawn.
    #[must_use]
    pub(crate) fn scroll_offset(mut self, offset: u16) -> Self {
        self.scroll_offset = offset;
        self
    }

    /// Returns the bottom-border hint, adding the scroll keys only when the
    /// content overflows the popup.
    fn bottom_hint(is_overflowing: bool) -> Line<'static> {
        let actions: &[HelpAction] = if is_overflowing {
            &[SCROLL_ACTION, CLOSE_ACTION]
        } else {
            &[CLOSE_ACTION]
        };
        let mut hint = help_format::footer_line(actions);
        hint.spans.insert(0, Span::raw(" "));
        hint.spans.push(Span::raw(" "));

        hint
    }
}

impl Component for UsageOverlay<'_> {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let layout = PopupLayout::new(self.content, area);
        let mut title = format!("Usage · {}", self.session_title);
        for login in &self.content.logins {
            title.push_str(" · ");
            title.push_str(usage_format::login_label(*login));
        }
        let title =
            text_util::truncate_with_ellipsis(&title, layout.content_width.saturating_sub(2));

        overlay::clear_popup_area(frame, layout.popup_area);
        let block = overlay::overlay_block(&title, palette::accent())
            .title_bottom(Self::bottom_hint(layout.max_scroll_offset() > 0).right_aligned());
        let scroll_offset = self.scroll_offset.min(layout.max_scroll_offset());
        let paragraph = Paragraph::new(layout.lines)
            .block(block)
            .scroll((scroll_offset, SCROLL_X_OFFSET));
        frame.render_widget(paragraph, layout.popup_area);
    }
}

/// Returns the largest scroll offset of `content` drawn in `area`: how many
/// lines it overflows the popup by, or zero when it fits.
///
/// Key handling bounds scrolling with the same layout the overlay draws.
pub(crate) fn max_scroll_offset(content: &UsagePanelContent, area: Rect) -> u16 {
    PopupLayout::new(content, area).max_scroll_offset()
}

/// Popup size and body lines of the panel for one drawing area.
struct PopupLayout {
    content_width: usize,
    lines: Vec<Line<'static>>,
    popup_area: Rect,
    required_height: u16,
}

impl PopupLayout {
    /// Fits `content` into `area`.
    fn new(content: &UsagePanelContent, area: Rect) -> Self {
        let natural_width = TableLayout::natural_width(content).max(MIN_CONTENT_WIDTH);
        let popup_width = u16::try_from(natural_width + HORIZONTAL_CHROME_WIDTH)
            .unwrap_or(u16::MAX)
            .min(area.width);
        let content_width = overlay::overlay_content_width(popup_width);
        let lines = body_lines(content, content_width);
        let required_height = overlay::overlay_required_height(lines.len());
        let popup_area = overlay::centered_popup_area(area, 0, 0, popup_width, required_height);

        Self {
            content_width,
            lines,
            popup_area,
            required_height,
        }
    }

    /// Returns how many lines the content overflows the popup by.
    fn max_scroll_offset(&self) -> u16 {
        self.required_height.saturating_sub(self.popup_area.height)
    }
}

/// Builds the panel body for `content_width` columns.
fn body_lines(content: &UsagePanelContent, content_width: usize) -> Vec<Line<'static>> {
    let mut lines = summary_lines(content, content_width);
    lines.push(Line::default());
    if content.rows.is_empty() {
        lines.push(muted(EMPTY_USAGE_MESSAGE.to_string()).into());

        return lines;
    }
    let table = TableLayout::new(content, content_width);
    lines.push(table.header_line());
    lines.extend(content.rows.iter().map(|row| table.row_line(row, false)));
    lines.push(Line::default());
    lines.push(table.row_line(&content.total, true));
    // Every recorded row carries a cost figure, so at least one note
    // explains it.
    lines.push(Line::default());
    for note in &content.notes {
        // Inexact figures draw attention; the estimate disclaimer stays quiet.
        let (mark_tone, text_tone) = if note.is_estimate {
            (palette::text_muted(), palette::text_muted())
        } else {
            (palette::warning(), palette::text())
        };
        let spans = vec![
            Span::styled(
                note.mark.to_string(),
                Style::default().fg(mark_tone).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {}", note.text), Style::default().fg(text_tone)),
        ];
        lines.extend(text_util::wrap_styled_line(spans, content_width));
    }

    lines
}

/// Builds the cost, context, and cache summary lines.
fn summary_lines(content: &UsagePanelContent, content_width: usize) -> Vec<Line<'static>> {
    let summary = &content.summary;
    let cost_spans = vec![
        summary_label("Est. cost"),
        figure_span(&summary.cost),
        muted(format!(" {}", summary.cost_explanation)),
        muted("   Tokens ".to_string()),
        figure_span(&summary.tokens),
        muted("   Turns ".to_string()),
        Span::styled(summary.turns.clone(), text()),
        muted("   Requests ".to_string()),
        figure_span(&summary.requests),
    ];
    let mut lines = fit_spans(cost_spans, content_width);
    if let Some(context) = &summary.context {
        lines.extend(fit_spans(context_spans(context), content_width));
    }
    if let Some(cache) = &summary.cache {
        lines.extend(fit_spans(
            vec![summary_label("Cache"), Span::styled(cache.clone(), text())],
            content_width,
        ));
    }

    lines
}

/// Column widths and the visible columns of the usage table.
struct TableLayout {
    column_widths: [usize; UsageColumn::COUNT],
    columns: Vec<UsageColumn>,
    model_width: usize,
}

impl TableLayout {
    /// Fits the table to `content_width`, dropping columns by priority.
    fn new(content: &UsagePanelContent, content_width: usize) -> Self {
        let column_widths = Self::column_widths(content);
        let model_natural_width = Self::model_natural_width(content);
        let columns = usage_format::visible_usage_columns(
            &column_widths,
            model_natural_width.min(MODEL_MIN_WIDTH),
            COLUMN_GAP_WIDTH,
            content_width,
        );
        let values_width = columns.iter().fold(0, |width, column| {
            width + COLUMN_GAP_WIDTH + column_widths[column.index()]
        });
        let model_width = content_width
            .saturating_sub(values_width)
            .min(model_natural_width)
            .max(1);

        Self {
            column_widths,
            columns,
            model_width,
        }
    }

    /// Returns the width of the table with every column at its widest.
    fn natural_width(content: &UsagePanelContent) -> usize {
        Self::column_widths(content)
            .iter()
            .fold(Self::model_natural_width(content), |width, column_width| {
                width + COLUMN_GAP_WIDTH + column_width
            })
    }

    /// Returns each value column's width: its header or widest cell.
    fn column_widths(content: &UsagePanelContent) -> [usize; UsageColumn::COUNT] {
        UsageColumn::ALL.map(|column| {
            content
                .rows
                .iter()
                .chain(std::iter::once(&content.total))
                .map(|row| display_width(&row.cell(column).panel_text()))
                .fold(display_width(column.header()), usize::max)
        })
    }

    /// Returns the widest model label, purpose included.
    fn model_natural_width(content: &UsagePanelContent) -> usize {
        content
            .rows
            .iter()
            .chain(std::iter::once(&content.total))
            .map(|row| {
                display_width(&row.model)
                    + row.purpose.map_or(0, |purpose| 1 + display_width(purpose))
            })
            .fold(display_width(MODEL_HEADER), usize::max)
    }

    /// Builds the bold header line.
    fn header_line(&self) -> Line<'static> {
        let header_style = Style::default()
            .fg(palette::accent())
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![Span::styled(
            pad_right(MODEL_HEADER, self.model_width),
            header_style,
        )];
        for column in &self.columns {
            spans.push(Span::styled(
                pad_left(column.header(), self.column_width(*column)),
                header_style,
            ));
        }

        Line::from(spans)
    }

    /// Builds one table line; the totals line is bold.
    fn row_line(&self, row: &UsageTableRow, is_total: bool) -> Line<'static> {
        let emphasis = if is_total {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        let mut spans = model_spans(row, self.model_width, emphasis);
        for column in &self.columns {
            let figure = row.cell(*column);
            spans.push(Span::styled(
                pad_left(&figure.panel_text(), self.column_width(*column)),
                figure_style(figure).add_modifier(emphasis),
            ));
        }

        Line::from(spans)
    }

    /// Returns the width of `column` including its leading gap.
    fn column_width(&self, column: UsageColumn) -> usize {
        COLUMN_GAP_WIDTH + self.column_widths[column.index()]
    }
}

/// Builds the model cell, its purpose dimmed after it, truncated to `width`.
fn model_spans(row: &UsageTableRow, width: usize, emphasis: Modifier) -> Vec<Span<'static>> {
    let model = text_util::truncate_with_ellipsis(&row.model, width);
    let mut used_width = display_width(&model);
    let mut spans = vec![Span::styled(model, text().add_modifier(emphasis))];
    if let Some(purpose) = row.purpose {
        let purpose = text_util::truncate_with_ellipsis(
            &format!(" {purpose}"),
            width.saturating_sub(used_width),
        );
        used_width += display_width(&purpose);
        spans.push(Span::styled(
            purpose,
            Style::default().fg(palette::text_subtle()),
        ));
    }
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used_width))));

    spans
}

/// Keeps `spans` on one line, padding intact, when they fit in `width`;
/// otherwise word-wraps them.
fn fit_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let line = Line::from(spans);
    if line.width() <= width {
        return vec![line];
    }

    text_util::wrap_styled_line(line.spans, width)
}

/// Builds the context summary spans: gauge, latest fill, and peak.
fn context_spans(context: &ContextSummary) -> Vec<Span<'static>> {
    let mut spans = vec![summary_label("Context")];
    if let Some(filled) = context.gauge_filled {
        spans.push(Span::styled(
            format!("{} ", usage_format::context_gauge(filled)),
            Style::default().fg(context_tone(context.now_percent)),
        ));
    }
    if let Some(now) = &context.now {
        spans.push(Span::styled(now.clone(), text()));
    }
    if let Some(peak) = &context.peak {
        let prefix = if context.now.is_some() {
            "   peak "
        } else {
            "peak "
        };
        spans.push(muted(prefix.to_string()));
        spans.push(Span::styled(peak.clone(), text()));
    }

    spans
}

/// Returns one figure's span in the panel's spelling and style.
fn figure_span(figure: &UsageFigure) -> Span<'static> {
    Span::styled(figure.panel_text(), figure_style(figure))
}

/// Returns the style of one figure: dimmed when unknown, unpriced, or not
/// recorded; weight is left to the totals row.
fn figure_style(figure: &UsageFigure) -> Style {
    match figure {
        UsageFigure::Known { .. } | UsageFigure::Cost(_) | UsageFigure::CostPair { .. } => text(),
        UsageFigure::NotRecorded | UsageFigure::Unreported | UsageFigure::Unpriced => {
            Style::default().fg(palette::text_subtle())
        }
    }
}

/// Returns one padded summary label.
fn summary_label(label: &str) -> Span<'static> {
    muted(pad_right(label, SUMMARY_LABEL_WIDTH))
}

/// Returns one muted span.
fn muted(text: String) -> Span<'static> {
    Span::styled(text, Style::default().fg(palette::text_muted()))
}

/// Returns the body text style.
fn text() -> Style {
    Style::default().fg(palette::text())
}

/// Pads `text` with trailing spaces to `width` columns.
fn pad_right(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(display_width(text)))
    )
}

/// Pads `text` with leading spaces to `width` columns.
fn pad_left(text: &str, width: usize) -> String {
    format!(
        "{}{text}",
        " ".repeat(width.saturating_sub(display_width(text)))
    )
}

/// Returns the terminal display width of `text`.
fn display_width(text: &str) -> usize {
    Span::raw(text).width()
}

#[cfg(test)]
#[path = "usage_overlay_test.rs"]
mod tests;
