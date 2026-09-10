use std::path::PathBuf;

use ratatui::layout::{Alignment, Constraint};
use ratatui::text::Line;
use ratatui::widgets::TableState;

use super::{
    ActiveProjectStats, HEATMAP_CONTENT_HEIGHT, PROJECT_DASHBOARD_PANEL_HEIGHT, ProjectListPage,
    ROW_HIGHLIGHT_SYMBOL, TABLE_COLUMN_SPACING, agent_cli_summary_lines, display_project_path,
    left_aligned_line, project_dashboard_panel_constraints, project_list_footer_line,
    project_row_style, project_row_values, project_table_column_constraints, session_count_line,
    work_stats_summary_lines,
};
use crate::domain::agent::{AgentCliInfo, AgentKind};
use crate::domain::project::{Project, ProjectListItem};
use crate::domain::session::DailyActivity;
use crate::domain::theme::ColorTheme;
use crate::presentation::help_action;
use crate::ui::activity_heatmap::RecentActivityStats;
use crate::ui::render::Page;
use crate::ui::style;

const TEST_ACTIVITY_DAY_KEY: i64 = 20_000;

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
        Constraint::Fill(3),
        Constraint::Fill(2),
        Constraint::Fill(1),
        Constraint::Fill(4),
    ];

    // Act
    let constraints = project_table_column_constraints();

    // Assert
    assert_eq!(constraints, expected_constraints);
}

#[test]
fn test_dashboard_panels_use_equal_widths() {
    // Arrange
    let expected_constraints = [
        Constraint::Fill(1),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ];

    // Act
    let constraints = project_dashboard_panel_constraints();

    // Assert
    assert_eq!(constraints, expected_constraints);
}

#[test]
fn test_dashboard_panel_height_matches_heatmap() {
    // Arrange
    let expected_height = 10;

    // Act
    let panel_height = PROJECT_DASHBOARD_PANEL_HEIGHT;

    // Assert
    assert_eq!(panel_height, expected_height);
}

#[test]
fn test_render_uses_palette_border_for_projects_table() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
    let projects = vec![ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
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
            ProjectListPage::new(
                &projects,
                &[],
                &activity,
                &mut table_state,
                42,
                TEST_ACTIVITY_DAY_KEY,
            )
            .render(frame, frame.area());
        })
        .expect("failed to draw projects page");

    // Assert
    let border_cell_count = foreground_symbol_cell_count(terminal.backend().buffer(), "┌");
    assert!(
        border_cell_count >= 4,
        "expected heatmap, work-pace, agent CLI, and projects panels to use palette border color"
    );
}

#[test]
fn test_project_row_values_show_project_name_and_branch() {
    // Arrange
    let project_item = ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: Some(20),
        output_tokens: 0,
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
    let values = project_row_values(&project_item, 99, None);

    // Assert
    assert_eq!(values.0.to_string(), "agentty");
    assert_eq!(values.1.to_string(), "main");
    assert_eq!(values.2.to_string(), "/tmp/agentty");
}

#[test]
fn test_project_row_values_keep_branch_column_plain() {
    // Arrange
    let project_item = ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: Some(20),
        output_tokens: 0,
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
    let values = project_row_values(&project_item, 99, None);

    // Assert
    assert_eq!(values.1.spans[0].content.as_ref(), "main");
    assert_eq!(values.1.spans[0].style.fg, None);
}

#[test]
fn test_project_row_values_use_fallback_for_missing_branch() {
    // Arrange
    let project_item = ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
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
    let values = project_row_values(&project_item, 99, None);

    // Assert
    assert_eq!(values.0.to_string(), "agentty");
    assert_eq!(values.1.to_string(), "-");
    assert_eq!(values.2.to_string(), "/tmp/agentty");
}

#[test]
fn test_project_row_values_shortens_home_directory_path() {
    // Arrange
    let home_directory = PathBuf::from("/home/test-user");
    let project_item = ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
        project: Project {
            created_at: 1,
            display_name: Some("agentty".to_string()),
            git_branch: Some("main".to_string()),
            id: 1,
            is_favorite: false,
            last_opened_at: None,
            path: home_directory.join("workspace").join("agentty"),
            updated_at: 2,
        },
        session_count: 0,
    };

    // Act
    let values = project_row_values(&project_item, 99, Some(home_directory.as_path()));

    // Assert
    assert_eq!(values.2.to_string(), "~/workspace/agentty");
}

#[test]
fn test_display_project_path_uses_tilde_for_home_directory() {
    // Arrange
    let home_directory = PathBuf::from("/home/test-user");

    // Act
    let display_path = display_project_path(home_directory.as_path(), Some(&home_directory));

    // Assert
    assert_eq!(display_path, "~");
}

#[test]
fn test_display_project_path_keeps_paths_outside_home_absolute() {
    // Arrange
    let home_directory = PathBuf::from("/home/test-user");
    let project_path = PathBuf::from("/home/test-user-other/agentty");

    // Act
    let display_path = display_project_path(project_path.as_path(), Some(&home_directory));

    // Assert
    assert_eq!(display_path, "/home/test-user-other/agentty");
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
        input_tokens: 0,
        last_session_updated_at: Some(20),
        output_tokens: 0,
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
    let values = project_row_values(&project_item, 42, None);

    // Assert
    assert_eq!(values.0.to_string(), "* agentty");
}

