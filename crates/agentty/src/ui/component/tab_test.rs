//! Header tab spacing, highlighting, and process-stats rendering unit tests.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use super::{Tabs, process_stats_label, tab_spans};
use crate::app::Tab;
use crate::domain::resource::SessionResources;
use crate::ui::{Component, style};

#[test]
fn test_tab_spans_use_equal_spacing_between_labels() {
    // Arrange
    let current_tab = Tab::Projects;

    // Act
    let spans = tab_spans(current_tab, None);
    let rendered_tabs: String = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("");

    // Assert
    assert_eq!(rendered_tabs, " Projects | Sessions | Settings ");
}

#[test]
fn test_tab_spans_highlight_the_active_tab() {
    // Arrange
    let current_tab = Tab::Settings;

    // Act
    let spans = tab_spans(current_tab, None);

    // Assert
    assert_eq!(spans[0].style.fg, Some(style::palette::text_muted()));
    assert_eq!(spans[2].style.fg, Some(style::palette::text_muted()));
    assert_eq!(spans[4].style.fg, Some(style::palette::text_selection()));
    assert_eq!(spans[4].style.bg, Some(style::palette::surface_selection()));
    assert!(spans[4].style.add_modifier.contains(Modifier::BOLD));
    assert!(
        spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
}

#[test]
fn test_tab_spans_render_divider_spans_with_border_color() {
    // Arrange
    let current_tab = Tab::Projects;

    // Act
    let spans = tab_spans(current_tab, None);

    // Assert
    assert_eq!(spans.len(), 5);
    assert_eq!(spans[1].content.as_ref(), "|");
    assert_eq!(spans[3].content.as_ref(), "|");
    assert_eq!(spans[1].style.fg, Some(style::palette::border()));
    assert_eq!(spans[3].style.fg, Some(style::palette::border()));
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
    let tabs = Tabs::new(Tab::Sessions).process_stats(Some(resources));

    // Act
    let wide_row = rendered_tab_row(&tabs, 80);
    let narrow_row = rendered_tab_row(&tabs, 50);

    // Assert
    assert!(wide_row.starts_with(" Projects | Sessions | Settings "));
    assert!(wide_row.ends_with("Processes: 2  CPU: 12.5%  RAM: 3.0 MiB"));
    assert!(narrow_row.starts_with(" Projects | Sessions | Settings "));
    assert!(narrow_row.ends_with("  Processes: 2"));
}

#[test]
fn test_tabs_omit_process_stats_before_the_first_sample() {
    // Arrange
    let tabs = Tabs::new(Tab::Sessions);

    // Act
    let tab_row = rendered_tab_row(&tabs, 100);

    // Assert
    assert_eq!(tab_row.trim_end(), " Projects | Sessions | Settings");
}

#[test]
fn test_tabs_show_the_remote_tab_under_its_label_when_offered() {
    // Arrange
    let inactive = Tabs::new(Tab::Sessions).remote_label(Some("Remote (1 online)".to_string()));
    let active_spans = tab_spans(Tab::Remote, Some("Remote"));

    // Act
    let tab_row = rendered_tab_row(&inactive, 100);

    // Assert
    assert_eq!(
        tab_row.trim_end(),
        " Projects | Sessions | Settings | Remote (1 online)"
    );
    assert_eq!(active_spans.len(), 7);
    assert_eq!(active_spans[6].content.as_ref(), " Remote ");
    assert_eq!(active_spans[6].style, style::selection_style());
    assert_eq!(active_spans[4].style.fg, Some(style::palette::text_muted()));
}

#[test]
fn test_tabs_fill_their_height_with_spacer_tab_row_and_bottom_rule() {
    // Arrange
    let tabs = Tabs::new(Tab::Sessions);

    // Act
    let rows = rendered_rows(&tabs, 40);

    // Assert
    assert_eq!(rows.len(), usize::from(Tabs::HEIGHT));
    assert_eq!(rows[0], " ".repeat(40));
    assert_eq!(rows[1].trim_end(), " Projects | Sessions | Settings");
    assert_eq!(rows[2], "─".repeat(40));
}

/// Renders tabs at `width` columns and returns the tab-label row.
fn rendered_tab_row(tabs: &Tabs, width: u16) -> String {
    rendered_rows(tabs, width).swap_remove(1)
}

/// Renders tabs into a `width` by [`Tabs::HEIGHT`] terminal and returns every
/// row from top to bottom.
fn rendered_rows(tabs: &Tabs, width: u16) -> Vec<String> {
    let mut terminal =
        Terminal::new(TestBackend::new(width, Tabs::HEIGHT)).expect("create terminal");
    terminal
        .draw(|frame| tabs.render(frame, frame.area()))
        .expect("draw tabs");
    let buffer = terminal.backend().buffer();

    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect()
        })
        .collect()
}
