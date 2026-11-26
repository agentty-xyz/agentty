//! Header tabs for list-mode pages with right-aligned Agentty process-tree
//! stats.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::app::Tab;
use crate::domain::resource::SessionResources;
use crate::ui::{Component, style};

/// Minimum blank columns kept between the tabs and the process stats.
const PROCESS_STATS_GAP: usize = 2;

/// Header tabs rendered below the status bar on list mode pages, with
/// Agentty process-tree stats right-aligned on the tab row.
///
/// The component occupies [`Tabs::HEIGHT`] rows: a blank spacer row, the tab
/// row, and its bottom rule.
pub struct Tabs {
    current_tab: Tab,
    /// Label of the Platform tab, or `None` when this launch does not offer
    /// it.
    platform_label: Option<String>,
    process_resources: Option<SessionResources>,
}

impl Tabs {
    /// Rows the tab header occupies: a blank spacer row, the tab row, and its
    /// bottom rule.
    pub const HEIGHT: u16 = 3;

    /// Creates a tabs component with the provided active tab.
    pub fn new(current_tab: Tab) -> Self {
        Self {
            current_tab,
            platform_label: None,
            process_resources: None,
        }
    }

    /// Shows the Platform tab after the others under `platform_label`,
    /// which reports its connected devices; `None` leaves the tab out.
    #[must_use]
    pub fn platform_label(mut self, platform_label: Option<String>) -> Self {
        self.platform_label = platform_label;

        self
    }

    /// Sets the Agentty process-tree totals shown at the right edge of the
    /// tab row.
    #[must_use]
    pub fn process_stats(mut self, process_resources: Option<SessionResources>) -> Self {
        self.process_resources = process_resources;

        self
    }
}

impl Component for Tabs {
    /// Renders the tabs and right-aligns the process stats in the space left
    /// beside them. Tabs keep priority on narrow terminals.
    fn render(&self, frame: &mut Frame, area: Rect) {
        let line = Line::from(tab_spans(self.current_tab, self.platform_label.as_deref()));
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(style::border_style())
            .padding(Padding::top(1));
        let inner_area = block.inner(area);
        let stats_width =
            usize::from(inner_area.width).saturating_sub(line.width() + PROCESS_STATS_GAP);
        let stats = process_stats_label(self.process_resources, stats_width);

        frame.render_widget(Paragraph::new(line).block(block), area);
        if let Some(stats) = stats
            && inner_area.height > 0
        {
            let stats_line = Line::styled(stats, Style::default().fg(style::palette::text_muted()))
                .right_aligned();
            frame.render_widget(
                Paragraph::new(stats_line),
                Rect {
                    height: 1,
                    ..inner_area
                },
            );
        }
    }
}

/// Formats compact process-tree stats: `Processes`, `CPU`, and `RAM`
/// (resident memory).
///
/// Trailing stats are dropped until the label fits `max_width`. Returns `None`
/// when no sample is available or not even the process count fits.
fn process_stats_label(
    process_resources: Option<SessionResources>,
    max_width: usize,
) -> Option<String> {
    let resources = process_resources?;
    let memory = resources.resident_memory_kib;
    let stats = [
        format!("Processes: {}", resources.process_count),
        format!("CPU: {:.1}%", resources.cpu_percent),
        format!("RAM: {}.{} MiB", memory / 1024, memory % 1024 * 10 / 1024),
    ];

    (1..=stats.len())
        .rev()
        .map(|count| stats[..count].join("  "))
        .find(|label| Line::raw(label.as_str()).width() <= max_width)
}

/// Returns styled tab spans with separators between the top-level tabs. The
/// Platform tab comes last under `platform_label`, when it is offered.
fn tab_spans(current_tab: Tab, platform_label: Option<&str>) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(2 * Tab::ALL.len() - 1);

    for tab in Tab::ALL {
        let label = match tab {
            Tab::Platform => platform_label,
            Tab::Projects | Tab::Sessions | Tab::Settings => Some(tab.title()),
        };
        let Some(label) = label else {
            continue;
        };
        if !spans.is_empty() {
            spans.push(tab_separator_span());
        }
        spans.push(tab_span(tab, label, current_tab));
    }

    spans
}

/// Returns one styled tab span with active/inactive affordance treatment.
fn tab_span(tab: Tab, label: &str, current_tab: Tab) -> Span<'static> {
    let label = format!(" {label} ");

    if tab == current_tab {
        return Span::styled(label, style::selection_style());
    }

    Span::styled(label, Style::default().fg(style::palette::text_muted()))
}

/// Returns one styled separator span between tabs.
fn tab_separator_span() -> Span<'static> {
    Span::styled("|", Style::default().fg(style::palette::border()))
}

#[cfg(test)]
#[path = "tab_test.rs"]
mod tests;
