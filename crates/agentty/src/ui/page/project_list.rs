use std::env;
use std::path::Path;

use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};

use crate::domain::agent::{AgentCliInfo, AgentCliVersion};
use crate::domain::project::ProjectListItem;
use crate::domain::session::DailyActivity;
use crate::presentation::help_action;
use crate::ui::activity_heatmap::{
    RecentActivityStats, build_activity_heatmap_grid, build_recent_activity_stats,
    build_visible_heatmap_month_row, heatmap_intensity_level, heatmap_max_count,
    visible_heatmap_week_count,
};
use crate::ui::{Page, layout, style};

const DAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

const HEATMAP_CELL_WIDTH: usize = 2;

const HEATMAP_DAY_LABEL_WIDTH: usize = 4;

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";

const ACTIVE_PROJECT_MARKER: &str = "* ";

/// Horizontal spacing between project-table columns.
const TABLE_COLUMN_SPACING: u16 = 2;

/// Month heading plus one row for each weekday in the activity heatmap.
const HEATMAP_CONTENT_HEIGHT: u16 = 8;

/// Heatmap content height plus the top dashboard panel borders.
const PROJECT_DASHBOARD_PANEL_HEIGHT: u16 = HEATMAP_CONTENT_HEIGHT + 2;

/// Projects tab renderer showing saved repositories, activity, compact
/// work-performance stats, available agent CLIs, and project metadata.
pub struct ProjectListPage<'a> {
    /// Identifier for the currently active project.
    pub active_project_id: i64,
    /// Local day key derived from the frame timestamp and UTC offset.
    pub activity_end_day_key: i64,
    /// Locally available agent CLI executables and detected versions.
    pub agent_clis: &'a [AgentCliInfo],
    /// Git repository project rows displayed in the table.
    pub projects: &'a [ProjectListItem],
    /// Persisted local-day session activity used by the projects heatmap.
    pub stats_activity: &'a [DailyActivity],
    /// Stateful cursor position for the project table.
    pub table_state: &'a mut TableState,
}

impl<'a> ProjectListPage<'a> {
    /// Creates a project-list page renderer with active-project highlighting
    /// plus activity and agent-CLI summary data.
    pub fn new(
        projects: &'a [ProjectListItem],
        agent_clis: &'a [AgentCliInfo],
        stats_activity: &'a [DailyActivity],
        table_state: &'a mut TableState,
        active_project_id: i64,
        activity_end_day_key: i64,
    ) -> Self {
        Self {
            active_project_id,
            activity_end_day_key,
            agent_clis,
            projects,
            stats_activity,
            table_state,
        }
    }
}

