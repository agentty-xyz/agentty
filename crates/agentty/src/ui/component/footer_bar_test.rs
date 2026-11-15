use std::env;
use std::path::Path;

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::{FooterBar, centered_label_area};
use crate::domain::theme::ColorTheme;
use crate::ui::{Component, style};

#[test]
fn test_footer_bar_new_with_git_branch() {
    // Arrange
    let path = "/home/user/project".to_string();
    let branch = Some("main".to_string());

    // Act
    let footer = FooterBar::new(path.clone()).git_branch(branch.clone());

    // Assert
    assert_eq!(footer.working_dir, path);
    assert_eq!(footer.git_branch, branch);
    assert_eq!(footer.git_base_ref, None);
    assert_eq!(footer.git_base_status, None);
    assert_eq!(footer.git_status, None);
    assert_eq!(footer.git_upstream_ref, None);
    assert!(footer.workspace_context_visible);
}

#[test]
fn test_footer_bar_new_without_git_branch() {
    // Arrange
    let path = "/home/user/project".to_string();

    // Act
    let footer = FooterBar::new(path.clone());

    // Assert
    assert_eq!(footer.working_dir, path);
    assert_eq!(footer.git_branch, None);
    assert_eq!(footer.git_base_ref, None);
    assert_eq!(footer.git_base_status, None);
    assert_eq!(footer.git_status, None);
    assert_eq!(footer.git_upstream_ref, None);
    assert!(footer.workspace_context_visible);
}

#[test]
fn test_footer_bar_display_path_abbreviates_only_paths_under_home() {
    // Arrange
    let cases = [
        (
            "/home/user/project",
            Some(Path::new("/home/user")),
            "~/project",
        ),
        (
            "/srv/project",
            Some(Path::new("/home/user")),
            "/srv/project",
        ),
        ("/home/user/project", None, "/home/user/project"),
    ];

    for (working_dir, home_dir, expected_display_path) in cases {
        // Act
        let display_path = FooterBar::display_path(working_dir, home_dir);

        // Assert
        assert_eq!(display_path, expected_display_path, "for {working_dir}");
    }
}

#[test]
fn test_footer_bar_render_skips_zero_width_area() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(10, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/project".to_string()).git_branch(Some("main".to_string()));

    // Act
    terminal
        .draw(|f| {
            Component::render(&footer, f, Rect::new(0, 0, 0, 1));
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    assert!(
        buffer
            .content()
            .iter()
            .all(|cell| cell.symbol() == " " && cell.bg == ratatui::style::Color::Reset)
    );
}

#[test]
fn test_footer_bar_render_omits_branch_that_does_not_fit() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(16, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("feature/very-long-branch".to_string()));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert_eq!(text, " /tmp/project   ");
}

#[test]
fn test_footer_bar_render_hides_workspace_context() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(40, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("main".to_string()))
        .workspace_context_visible(false);

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert_eq!(text.trim(), "");
    assert!(!text.contains("/tmp/project"));
    assert!(!text.contains("main"));
    assert!(
        content
            .iter()
            .all(|cell| cell.bg == style::palette::surface())
    );
}

#[test]
fn test_footer_bar_render_with_git_branch() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let path = if let Some(home) = env::home_dir() {
        home.join("project").to_string_lossy().to_string()
    } else {
        "/tmp/project".to_string()
    };
    let footer = FooterBar::new(path).git_branch(Some("main".to_string()));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    if env::home_dir().is_some() {
        assert!(text.contains("~/project"));
    } else {
        assert!(text.contains("/tmp/project"));
    }
    assert!(text.contains("main"));
}

#[test]
fn test_footer_bar_render_uses_path_color_for_branch_text() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(30, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/x".to_string()).git_branch(Some("topic".to_string()));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let path_cell = &buffer[(1, 0)];
    let branch_cell = &buffer[(24, 0)];
    assert_eq!(path_cell.symbol(), "/");
    assert_eq!(branch_cell.symbol(), "t");
    assert_eq!(branch_cell.fg, path_cell.fg);
}

