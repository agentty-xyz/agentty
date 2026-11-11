use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::app::Tab;
use crate::domain::project::ProjectListItem;
use crate::domain::resource::SessionResources;
use crate::ui::{Component, style};

/// Minimum blank columns kept between the tabs and the process stats.
const PROCESS_STATS_GAP: usize = 2;

/// Header tabs rendered at the top of list mode pages, with Agentty
/// process-tree stats right-aligned on the same row.
pub struct Tabs<'a> {
    active_project_id: i64,
    current_tab: Tab,
    process_resources: Option<SessionResources>,
    projects: &'a [ProjectListItem],
}

impl Tabs<'_> {
    /// Creates a tabs component with the provided active tab and project
    /// context used to label the project-scoped tab group.
    pub fn new(current_tab: Tab, active_project_id: i64, projects: &[ProjectListItem]) -> Tabs<'_> {
        Tabs {
            active_project_id,
            current_tab,
            process_resources: None,
            projects,
        }
    }

    /// Sets the Agentty process-tree totals shown at the right edge of the
    /// tab row.
    #[must_use]
    pub fn process_stats(mut self, process_resources: Option<SessionResources>) -> Self {
        self.process_resources = process_resources;

        self
    }
}

impl Component for Tabs<'_> {
    /// Renders the tabs and right-aligns the process stats in the space left
    /// beside them. Tabs keep priority on narrow terminals.
    fn render(&self, f: &mut Frame, area: Rect) {
        let line = Line::from(tab_spans(
            self.current_tab,
            self.active_project_id,
            self.projects,
        ));
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(style::border_style())
            .padding(Padding::top(1));
        let inner_area = block.inner(area);
        let stats_width =
            usize::from(inner_area.width).saturating_sub(line.width() + PROCESS_STATS_GAP);
        let stats = process_stats_label(self.process_resources, stats_width);

        f.render_widget(Paragraph::new(line).block(block), area);
        if let Some(stats) = stats
            && inner_area.height > 0
        {
            let stats_line = Line::styled(stats, Style::default().fg(style::palette::text_muted()))
                .right_aligned();
            f.render_widget(
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

/// Returns styled tab spans with separators and a shared project-scope label.
fn tab_spans(
    current_tab: Tab,
    active_project_id: i64,
    projects: &[ProjectListItem],
) -> Vec<Span<'static>> {
    let tab_count = Tab::project_scoped_tabs().len();
    let mut spans = Vec::with_capacity(2 * tab_count + 3);

    spans.push(tab_span(Tab::Projects, current_tab));
    spans.push(tab_separator_span());
    spans.push(project_context_span(active_project_id, projects));

    for tab in Tab::project_scoped_tabs() {
        spans.push(tab_separator_span());
        spans.push(tab_span(*tab, current_tab));
    }

    spans
}

/// Returns one styled separator span between tabs.
fn tab_separator_span() -> Span<'static> {
    Span::styled("|", Style::default().fg(style::palette::border()))
}

/// Returns one styled tab span with active/inactive affordance treatment.
fn tab_span(tab: Tab, current_tab: Tab) -> Span<'static> {
    let label = format!(" {} ", tab_label(tab));

    if tab == current_tab {
        return Span::styled(label, style::selection_style());
    }

    Span::styled(label, Style::default().fg(style::palette::text_muted()))
}

/// Returns a styled span describing the active project for project-scoped tabs.
fn project_context_span(active_project_id: i64, projects: &[ProjectListItem]) -> Span<'static> {
    let (project_name, project_scope_style) = active_project_name(active_project_id, projects)
        .map_or_else(
            || {
                (
                    "None".to_string(),
                    Style::default().fg(style::palette::text_subtle()),
                )
            },
            |project_name| {
                (
                    project_name,
                    Style::default()
                        .fg(style::palette::accent_soft())
                        .add_modifier(Modifier::BOLD),
                )
            },
        );
    let project_scope = format!(" Project: {project_name} ");

    Span::styled(project_scope, project_scope_style)
}

/// Returns the display label for a top-level tab.
fn tab_label(tab: Tab) -> &'static str {
    tab.title()
}

/// Returns the active project name shown before project-scoped tabs.
fn active_project_name(active_project_id: i64, projects: &[ProjectListItem]) -> Option<String> {
    projects
        .iter()
        .find(|project_item| project_item.project.id == active_project_id)
        .map(|project_item| project_item.project.display_label())
}

#[cfg(test)]
#[path = "tab_test.rs"]
mod tests;
