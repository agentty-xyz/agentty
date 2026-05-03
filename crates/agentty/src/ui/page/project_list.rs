use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use time::OffsetDateTime;

use crate::domain::project::ProjectListItem;
use crate::domain::session::DailyActivity;
use crate::ui::activity_heatmap::{
    RecentActivityStats, build_activity_heatmap_grid, build_recent_activity_stats,
    build_visible_heatmap_month_row, current_day_key_local, heatmap_intensity_level,
    heatmap_max_count, visible_heatmap_week_count,
};
use crate::ui::state::help_action;
use crate::ui::{Page, layout, style};

const DAY_LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const HEATMAP_CELL_WIDTH: usize = 2;
const HEATMAP_DAY_LABEL_WIDTH: usize = 4;
/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
const ACTIVE_PROJECT_MARKER: &str = "* ";
/// Horizontal spacing between project-table columns.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Fixed height reserved for the top activity and work-pace panels.
const PROJECT_DASHBOARD_PANEL_HEIGHT: u16 = 12;
/// Percentage of the top-panel width reserved for the activity heatmap,
/// keeping it at half of the available panel.
const ACTIVITY_HEATMAP_WIDTH_PERCENT: u16 = 50;
/// Projects tab renderer showing saved repositories, activity, compact
/// work-performance stats, and project metadata.
pub struct ProjectListPage<'a> {
    /// Identifier for the currently active project.
    pub active_project_id: i64,
    /// Git repository project rows displayed in the table.
    pub projects: &'a [ProjectListItem],
    /// Persisted local-day session activity used by the projects heatmap.
    pub stats_activity: &'a [DailyActivity],
    /// Stateful cursor position for the project table.
    pub table_state: &'a mut TableState,
}

impl<'a> ProjectListPage<'a> {
    /// Creates a project-list page renderer with active-project highlighting
    /// and activity summary data.
    pub fn new(
        projects: &'a [ProjectListItem],
        stats_activity: &'a [DailyActivity],
        table_state: &'a mut TableState,
        active_project_id: i64,
    ) -> Self {
        Self {
            active_project_id,
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
        let info_panel_chunks = Layout::horizontal([
            Constraint::Percentage(ACTIVITY_HEATMAP_WIDTH_PERCENT),
            Constraint::Fill(1),
        ])
        .split(info_area);
        let heatmap_area = info_panel_chunks[0];
        let details_area = info_panel_chunks[1];
        let heatmap_block = Block::default()
            .borders(Borders::ALL)
            .title("Activity")
            .border_style(style::border_style());
        let details_block = Block::default()
            .borders(Borders::ALL)
            .title("Work Pace")
            .border_style(style::border_style());
        let heatmap_inner_area = heatmap_block.inner(heatmap_area);
        let heatmap_panel = Paragraph::new(self.build_heatmap_lines(heatmap_inner_area.width))
            .style(Style::default().fg(style::palette::text()))
            .block(heatmap_block)
            .wrap(Wrap { trim: false });
        let activity_stats =
            build_recent_activity_stats(self.stats_activity, current_day_key_local());
        let active_stats = ActiveProjectStats::from_projects(self.projects);
        let details_panel = Paragraph::new(work_stats_summary_lines(&activity_stats, active_stats))
            .style(Style::default().fg(style::palette::text()))
            .block(details_block)
            .wrap(Wrap { trim: true });

        let selected_style = Style::default().bg(style::palette::surface());
        let header = Row::new(["Project", "Branch", "Sessions", "Last Opened", "Path"])
            .style(
                Style::default()
                    .bg(style::palette::surface())
                    .fg(style::palette::text_muted())
                    .add_modifier(Modifier::BOLD),
            )
            .height(1)
            .bottom_margin(1);
        let active_project_id = self.active_project_id;
        let rows = self
            .projects
            .iter()
            .map(|project_item| render_project_row(project_item, active_project_id));
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

        let help_message = Paragraph::new(project_list_footer_line());
        f.render_widget(help_message, areas.footer_area);
    }
}

impl ProjectListPage<'_> {
    /// Builds the project-list activity heatmap lines and trims week columns
    /// to the visible panel width.
    fn build_heatmap_lines(&self, available_width: u16) -> Vec<Line<'static>> {
        let content_width = usize::from(available_width);
        let end_day_key = current_day_key_local();
        let grid = build_activity_heatmap_grid(self.stats_activity, end_day_key);
        let max_count = heatmap_max_count(&grid);
        let visible_week_count = Self::visible_heatmap_week_count(available_width);
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(Span::styled(
            format!("Activity Heatmap (Last {visible_week_count} Weeks)"),
            Style::default()
                .fg(style::palette::text())
                .add_modifier(Modifier::BOLD),
        )));

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

        if content_width < 24 {
            lines.push(Line::from(Span::styled(
                format!("Max/day: {max_count}"),
                Style::default().fg(style::palette::text_muted()),
            )));

            return lines;
        }

        let mut legend = vec![Span::styled(
            "Less ",
            Style::default().fg(style::palette::text_muted()),
        )];
        for intensity in 0_u8..=4 {
            legend.push(Span::styled(
                "  ",
                Style::default().bg(Self::heatmap_color(intensity)),
            ));
            legend.push(Span::raw(" "));
        }
        legend.push(Span::styled(
            "More",
            Style::default().fg(style::palette::text_muted()),
        ));
        if content_width >= 36 {
            legend.push(Span::raw(format!(" | Max/day: {max_count}")));
        }
        lines.push(Line::from(legend));

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