#[test]
fn test_footer_bar_render_with_git_status() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let path = "/tmp/project".to_string();
    // 1 ahead, 2 behind
    let footer = FooterBar::new(path)
        .git_branch(Some("main".to_string()))
        .git_status(Some((1, 2)));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("↓2 ↑1"));
    assert!(text.contains("main"));
}

#[test]
fn test_footer_bar_render_with_git_upstream_reference() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("main".to_string()))
        .git_upstream_ref(Some("origin/main".to_string()));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("main -> origin/main"));
}

#[test]
fn test_footer_bar_render_with_base_and_remote_statuses() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("wt/session".to_string()))
        .git_base_ref(Some("main".to_string()))
        .git_base_status(Some((1, 2)))
        .git_status(Some((3, 4)))
        .git_upstream_ref(Some("origin/wt/session".to_string()));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("↓2 ↑1 main"));
    assert!(text.contains("| ↓4 ↑3 wt/session -> origin/wt/session"));
}

#[test]
fn test_footer_bar_render_with_base_and_local_statuses_without_upstream() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(120, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("wt/session".to_string()))
        .git_base_ref(Some("main".to_string()))
        .git_base_status(Some((1, 2)));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("↓2 ↑1 main"));
    assert!(text.contains("| ✓ wt/session"));
}

#[test]
fn test_footer_bar_render_without_git_branch() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 3);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let path = "/tmp/other-project".to_string();
    let footer = FooterBar::new(path);

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("/tmp/other-project"));
}

#[test]
fn test_footer_bar_render_shows_git_status_when_unicode_width_exactly_fits() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(20, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/p".to_string())
        .git_branch(Some("main".to_string()))
        .git_status(Some((1, 2)));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("↓2 ↑1"));
    assert!(text.contains("main"));
}

#[test]
fn test_footer_bar_render_clears_stale_branch_cells_on_redraw() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(40, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer_with_branch = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("main".to_string()))
        .git_status(Some((0, 0)));
    let footer_without_branch = FooterBar::new("/tmp/other".to_string());

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer_with_branch, f, area);
        })
        .expect("failed to draw");
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer_without_branch, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let content = buffer.content();
    let text: String = content.iter().map(ratatui::buffer::Cell::symbol).collect();
    assert!(text.contains("/tmp/other"));
    assert!(!text.contains("main"));
    assert!(!text.contains("✓"));
}