impl Page for ProjectListPage<'_> {
    /// Renders the projects page with separate activity and work-pace dashboard
    /// panels, project rows, and compact tab-page spacing.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);
        let content_chunks = Layout::vertical([
            Constraint::Length(PROJECT_DASHBOARD_PANEL_HEIGHT),
            Constraint::Min(0),
        ])
        .split(areas.main_area);
        let info_area = content_chunks[0];
        let project_area = content_chunks[1];
        let info_panel_chunks =
            Layout::horizontal(project_dashboard_panel_constraints()).split(info_area);
        let heatmap_area = info_panel_chunks[0];
        let details_area = info_panel_chunks[1];
        let agent_cli_area = info_panel_chunks[2];
        let heatmap_block = Block::default()
            .borders(Borders::ALL)
            .title("Activity")
            .border_style(style::border_style());
        let details_block = Block::default()
            .borders(Borders::ALL)
            .title("Work Pace")
            .border_style(style::border_style());
        let agent_cli_block = Block::default()
            .borders(Borders::ALL)
            .title("Agent CLIs")
            .border_style(style::border_style());
        let heatmap_inner_area = heatmap_block.inner(heatmap_area);
        let heatmap_panel = Paragraph::new(self.build_heatmap_lines(heatmap_inner_area.width))
            .style(Style::default().fg(style::palette::text()))
            .block(heatmap_block)
            .wrap(Wrap { trim: false });
        let activity_stats =
            build_recent_activity_stats(self.stats_activity, self.activity_end_day_key);
        let active_stats = ActiveProjectStats::from_projects(self.projects);
        let details_panel = Paragraph::new(work_stats_summary_lines(&activity_stats, active_stats))
            .style(Style::default().fg(style::palette::text()))
            .block(details_block)
            .wrap(Wrap { trim: true });
        let agent_cli_panel = Paragraph::new(agent_cli_summary_lines(self.agent_clis))
            .style(Style::default().fg(style::palette::text()))
            .block(agent_cli_block)
            .wrap(Wrap { trim: true });

        let selected_style = Style::default().bg(style::palette::surface_selection());
        let header_cells = ["Project", "Branch", "Sessions", "Path"]
            .iter()
            .map(|header| left_aligned_cell(*header));
        let header = Row::new(header_cells)
            .style(
                Style::default()
                    .bg(style::palette::surface())
                    .fg(style::palette::text_muted())
                    .add_modifier(Modifier::BOLD),
            )
            .height(1)
            .bottom_margin(1);
        let active_project_id = self.active_project_id;
        let home_directory = env::home_dir();
        let rows = self.projects.iter().map(|project_item| {
            render_project_row(project_item, active_project_id, home_directory.as_deref())
        });
        let table = Table::new(rows, project_table_column_constraints())
            .column_spacing(TABLE_COLUMN_SPACING)
            .header(header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Projects")
                    .border_style(style::border_style()),
            )
            .row_highlight_style(selected_style)
            .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        f.render_stateful_widget(table, project_area, self.table_state);
        f.render_widget(heatmap_panel, heatmap_area);
        f.render_widget(details_panel, details_area);
        f.render_widget(agent_cli_panel, agent_cli_area);

        let help_message = Paragraph::new(project_list_footer_line());
        f.render_widget(help_message, areas.footer_area);
    }
}

impl ProjectListPage<'_> {
    /// Builds the heatmap month heading and weekday rows, trimming week
    /// columns to the visible panel width.
    fn build_heatmap_lines(&self, available_width: u16) -> Vec<Line<'static>> {
        let end_day_key = self.activity_end_day_key;
        let grid = build_activity_heatmap_grid(self.stats_activity, end_day_key);
        let max_count = heatmap_max_count(&grid);
        let visible_week_count = Self::visible_heatmap_week_count(available_width);
        let mut lines: Vec<Line<'static>> = Vec::new();

        let month_row = build_visible_heatmap_month_row(
            end_day_key,
            HEATMAP_DAY_LABEL_WIDTH,
            HEATMAP_CELL_WIDTH,
            visible_week_count,
        );
        lines.push(Line::from(Span::styled(
            month_row,
            Style::default().fg(style::palette::text_muted()),
        )));

        for (day_index, day_label) in DAY_LABELS.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!("{day_label} "),
                Style::default().fg(style::palette::text_muted()),
            )];

            let first_visible_week = grid[day_index].len().saturating_sub(visible_week_count);
            for cell_count in &grid[day_index][first_visible_week..] {
                let intensity = heatmap_intensity_level(*cell_count, max_count);
                spans.push(Span::styled(
                    "  ",
                    Style::default().bg(Self::heatmap_color(intensity)),
                ));
            }

            lines.push(Line::from(spans));
        }

        lines
    }

    /// Returns the number of heatmap week columns visible inside a panel of
    /// `available_width`.
    fn visible_heatmap_week_count(available_width: u16) -> usize {
        let content_width = usize::from(available_width);

        visible_heatmap_week_count(content_width, HEATMAP_DAY_LABEL_WIDTH, HEATMAP_CELL_WIDTH)
    }

    /// Returns the color used for one projects-page heatmap intensity.
    fn heatmap_color(intensity: u8) -> Color {
        match intensity {
            1 => Color::Rgb(14, 68, 41),
            2 => Color::Rgb(0, 109, 50),
            3 => Color::Rgb(38, 166, 65),
            4 => Color::Rgb(57, 211, 83),
            _ => Color::Rgb(33, 38, 45),
        }
    }
}

