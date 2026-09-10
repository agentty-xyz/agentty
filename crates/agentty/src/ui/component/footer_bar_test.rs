use std::env;

use super::FooterBar;
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