/// Aggregated active-session load shown in the compact work stats panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveProjectStats {
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

        Self {
            project_count,
            session_count,
        }
    }
}

/// Renders one project metadata row.
fn render_project_row(project_item: &ProjectListItem, active_project_id: i64) -> Row<'static> {
    let (title, branch, last_opened, path) = project_row_values(project_item, active_project_id);

    Row::new(vec![
        Cell::from(title),
        Cell::from(branch),
        Cell::from(session_count_line(
            project_item.session_count,
            project_item.active_session_count,
        )),
        Cell::from(last_opened),
        Cell::from(path),
    ])
    .style(project_row_style(project_item, active_project_id))
}

/// Returns responsive column widths for the Projects metadata table.
fn project_table_column_constraints() -> [Constraint; 5] {
    [
        Constraint::Fill(2),
        Constraint::Fill(2),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Fill(3),
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
    ]
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
    help_action::footer_line(&help_action::project_list_footer_actions())
}

/// Returns project row display values for reuse and testing.
fn project_row_values(
    project_item: &ProjectListItem,
    active_project_id: i64,
) -> (String, String, String, String) {
    let project = &project_item.project;
    let title = project_title(project_item, active_project_id);
    let branch = project.git_branch.as_deref().unwrap_or("-");
    let last_opened = format_last_opened(project.last_opened_at);
    let path = project.path.to_string_lossy().to_string();

    (title, branch.to_string(), last_opened, path)
}

/// Returns style for one project row, emphasizing the active project.
fn project_row_style(project_item: &ProjectListItem, active_project_id: i64) -> Style {
    if project_item.project.id == active_project_id {
        return Style::default().fg(style::palette::accent_soft());
    }

    Style::default().fg(style::palette::text())
}