/// Aggregated project metrics shown in the compact work stats panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveProjectStats {
    /// Total input tokens accumulated across visible projects.
    input_tokens: u64,
    /// Total output tokens accumulated across visible projects.
    output_tokens: u64,
    /// Number of projects with at least one active session.
    project_count: u32,
    /// Number of currently active sessions across all projects.
    session_count: u32,
}

impl ActiveProjectStats {
    /// Builds active-session load metrics from project-list rows.
    fn from_projects(projects: &[ProjectListItem]) -> Self {
        let project_count = projects
            .iter()
            .filter(|project_item| project_item.active_session_count > 0)
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        let session_count = projects.iter().fold(0_u32, |total, project_item| {
            total.saturating_add(project_item.active_session_count)
        });
        let input_tokens = projects.iter().fold(0_u64, |total, project_item| {
            total.saturating_add(project_item.input_tokens)
        });
        let output_tokens = projects.iter().fold(0_u64, |total, project_item| {
            total.saturating_add(project_item.output_tokens)
        });

        Self {
            input_tokens,
            output_tokens,
            project_count,
            session_count,
        }
    }
}

/// Returns equal-width constraints for the top dashboard panels.
fn project_dashboard_panel_constraints() -> [Constraint; 3] {
    [
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ]
}

/// Renders one project metadata row.
fn render_project_row(
    project_item: &ProjectListItem,
    active_project_id: i64,
    home_directory: Option<&Path>,
) -> Row<'static> {
    let (title, branch, path) = project_row_values(project_item, active_project_id, home_directory);

    Row::new(vec![
        left_aligned_cell(title),
        left_aligned_cell(branch),
        left_aligned_cell(session_count_line(
            project_item.session_count,
            project_item.active_session_count,
        )),
        left_aligned_cell(path),
    ])
    .style(project_row_style(project_item, active_project_id))
}

/// Returns responsive column widths for the Projects metadata table.
fn project_table_column_constraints() -> [Constraint; 4] {
    [
        Constraint::Fill(3),
        Constraint::Fill(2),
        Constraint::Fill(1),
        Constraint::Fill(4),
    ]
}

/// Builds styled summary lines for work-performance metrics shown beneath
/// the `Work Pace` panel title.
fn work_stats_summary_lines(
    activity_stats: &RecentActivityStats,
    active_stats: ActiveProjectStats,
) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            stat_label_span("7d "),
            stat_value_span(activity_stats.sessions_last_7_days.to_string()),
            stat_label_span("   30d "),
            stat_value_span(activity_stats.sessions_last_30_days.to_string()),
        ]),
        Line::from(vec![
            stat_label_span("Streak "),
            stat_value_span(format!("{}d", activity_stats.current_streak_days)),
            stat_label_span("   Best "),
            stat_value_span(format!("{}d", activity_stats.best_streak_days)),
        ]),
        Line::from(vec![
            stat_label_span("Active Sessions "),
            stat_value_span(active_stats.session_count.to_string()),
        ]),
        Line::from(vec![
            stat_label_span("Active Projects "),
            stat_value_span(active_stats.project_count.to_string()),
        ]),
        Line::from(vec![
            stat_label_span("Tokens In "),
            stat_value_span(text_util::format_token_count(active_stats.input_tokens)),
            stat_label_span("   Out "),
            stat_value_span(text_util::format_token_count(active_stats.output_tokens)),
        ]),
    ]
}

/// Builds styled summary lines for available agent CLI versions.
fn agent_cli_summary_lines(agent_clis: &[AgentCliInfo]) -> Vec<Line<'static>> {
    if agent_clis.is_empty() {
        return vec![Line::from(Span::styled(
            "No supported CLIs found",
            Style::default().fg(style::palette::text_muted()),
        ))];
    }

    agent_clis
        .iter()
        .map(|agent_cli| {
            Line::from(vec![
                stat_label_span(format!("{} ", agent_cli.executable_name)),
                agent_cli_version_span(&agent_cli.version),
            ])
        })
        .collect()
}

