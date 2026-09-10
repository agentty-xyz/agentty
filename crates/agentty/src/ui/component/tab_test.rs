use std::path::PathBuf;

use ratatui::style::Modifier;

use super::tab_spans;
use crate::app::Tab;
use crate::domain::project::{Project, ProjectListItem};
use crate::ui::style;

#[test]
fn test_tab_spans_use_equal_spacing_between_labels() {
    // Arrange
    let current_tab = Tab::Projects;

    // Act
    let spans = tab_spans(current_tab, 0, &[]);
    let rendered_tabs: String = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("");

    // Assert
    assert_eq!(
        rendered_tabs,
        " Projects | Project: None | Sessions | Settings "
    );
}

#[test]
fn test_tab_spans_highlight_the_active_tab() {
    // Arrange
    let current_tab = Tab::Settings;

    // Act
    let spans = tab_spans(current_tab, 0, &[]);

    // Assert
    assert_eq!(spans[0].style.fg, Some(style::palette::text_muted()));
    assert_eq!(spans[2].style.fg, Some(style::palette::text_subtle()));
    assert_eq!(spans[4].style.fg, Some(style::palette::text_muted()));
    assert_eq!(spans[6].style.fg, Some(style::palette::warning()));
    assert_eq!(spans[6].style.bg, Some(style::palette::surface()));
    assert!(spans[6].style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn test_tab_spans_include_selected_project_name_in_project_scope_label() {
    // Arrange
    let current_tab = Tab::Sessions;
    let projects = vec![
        project_list_item(7, Some("Primary"), "/tmp/primary"),
        project_list_item(8, Some("Secondary"), "/tmp/secondary"),
    ];

    // Act
    let spans = tab_spans(current_tab, 7, &projects);
    let rendered_tabs: String = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("");

    // Assert
    assert_eq!(
        rendered_tabs,
        " Projects | Project: Primary | Sessions | Settings "
    );
    assert_eq!(spans[2].style.fg, Some(style::palette::accent_soft()));
    assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(spans[4].style.fg, Some(style::palette::warning()));
    assert_eq!(spans[4].style.bg, Some(style::palette::surface()));
}

#[test]
fn test_tab_spans_render_divider_spans_with_border_color() {
    // Arrange
    let current_tab = Tab::Projects;

    // Act
    let spans = tab_spans(current_tab, 0, &[]);

    // Assert
    assert_eq!(spans[1].content.as_ref(), "|");
    assert_eq!(spans[3].content.as_ref(), "|");
    assert_eq!(spans[5].content.as_ref(), "|");
    assert_eq!(spans[1].style.fg, Some(style::palette::border()));
    assert_eq!(spans[3].style.fg, Some(style::palette::border()));
    assert_eq!(spans[5].style.fg, Some(style::palette::border()));
}

#[test]
fn test_tab_spans_dim_project_scope_when_no_project_is_selected() {
    // Arrange
    let current_tab = Tab::Settings;

    // Act
    let spans = tab_spans(current_tab, 0, &[]);

    // Assert
    assert_eq!(spans[2].content.as_ref(), " Project: None ");
    assert_eq!(spans[2].style.fg, Some(style::palette::text_subtle()));
}

/// Creates a `ProjectListItem` for tab-label rendering tests.
fn project_list_item(id: i64, display_name: Option<&str>, path: &str) -> ProjectListItem {
    ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
        project: Project {
            created_at: 0,
            display_name: display_name.map(std::string::ToString::to_string),
            git_branch: None,
            id,
            is_favorite: false,
            last_opened_at: None,
            path: PathBuf::from(path),
            updated_at: 0,
        },
        session_count: 0,
    }
}
