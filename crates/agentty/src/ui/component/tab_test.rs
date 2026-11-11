use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use super::{Tabs, process_stats_label, tab_spans};
use crate::app::Tab;
use crate::domain::project::{Project, ProjectListItem};
use crate::domain::resource::SessionResources;
use crate::ui::{Component, style};

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
    assert_eq!(spans[6].style.fg, Some(style::palette::text_selection()));
    assert_eq!(spans[6].style.bg, Some(style::palette::surface_selection()));
    assert!(spans[6].style.add_modifier.contains(Modifier::BOLD));
    assert!(
        spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
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
    assert_eq!(spans[4].style.fg, Some(style::palette::text_selection()));
    assert_eq!(spans[4].style.bg, Some(style::palette::surface_selection()));
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

#[test]
fn test_process_stats_label_uses_short_keys() {
    // Arrange
    let resources = SessionResources {
        cpu_percent: 128.5,
        process_count: 3,
        resident_memory_kib: 3584,
    };

    // Act
    let available = process_stats_label(Some(resources), 80);
    let narrow = process_stats_label(Some(resources), 30);
    let too_narrow = process_stats_label(Some(resources), 3);
    let unsampled = process_stats_label(None, 80);

    // Assert
    assert_eq!(
        available.as_deref(),
        Some("Processes: 3  CPU: 128.5%  RAM: 3.5 MiB")
    );
    assert_eq!(narrow.as_deref(), Some("Processes: 3  CPU: 128.5%"));
    assert_eq!(too_narrow, None);
    assert_eq!(unsampled, None);
}

#[test]
fn test_tabs_right_align_process_stats_beside_tabs() {
    // Arrange
    let resources = SessionResources {
        cpu_percent: 12.5,
        process_count: 2,
        resident_memory_kib: 3072,
    };
    let tabs = Tabs::new(Tab::Sessions, 0, &[]).process_stats(Some(resources));

    // Act
    let wide_row = rendered_tab_row(&tabs, 100);
    let narrow_row = rendered_tab_row(&tabs, 70);

    // Assert
    assert!(wide_row.starts_with(" Projects | Project: None | Sessions | Settings "));
    assert!(wide_row.ends_with("Processes: 2  CPU: 12.5%  RAM: 3.0 MiB"));
    assert!(narrow_row.starts_with(" Projects | Project: None | Sessions | Settings "));
    assert!(narrow_row.ends_with("  Processes: 2"));
}

#[test]
fn test_tabs_omit_process_stats_before_the_first_sample() {
    // Arrange
    let tabs = Tabs::new(Tab::Sessions, 0, &[]);

    // Act
    let tab_row = rendered_tab_row(&tabs, 100);

    // Assert
    assert_eq!(
        tab_row.trim_end(),
        " Projects | Project: None | Sessions | Settings"
    );
}

/// Renders tabs at `width` columns and returns the tab-label row.
fn rendered_tab_row(tabs: &Tabs<'_>, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 3)).expect("create terminal");
    terminal
        .draw(|frame| tabs.render(frame, frame.area()))
        .expect("draw tabs");
    let buffer = terminal.backend().buffer();

    (0..buffer.area.width)
        .map(|column| buffer[(column, 1)].symbol())
        .collect()
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