/// Returns the visible project title, marking the active project in the list.
fn project_title(project_item: &ProjectListItem, active_project_id: i64) -> String {
    let display_label = project_item.project.display_label();
    if project_item.project.id == active_project_id {
        return format!("{ACTIVE_PROJECT_MARKER}{display_label}");
    }

    display_label
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

/// Formats the project last-opened timestamp for table display.
fn format_last_opened(last_opened_at: Option<i64>) -> String {
    let Some(last_opened_at) = last_opened_at else {
        return "Never".to_string();
    };
    let Ok(last_opened_datetime) = OffsetDateTime::from_unix_timestamp(last_opened_at) else {
        return "Unknown".to_string();
    };

    let year = last_opened_datetime.year();
    let month = u8::from(last_opened_datetime.month());
    let day = last_opened_datetime.day();

    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::project::Project;
    use crate::domain::theme::ColorTheme;

    #[test]
    fn test_row_highlight_symbol_uses_background_only_selection() {
        // Arrange
        let highlight_symbol = ROW_HIGHLIGHT_SYMBOL;

        // Act
        let is_empty_symbol = highlight_symbol.is_empty();

        // Assert
        assert!(is_empty_symbol);
    }

    #[test]
    fn test_project_table_column_spacing_is_wider_for_readability() {
        // Arrange
        let expected_spacing = 2;

        // Act
        let spacing = TABLE_COLUMN_SPACING;

        // Assert
        assert_eq!(spacing, expected_spacing);
    }

    #[test]
    fn test_project_table_descriptive_columns_use_flexible_widths() {
        // Arrange
        let expected_constraints = [
            Constraint::Fill(2),
            Constraint::Fill(2),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Fill(3),
        ];

        // Act
        let constraints = project_table_column_constraints();

        // Assert
        assert_eq!(constraints, expected_constraints);
    }

    #[test]
    fn test_activity_heatmap_uses_half_of_top_panel_width() {
        // Arrange
        let expected_width_percent = 50;

        // Act
        let width_percent = ACTIVITY_HEATMAP_WIDTH_PERCENT;

        // Assert
        assert_eq!(width_percent, expected_width_percent);
    }

    #[test]
    fn test_render_uses_palette_border_for_projects_table() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let projects = vec![ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: None,
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 42,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 0,
        }];
        let activity: Vec<DailyActivity> = Vec::new();
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                ProjectListPage::new(&projects, &activity, &mut table_state, 42)
                    .render(frame, frame.area());
            })
            .expect("failed to draw projects page");

        // Assert
        let border_cell_count = foreground_symbol_cell_count(terminal.backend().buffer(), "┌");
        assert!(
            border_cell_count >= 3,
            "expected heatmap, work-pace, and projects panels to use palette border color"
        );
    }

    #[test]
    fn test_format_last_opened_uses_iso_like_date() {
        // Arrange
        let last_opened_at = Some(1_700_000_000);

        // Act
        let formatted = format_last_opened(last_opened_at);

        // Assert
        assert_eq!(formatted, "2023-11-14");
    }

    #[test]
    fn test_format_last_opened_returns_never_without_timestamp() {
        // Arrange
        let last_opened_at = None;

        // Act
        let formatted = format_last_opened(last_opened_at);

        // Assert
        assert_eq!(formatted, "Never");
    }

    #[test]
    fn test_format_last_opened_returns_unknown_for_invalid_timestamp() {
        // Arrange
        let last_opened_at = Some(i64::MAX);

        // Act
        let formatted = format_last_opened(last_opened_at);

        // Assert
        assert_eq!(formatted, "Unknown");
    }

    #[test]
    fn test_project_row_values_show_dashboard_metadata() {
        // Arrange
        let project_item = ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: Some(20),
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 1,
                is_favorite: true,
                last_opened_at: Some(1_700_000_000),
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 3,
        };

        // Act
        let values = project_row_values(&project_item, 99);

        // Assert
        assert_eq!(values.0, "agentty");
        assert_eq!(values.1, "main");
        assert_eq!(values.2, "2023-11-14");
        assert_eq!(values.3, "/tmp/agentty");
    }

    #[test]
    fn test_project_row_values_use_fallbacks_for_missing_branch_and_timestamp() {
        // Arrange
        let project_item = ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: None,
            project: Project {
                created_at: 1,
                display_name: None,
                git_branch: None,
                id: 1,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 0,
        };

        // Act
        let values = project_row_values(&project_item, 99);

        // Assert
        assert_eq!(values.0, "agentty");
        assert_eq!(values.1, "-");
        assert_eq!(values.2, "Never");
        assert_eq!(values.3, "/tmp/agentty");
    }

    #[test]
    fn test_session_count_line_shows_plain_total_without_active() {
        // Arrange & Act
        let line = session_count_line(7, 0);

        // Assert
        assert_eq!(line.to_string(), "7");
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn test_session_count_line_colors_active_indicator_yellow() {
        // Arrange & Act
        let line = session_count_line(5, 2);

        // Assert
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content.as_ref(), "5 ");
        assert_eq!(line.spans[1].content.as_ref(), "▶ 2");
        assert_eq!(line.spans[1].style.fg, Some(style::palette::warning()));
    }

    #[test]
    fn test_project_row_values_mark_active_project_title() {
        // Arrange
        let project_item = ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: Some(20),
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 42,
                is_favorite: true,
                last_opened_at: Some(1_700_000_000),
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 3,
        };

        // Act
        let values = project_row_values(&project_item, 42);

        // Assert
        assert_eq!(values.0, "* agentty");
    }

    #[test]
    fn test_project_row_style_uses_accent_for_active_project() {
        // Arrange
        let project_item = ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: None,
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 42,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 0,
        };

        // Act
        let style = project_row_style(&project_item, 42);

        // Assert
        assert_eq!(style.fg, Some(style::palette::accent_soft()));
    }

    #[test]
    fn test_project_row_style_uses_text_color_for_inactive_project() {
        // Arrange
        let project_item = ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: None,
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 42,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 0,
        };

        // Act
        let style = project_row_style(&project_item, 7);

        // Assert
        assert_eq!(style.fg, Some(style::palette::text()));
    }

    #[test]
    fn test_project_list_footer_line_matches_project_shortcuts() {
        // Arrange
        let expected_line = help_action::footer_line(&help_action::project_list_footer_actions());

        // Act
        let footer_line = project_list_footer_line();

        // Assert
        assert_eq!(footer_line, expected_line);
    }

    #[test]
    fn test_render_shows_activity_heatmap_in_separate_top_panel() {
        // Arrange
        let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
        let projects = vec![ProjectListItem {
            active_session_count: 0,
            last_session_updated_at: None,
            project: Project {
                created_at: 1,
                display_name: Some("test-project".to_string()),
                git_branch: Some("main".to_string()),
                id: 42,
                is_favorite: false,
                last_opened_at: None,
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 0,
        }];
        let activity = vec![DailyActivity {
            day_key: current_day_key_local(),
            session_count: 3,
        }];
        let mut table_state = TableState::default();
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                ProjectListPage::new(&projects, &activity, &mut table_state, 42)
                    .render(frame, frame.area());
            })
            .expect("failed to draw projects page");

        // Assert
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("┐┌"));
        assert!(text.contains("Activity Heatmap"));
        assert!(text.contains("Less"));
        assert!(text.contains("More"));
        assert!(text.contains("Work Pace"));
        assert!(!text.contains("Version"));
        assert!(!text.contains("Agentic Development Environment"));
        assert!(text.contains("Active Sessions 0"));
        assert!(text.contains("Active Projects 0"));
        assert!(!text.contains("Daily Pace"));
    }

    #[test]
    fn test_build_heatmap_lines_uses_persisted_activity_for_max_count() {
        // Arrange
        let activity = vec![DailyActivity {
            day_key: current_day_key_local(),
            session_count: 50,
        }];
        let mut table_state = TableState::default();
        let page = ProjectListPage::new(&[], &activity, &mut table_state, 42);

        // Act
        let heatmap_lines = page.build_heatmap_lines(80);
        let rendered_text = heatmap_lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(rendered_text.contains("Max/day: 50"));
    }

    #[test]
    fn test_build_heatmap_lines_trims_visible_weeks_on_narrow_width() {
        // Arrange
        let activity = vec![DailyActivity {
            day_key: current_day_key_local(),
            session_count: 1,
        }];
        let mut table_state = TableState::default();
        let page = ProjectListPage::new(&[], &activity, &mut table_state, 42);

        // Act
        let heatmap_lines = page.build_heatmap_lines(28);
        let monday_row = &heatmap_lines[2];

        // Assert
        assert_eq!(monday_row.spans.len(), 13);
    }

    #[test]
    fn test_work_stats_summary_lines_show_recent_activity_counts() {
        // Arrange
        let activity_stats = RecentActivityStats {
            best_streak_days: 5,
            current_streak_days: 3,
            sessions_last_30_days: 22,
            sessions_last_7_days: 8,
        };
        let active_stats = ActiveProjectStats {
            project_count: 2,
            session_count: 4,
        };

        // Act
        let rendered_text = work_stats_summary_lines(&activity_stats, active_stats)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(!rendered_text.contains("Version"));
        assert!(!rendered_text.contains("Agentic Development Environment"));
        assert!(!rendered_text.contains("Work Pace"));
        assert!(rendered_text.contains("7d 8"));
        assert!(rendered_text.contains("30d 22"));
        assert!(rendered_text.contains("Streak 3d"));
        assert!(rendered_text.contains("Best 5d"));
        assert!(rendered_text.contains("Active Sessions 4"));
        assert!(rendered_text.contains("Active Projects 2"));
    }

    #[test]
    fn test_active_project_stats_counts_active_sessions_and_projects() {
        // Arrange
        let projects = vec![
            ProjectListItem {
                active_session_count: 0,
                last_session_updated_at: None,
                project: Project {
                    created_at: 1,
                    display_name: Some("agentty".to_string()),
                    git_branch: Some("main".to_string()),
                    id: 1,
                    is_favorite: false,
                    last_opened_at: None,
                    path: PathBuf::from("/tmp/agentty"),
                    updated_at: 2,
                },
                session_count: 3,
            },
            ProjectListItem {
                active_session_count: 2,
                last_session_updated_at: None,
                project: Project {
                    created_at: 1,
                    display_name: Some("other".to_string()),
                    git_branch: Some("main".to_string()),
                    id: 2,
                    is_favorite: false,
                    last_opened_at: None,
                    path: PathBuf::from("/tmp/other"),
                    updated_at: 2,
                },
                session_count: 5,
            },
            ProjectListItem {
                active_session_count: 1,
                last_session_updated_at: None,
                project: Project {
                    created_at: 1,
                    display_name: Some("third".to_string()),
                    git_branch: Some("main".to_string()),
                    id: 3,
                    is_favorite: false,
                    last_opened_at: None,
                    path: PathBuf::from("/tmp/third"),
                    updated_at: 2,
                },
                session_count: 1,
            },
        ];

        // Act
        let stats = ActiveProjectStats::from_projects(&projects);

        // Assert
        assert_eq!(stats.project_count, 2);
        assert_eq!(stats.session_count, 3);
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Counts cells matching a rendered symbol and the active palette border
    /// color.
    fn foreground_symbol_cell_count(buffer: &ratatui::buffer::Buffer, symbol: &str) -> usize {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == symbol && cell.fg == style::palette::border())
            .count()
    }
}