/// Returns a styled version or loading span for one agent CLI row.
fn agent_cli_version_span(version: &AgentCliVersion) -> Span<'static> {
    match version {
        AgentCliVersion::Loading => Span::styled(
            "updating...",
            Style::default().fg(style::palette::text_muted()),
        ),
        AgentCliVersion::Unknown => stat_value_span("version unknown"),
        AgentCliVersion::Value(version) => stat_value_span(version.clone()),
    }
}

/// Returns a muted label span for the work-stats summary.
fn stat_label_span(text: impl Into<String>) -> Span<'static> {
    Span::styled(
        text.into(),
        Style::default().fg(style::palette::text_muted()),
    )
}

/// Returns an emphasized value span for the work-stats summary.
fn stat_value_span(text: impl Into<String>) -> Span<'static> {
    Span::styled(
        text.into(),
        Style::default().fg(style::palette::accent_soft()),
    )
}

/// Returns the footer help content rendered below the projects table.
fn project_list_footer_line() -> Line<'static> {
    crate::ui::help_format::footer_line(&help_action::project_list_footer_actions())
}

/// Returns project row display values for reuse and testing.
fn project_row_values(
    project_item: &ProjectListItem,
    active_project_id: i64,
    home_directory: Option<&Path>,
) -> (Line<'static>, Line<'static>, Line<'static>) {
    let project = &project_item.project;
    let title = project_title(project_item, active_project_id);
    let branch = project_branch_line(project_item);
    let path = display_project_path(project.path.as_path(), home_directory);

    (
        left_aligned_line(title),
        left_aligned_line(branch),
        left_aligned_line(path),
    )
}

/// Returns a project path label using `~` for paths inside the home directory.
fn display_project_path(project_path: &Path, home_directory: Option<&Path>) -> String {
    let Some(home_directory) = home_directory else {
        return project_path.to_string_lossy().to_string();
    };
    let Ok(relative_path) = project_path.strip_prefix(home_directory) else {
        return project_path.to_string_lossy().to_string();
    };

    if relative_path.as_os_str().is_empty() {
        return "~".to_string();
    }

    format!("~/{}", relative_path.display())
}

/// Returns style for one project row, emphasizing the active project.
fn project_row_style(project_item: &ProjectListItem, active_project_id: i64) -> Style {
    if project_item.project.id == active_project_id {
        return Style::default().fg(style::palette::accent_soft());
    }

    Style::default().fg(style::palette::text())
}

/// Returns the visible project title, marking the active project in the list.
fn project_title(project_item: &ProjectListItem, active_project_id: i64) -> Line<'static> {
    let mut spans = Vec::new();
    if project_item.project.id == active_project_id {
        spans.push(Span::raw(ACTIVE_PROJECT_MARKER));
    }

    spans.push(Span::raw(project_item.project.display_label()));

    Line::from(spans)
}

/// Returns the branch label for the branch column.
fn project_branch_line(project_item: &ProjectListItem) -> Line<'static> {
    let branch = project_item.project.git_branch.as_deref().unwrap_or("-");

    Line::from(branch.to_string())
}

/// Builds a styled line for the session count column, coloring the active
/// indicator in yellow when active sessions exist.
fn session_count_line(total: u32, active: u32) -> Line<'static> {
    if active > 0 {
        return Line::from(vec![
            Span::raw(format!("{total} ")),
            Span::styled(
                format!("▶ {active}"),
                Style::default().fg(style::palette::warning()),
            ),
        ]);
    }

    Line::from(total.to_string())
}

/// Builds a table cell whose text is explicitly left-aligned.
fn left_aligned_cell(content: impl Into<Line<'static>>) -> Cell<'static> {
    Cell::from(left_aligned_line(content))
}

/// Returns table content with explicit left alignment.
fn left_aligned_line(content: impl Into<Line<'static>>) -> Line<'static> {
    content.into().alignment(Alignment::Left)
}

#[cfg(test)]
#[path = "project_list_test.rs"]
mod tests;