#[test]
fn test_footer_bar_render_keeps_one_space_after_branch_name() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(40, 1);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let footer = FooterBar::new("/tmp/p".to_string())
        .git_branch(Some("main".to_string()))
        .git_upstream_ref(Some("origin/main".to_string()));

    // Act
    terminal
        .draw(|f| {
            let area = f.area();
            Component::render(&footer, f, area);
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let last_column_index = buffer.area.width.saturating_sub(1);
    let second_to_last_column_index = last_column_index.saturating_sub(1);
    let last_symbol = buffer[(last_column_index, 0)].symbol();
    let second_to_last_symbol = buffer[(second_to_last_column_index, 0)].symbol();
    assert_eq!(last_symbol, " ");
    assert_eq!(second_to_last_symbol, "n");
}

/// Renders `footer` into a one-row terminal of `width` columns and returns
/// the terminal for buffer inspection.
fn rendered_footer(footer: &FooterBar, width: u16) -> ratatui::Terminal<TestBackend> {
    let mut terminal =
        ratatui::Terminal::new(TestBackend::new(width, 1)).expect("failed to create terminal");
    terminal
        .draw(|f| Component::render(footer, f, f.area()))
        .expect("failed to draw");

    terminal
}

/// Returns the first row of `terminal` as plain text.
fn footer_text(terminal: &ratatui::Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn test_footer_bar_render_centers_project_name_between_path_and_branch() {
    // Arrange
    let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
    let footer = FooterBar::new("/tmp/p".to_string())
        .git_branch(Some("main".to_string()))
        .project_name(Some("agentty".to_string()));

    // Act
    let terminal = rendered_footer(&footer, 60);

    // Assert
    let text = footer_text(&terminal);
    let buffer = terminal.backend().buffer();
    assert!(text.starts_with(" /tmp/p "));
    assert!(text.ends_with("● main "));
    assert_eq!(text.find("agentty"), Some(26));
    assert_eq!(buffer[(26, 0)].fg, style::palette::accent_soft());
    assert_eq!(buffer[(26, 0)].bg, style::palette::surface());
    assert!(buffer[(26, 0)].modifier.contains(Modifier::BOLD));
}

#[test]
fn test_footer_bar_render_shortens_then_omits_project_name_before_path_and_branch() {
    // Arrange
    let footer = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("main".to_string()))
        .project_name(Some("my-long-project-name".to_string()));

    // Act
    let shortened = footer_text(&rendered_footer(&footer, 40));
    let omitted = footer_text(&rendered_footer(&footer, 24));

    // Assert
    assert_eq!(shortened, " /tmp/project my-long-project... ● main ");
    assert_eq!(omitted, " /tmp/project    ● main ");
}

#[test]
fn test_footer_bar_render_keeps_joined_emoji_project_name_that_fits() {
    // Arrange
    let footer = FooterBar::new("/tmp/project".to_string())
        .git_branch(Some("main".to_string()))
        .project_name(Some("👩‍💻".to_string()));

    // Act
    let terminal = rendered_footer(&footer, 40);

    // Assert
    let text = footer_text(&terminal);
    assert!(text.contains("👩‍💻"), "footer was {text:?}");
    assert!(!text.contains(".."), "footer was {text:?}");
}

#[test]
fn test_footer_bar_render_hides_project_name_with_workspace_context() {
    // Arrange
    let footer = FooterBar::new("/tmp/project".to_string())
        .project_name(Some("agentty".to_string()))
        .workspace_context_visible(false);

    // Act
    let text = footer_text(&rendered_footer(&footer, 40));

    // Assert
    assert_eq!(text.trim(), "");
}

#[test]
fn test_centered_label_area_centers_label_across_full_width() {
    // Arrange
    let (area_width, left_width, right_width, label_width) = (60, 16, 8, 7);

    // Act
    let label_area = centered_label_area(area_width, left_width, right_width, label_width);

    // Assert
    assert_eq!(label_area, Some((26, 7)));
}

#[test]
fn test_centered_label_area_moves_label_into_free_gap_near_long_left_text() {
    // Arrange
    let (area_width, left_width, right_width, label_width) = (60, 30, 8, 10);

    // Act
    let label_area = centered_label_area(area_width, left_width, right_width, label_width);

    // Assert
    assert_eq!(label_area, Some((36, 10)));
}

#[test]
fn test_centered_label_area_shortens_label_to_free_gap() {
    // Arrange, Act
    let shortened = centered_label_area(40, 16, 8, 20);
    let minimum = centered_label_area(30, 16, 8, 10);
    let whole_short_label = centered_label_area(29, 16, 8, 2);

    // Assert
    assert_eq!(shortened, Some((17, 14)));
    assert_eq!(minimum, Some((17, 4)));
    assert_eq!(whole_short_label, Some((17, 2)));
}

#[test]
fn test_centered_label_area_omits_label_without_room() {
    // Arrange, Act
    let below_minimum = centered_label_area(29, 16, 8, 10);
    let no_gap = centered_label_area(10, 16, 0, 5);
    let empty_label = centered_label_area(60, 16, 8, 0);

    // Assert
    assert_eq!(below_minimum, None);
    assert_eq!(no_gap, None);
    assert_eq!(empty_label, None);
}