#[test]
fn test_left_aligned_line_marks_table_content_left() {
    // Arrange
    let line = Line::from("Project");

    // Act
    let aligned_line = left_aligned_line(line);

    // Assert
    assert_eq!(aligned_line.alignment, Some(Alignment::Left));
}

#[test]
fn test_project_row_style_uses_accent_for_active_project() {
    // Arrange
    let project_item = ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
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
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
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
    let expected_line =
        crate::ui::help_format::footer_line(&help_action::project_list_footer_actions());

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
        input_tokens: 12_345,
        last_session_updated_at: None,
        output_tokens: 5_678,
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
        day_key: TEST_ACTIVITY_DAY_KEY,
        session_count: 3,
    }];
    let agent_clis = vec![AgentCliInfo::new(
        AgentKind::Claude,
        Some("2.1.39".to_string()),
    )];
    let mut table_state = TableState::default();
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| {
            ProjectListPage::new(
                &projects,
                &agent_clis,
                &activity,
                &mut table_state,
                42,
                TEST_ACTIVITY_DAY_KEY,
            )
            .render(frame, frame.area());
        })
        .expect("failed to draw projects page");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("┐┌"));
    assert!(text.contains("Work Pace"));
    assert!(text.contains("Agent CLIs"));
    assert!(text.contains("claude 2.1.39"));
    assert!(!text.contains("Version"));
    assert!(!text.contains("Agentic Development Environment"));
    assert!(text.contains("Active Sessions 0"));
    assert!(text.contains("Active Projects 0"));
    assert!(text.contains("Tokens In 12.3k"));
    assert!(text.contains("Out 5.7k"));
    assert!(!text.contains("Daily Pace"));
}

#[test]
fn test_build_heatmap_lines_uses_persisted_activity_for_intensity() {
    // Arrange
    let activity = vec![DailyActivity {
        day_key: TEST_ACTIVITY_DAY_KEY,
        session_count: 50,
    }];
    let mut table_state = TableState::default();
    let page = ProjectListPage::new(
        &[],
        &[],
        &activity,
        &mut table_state,
        42,
        TEST_ACTIVITY_DAY_KEY,
    );

    // Act
    let heatmap_lines = page.build_heatmap_lines(80);

    // Assert
    assert_eq!(heatmap_lines.len(), usize::from(HEATMAP_CONTENT_HEIGHT));
    assert!(heatmap_lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.style.bg == Some(ProjectListPage::heatmap_color(4)))
    }));
}

#[test]
fn test_build_heatmap_lines_trims_visible_weeks_on_narrow_width() {
    // Arrange
    let activity = vec![DailyActivity {
        day_key: TEST_ACTIVITY_DAY_KEY,
        session_count: 1,
    }];
    let mut table_state = TableState::default();
    let page = ProjectListPage::new(
        &[],
        &[],
        &activity,
        &mut table_state,
        42,
        TEST_ACTIVITY_DAY_KEY,
    );

    // Act
    let heatmap_lines = page.build_heatmap_lines(28);
    let monday_row = &heatmap_lines[1];

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
        input_tokens: 12_345,
        output_tokens: 5_678,
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
    assert!(rendered_text.contains("Tokens In 12.3k"));
    assert!(rendered_text.contains("Out 5.7k"));
}

/// Ensures project summaries show agent executable names with detected CLI
/// versions or an unknown fallback.
#[test]
fn test_agent_cli_summary_lines_show_versions_and_unknown_fallback() {
    // Arrange
    let agent_clis = vec![
        AgentCliInfo::new(AgentKind::Antigravity, Some("0.39.1".to_string())),
        AgentCliInfo::new(AgentKind::Codex, None),
    ];

    // Act
    let rendered_text = agent_cli_summary_lines(&agent_clis)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(rendered_text.contains("agy 0.39.1"));
    assert!(rendered_text.contains("codex version unknown"));
}

#[test]
fn test_agent_cli_summary_lines_show_update_loading_state() {
    // Arrange
    let agent_clis = vec![AgentCliInfo::loading(AgentKind::Claude)];

    // Act
    let rendered_text = agent_cli_summary_lines(&agent_clis)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(rendered_text.contains("claude updating..."));
}

#[test]
fn test_active_project_stats_counts_active_sessions_and_projects() {
    // Arrange
    let projects = vec![
        ProjectListItem {
            active_session_count: 0,
            input_tokens: 100,
            last_session_updated_at: None,
            output_tokens: 25,
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
            input_tokens: 1_000,
            last_session_updated_at: None,
            output_tokens: 250,
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
            input_tokens: 2_000,
            last_session_updated_at: None,
            output_tokens: 500,
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
    assert_eq!(stats.input_tokens, 3_100);
    assert_eq!(stats.output_tokens, 775);
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
