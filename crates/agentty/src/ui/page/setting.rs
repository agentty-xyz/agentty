use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};

use crate::app::setting::{SettingsManager, SettingsSelectorDropdown};
use crate::presentation::help_action;
use crate::ui::text_util::truncate_with_ellipsis;
use crate::ui::{Component, Page, component, layout, overlay, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
/// Horizontal spacing between settings-table columns.
const TABLE_COLUMN_SPACING: u16 = 2;
/// Minimum dropdown height that leaves room for one option and the help hint.
const DROPDOWN_MIN_HEIGHT: u16 = 6;
/// Minimum dropdown width sized for setting values and selector hints.
const DROPDOWN_MIN_WIDTH: u16 = 36;
/// Percentage of the active section width used by the dropdown panel.
const DROPDOWN_WIDTH_PERCENT: u16 = 58;

/// Renders the settings page table and inline editing hints.
pub struct SettingsPage<'a> {
    manager: &'a SettingsManager,
    project_name: Option<String>,
}

impl<'a> SettingsPage<'a> {
    /// Creates a settings page renderer bound to the active project settings.
    pub fn new(manager: &'a SettingsManager, project_name: Option<String>) -> Self {
        Self {
            manager,
            project_name,
        }
    }

    /// Returns the title for the active project's settings section.
    fn project_section_title(&self) -> String {
        project_section_title(self.project_name.as_deref())
    }
}

impl Page for SettingsPage<'_> {
    /// Renders the global and project settings tables with compact tab-page
    /// spacing.
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let areas = layout::tab_page_areas(area);

        let selected_style = Style::default().bg(style::palette::surface_selection());
        let global_rows = settings_table_rows(self.manager.global_settings_rows());
        let project_rows = settings_table_rows(self.manager.project_settings_rows());
        let global_row_count = global_rows.len();

        let project_section_title = self.project_section_title();
        let global_section_title = "Global settings".to_string();

        let global_table_state =
            section_table_state(self.manager.table_state.selected(), 0, global_row_count);
        let project_table_state = section_table_state(
            self.manager.table_state.selected(),
            global_row_count,
            project_rows.len(),
        );

        let section_heights = [
            settings_section_height(global_row_count),
            settings_section_height(project_rows.len()),
        ];
        let table_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(section_heights)
            .split(areas.main_area);

        let table_columns = [Constraint::Percentage(50), Constraint::Percentage(50)];

        let global_table = Table::new(global_rows, table_columns)
            .column_spacing(TABLE_COLUMN_SPACING)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(global_section_title)
                    .border_style(style::border_style()),
            )
            .row_highlight_style(selected_style)
            .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        let project_table = Table::new(project_rows, table_columns)
            .column_spacing(TABLE_COLUMN_SPACING)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(project_section_title)
                    .border_style(style::border_style()),
            )
            .row_highlight_style(selected_style)
            .highlight_symbol(ROW_HIGHLIGHT_SYMBOL);

        let mut global_table_state = global_table_state;
        let mut project_table_state = project_table_state;
        f.render_stateful_widget(global_table, table_chunks[0], &mut global_table_state);
        f.render_stateful_widget(project_table, table_chunks[1], &mut project_table_state);

        let footer = Paragraph::new(settings_footer_line(self.manager));

        f.render_widget(footer, areas.footer_area);

        if let Some(selector_dropdown) = self.manager.selector_dropdown() {
            render_settings_selector_dropdown(
                f,
                areas.main_area,
                &table_chunks,
                global_row_count,
                &selector_dropdown,
            );
        }

        if let Some(launch_configuration_editor) = self.manager.launch_configuration_list_editor() {
            component::launch_configuration_list_editor::LaunchConfigurationListEditor::new(
                &launch_configuration_editor,
            )
            .render(f, areas.main_area);
        }
    }
}

/// Fixed table border height added to every settings section.
const SETTINGS_SECTION_PADDING: usize = 2;

/// Formats the active project's section title.
fn project_section_title(project_name: Option<&str>) -> String {
    match project_name {
        Some(name) => format!("'{name}' settings"),
        None => "Project settings".to_string(),
    }
}

/// Returns the table-height constraint for one settings section.
fn settings_section_height(setting_row_count: usize) -> Constraint {
    let total_row_count = setting_row_count.saturating_add(SETTINGS_SECTION_PADDING);
    let constraint_height = u16::try_from(total_row_count).unwrap_or(u16::MAX);

    Constraint::Length(constraint_height)
}

/// Projects the shared settings selection into a section-local table state.
fn section_table_state(
    selected: Option<usize>,
    section_start: usize,
    section_len: usize,
) -> TableState {
    let mut section_table_state = TableState::default();
    let Some(selected) = selected else {
        return section_table_state;
    };

    let section_end = section_start.saturating_add(section_len);
    if (section_start..section_end).contains(&selected) {
        section_table_state.select(Some(selected - section_start));
    }

    section_table_state
}

/// Renders the open settings selector dropdown over the active settings
/// section.
fn render_settings_selector_dropdown(
    f: &mut Frame,
    main_area: Rect,
    table_chunks: &[Rect],
    global_row_count: usize,
    selector_dropdown: &SettingsSelectorDropdown,
) {
    let popup_area = settings_selector_dropdown_area(
        main_area,
        table_chunks,
        global_row_count,
        selector_dropdown,
    );
    let lines =
        settings_selector_dropdown_lines(selector_dropdown, popup_area.width, popup_area.height);

    let dropdown = Paragraph::new(lines)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true })
        .block(overlay::overlay_block(
            "Select setting value",
            style::palette::accent(),
        ));

    overlay::clear_popup_area(f, popup_area);
    f.render_widget(dropdown, popup_area);
}

/// Calculates the dropdown panel area from the selected row and section.
fn settings_selector_dropdown_area(
    main_area: Rect,
    table_chunks: &[Rect],
    global_row_count: usize,
    selector_dropdown: &SettingsSelectorDropdown,
) -> Rect {
    let (section_area, local_row_index) =
        selected_dropdown_section(table_chunks, global_row_count, selector_dropdown.row_index);
    let width = settings_selector_dropdown_width(main_area, section_area);
    let height = settings_selector_dropdown_height(main_area, selector_dropdown.options.len());
    let row_y = section_area
        .y
        .saturating_add(1)
        .saturating_add(u16::try_from(local_row_index).unwrap_or(u16::MAX));

    let preferred_x = section_area.x.saturating_add(section_area.width / 2);
    let preferred_y = row_y.saturating_add(1);
    let max_x = main_area
        .x
        .saturating_add(main_area.width.saturating_sub(width));
    let max_y = main_area
        .y
        .saturating_add(main_area.height.saturating_sub(height));
    let x = preferred_x.min(max_x).max(main_area.x);
    let y = preferred_y.min(max_y).max(main_area.y);

    Rect::new(x, y, width, height)
}

/// Returns the section rectangle and local row index for the dropdown row.
fn selected_dropdown_section(
    table_chunks: &[Rect],
    global_row_count: usize,
    row_index: usize,
) -> (Rect, usize) {
    if row_index < global_row_count {
        return (table_chunks[0], row_index);
    }

    (table_chunks[1], row_index.saturating_sub(global_row_count))
}

/// Returns the dropdown width bounded to the visible page area.
fn settings_selector_dropdown_width(main_area: Rect, section_area: Rect) -> u16 {
    let section_width = section_area.width.saturating_sub(TABLE_COLUMN_SPACING);
    let preferred_width = section_width
        .saturating_mul(DROPDOWN_WIDTH_PERCENT)
        .saturating_div(100);

    preferred_width
        .max(DROPDOWN_MIN_WIDTH)
        .min(section_width)
        .min(main_area.width)
        .max(1)
}

/// Returns the dropdown height required for options and footer hint.
fn settings_selector_dropdown_height(main_area: Rect, option_count: usize) -> u16 {
    let inner_line_count = option_count.saturating_add(2);
    let required_height = overlay::overlay_required_height(inner_line_count);

    required_height
        .max(DROPDOWN_MIN_HEIGHT)
        .min(main_area.height)
        .max(1)
}

/// Builds styled dropdown lines for the visible selector options and the help
/// hint.
fn settings_selector_dropdown_lines(
    selector_dropdown: &SettingsSelectorDropdown,
    popup_width: u16,
    popup_height: u16,
) -> Vec<Line<'static>> {
    let label_width = overlay::overlay_content_width(popup_width)
        .saturating_sub(2)
        .max(1);
    let option_count = selector_dropdown.options.len();
    let selected_index = selector_dropdown
        .selected_index
        .min(option_count.saturating_sub(1));
    let visible_option_count = settings_selector_visible_option_count(popup_height, option_count);
    let window_start =
        settings_selector_option_window_start(option_count, selected_index, visible_option_count);
    let window_end = window_start
        .saturating_add(visible_option_count)
        .min(option_count);
    let mut lines: Vec<Line<'static>> = selector_dropdown
        .options
        .iter()
        .enumerate()
        .skip(window_start)
        .take(window_end.saturating_sub(window_start))
        .map(|(option_index, option)| {
            settings_selector_dropdown_option_line(
                option.label.as_str(),
                label_width,
                selected_index == option_index,
            )
        })
        .collect();

    lines.push(Line::from(""));
    lines.push(
        Line::from(vec![Span::styled(
            "j/k: move | Enter: select | Esc/q: close",
            Style::default().fg(style::palette::text_muted()),
        )])
        .alignment(Alignment::Center),
    );

    lines
}

/// Returns how many option rows fit inside the dropdown content area.
fn settings_selector_visible_option_count(popup_height: u16, option_count: usize) -> usize {
    let dropdown_chrome_height = overlay::overlay_required_height(0);
    let content_height = popup_height.saturating_sub(dropdown_chrome_height);
    let footer_line_count = 2;
    let visible_option_count = usize::from(content_height).saturating_sub(footer_line_count);

    visible_option_count.max(1).min(option_count)
}

/// Returns the first option index for a bounded dropdown window centered near
/// the selected option.
fn settings_selector_option_window_start(
    option_count: usize,
    selected_index: usize,
    visible_option_count: usize,
) -> usize {
    if option_count <= visible_option_count {
        return 0;
    }

    let centered_start = selected_index.saturating_sub(visible_option_count / 2);

    centered_start.min(option_count.saturating_sub(visible_option_count))
}

/// Builds one option line for the settings selector dropdown.
fn settings_selector_dropdown_option_line(
    option_label: &str,
    label_width: usize,
    is_selected: bool,
) -> Line<'static> {
    let option_label = truncate_with_ellipsis(option_label, label_width);

    if is_selected {
        let selected_label = format!("> {option_label:<label_width$}");

        return Line::from(Span::styled(
            selected_label,
            Style::default()
                .fg(style::palette::surface_overlay())
                .bg(style::palette::accent())
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(vec![
        Span::styled("  ", Style::default().fg(style::palette::text_subtle())),
        Span::styled(option_label, Style::default().fg(style::palette::text())),
    ])
}

/// Returns the footer help content for settings mode.
///
/// Selector dropdowns and command-list editing keep using the
/// manager-provided hint string, while list mode uses the shared styled
/// help-action rendering.
fn settings_footer_line(manager: &SettingsManager) -> Line<'static> {
    settings_footer_line_for_mode(
        manager.is_launch_configuration_list_editor_open() || manager.is_selector_dropdown_open(),
        manager.footer_hint(),
    )
}

/// Returns the footer help content for either list mode or overlay mode.
fn settings_footer_line_for_mode(uses_inline_hint: bool, footer_hint: &str) -> Line<'static> {
    if uses_inline_hint {
        return Line::from(footer_hint.to_string());
    }

    let actions = help_action::settings_footer_actions();

    crate::ui::help_format::footer_line(&actions)
}

/// Builds single-line settings table rows.
fn settings_table_rows(settings_rows: Vec<(&'static str, String)>) -> Vec<Row<'static>> {
    settings_rows
        .into_iter()
        .map(|(setting_name, setting_value)| {
            Row::new(vec![Cell::from(setting_name), Cell::from(setting_value)])
                .style(Style::default().fg(style::palette::text()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::setting::SettingsSelectorDropdownOption;

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
    fn test_settings_table_column_spacing_is_wider_for_readability() {
        // Arrange
        let expected_spacing = 2;

        // Act
        let spacing = TABLE_COLUMN_SPACING;

        // Assert
        assert_eq!(spacing, expected_spacing);
    }

    #[test]
    fn test_render_uses_palette_text_for_setting_rows() {
        // Arrange
        let rows = settings_table_rows(vec![("Theme", "Agentty Default".to_string())]);
        let table = Table::new(
            rows,
            [Constraint::Percentage(50), Constraint::Percentage(50)],
        );
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                frame.render_widget(table, frame.area());
            })
            .expect("failed to draw settings page");

        // Assert
        let buffer = terminal.backend().buffer();
        let theme_cell = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "T" && cell.fg == style::palette::text())
            .expect("expected Theme row to use palette text");
        assert_eq!(theme_cell.fg, style::palette::text());
    }

    #[test]
    fn test_project_section_title_wraps_project_name_in_quotes() {
        // Arrange
        let project_name = Some("Agentty");

        // Act
        let section_title = project_section_title(project_name);

        // Assert
        assert_eq!(section_title, "'Agentty' settings");
    }

    #[test]
    fn test_project_section_title_falls_back_without_project_name() {
        // Arrange
        let project_name = None;

        // Act
        let section_title = project_section_title(project_name);

        // Assert
        assert_eq!(section_title, "Project settings");
    }

    #[test]
    fn test_settings_section_height_includes_table_chrome() {
        // Arrange
        let setting_row_count = 6;

        // Act
        let height = settings_section_height(setting_row_count);

        // Assert
        assert_eq!(height, Constraint::Length(8));
    }

    #[test]
    fn test_section_table_state_selects_global_row() {
        // Arrange
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        // Act
        let section_state = section_table_state(table_state.selected(), 0, 1);

        // Assert
        assert_eq!(section_state.selected(), Some(0));
    }

    #[test]
    fn test_section_table_state_offsets_project_row_selection() {
        // Arrange
        let mut table_state = TableState::default();
        table_state.select(Some(3));

        // Act
        let section_state = section_table_state(table_state.selected(), 1, 6);

        // Assert
        assert_eq!(section_state.selected(), Some(2));
    }

    #[test]
    fn test_section_table_state_leaves_unselected_rows_unhighlighted() {
        // Arrange
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        // Act
        let section_state = section_table_state(table_state.selected(), 1, 6);

        // Assert
        assert_eq!(section_state.selected(), None);
    }

    #[test]
    fn test_settings_selector_dropdown_lines_highlight_selected_option() {
        // Arrange
        let selector_dropdown = SettingsSelectorDropdown {
            options: vec![
                SettingsSelectorDropdownOption {
                    label: "Agentty Default".to_string(),
                },
                SettingsSelectorDropdownOption {
                    label: "Agentty Green".to_string(),
                },
            ],
            row_index: 0,
            selected_index: 1,
        };

        // Act
        let lines = settings_selector_dropdown_lines(&selector_dropdown, 48, 8);

        // Assert
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].spans[0].style.bg, Some(style::palette::accent()));
        assert!(lines[1].to_string().contains("> Agentty Green"));
        assert!(lines[3].to_string().contains("Enter: select"));
    }

    #[test]
    fn test_settings_selector_dropdown_lines_window_to_selected_option() {
        // Arrange
        let selector_dropdown = SettingsSelectorDropdown {
            options: (0..12)
                .map(|option_index| SettingsSelectorDropdownOption {
                    label: format!("Option {option_index}"),
                })
                .collect(),
            row_index: 2,
            selected_index: 10,
        };

        // Act
        let lines = settings_selector_dropdown_lines(&selector_dropdown, 48, 7);
        let dropdown_text = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(dropdown_text.contains("> Option 10"));
        assert!(!dropdown_text.contains("Option 0"));
        assert!(lines.len() <= 5);
    }

    #[test]
    fn test_settings_selector_option_window_start_keeps_tail_selection_visible() {
        // Arrange
        let option_count = 12;
        let selected_index = 11;
        let visible_option_count = 3;

        // Act
        let window_start = settings_selector_option_window_start(
            option_count,
            selected_index,
            visible_option_count,
        );

        // Assert
        assert_eq!(window_start, 9);
    }

    #[test]
    fn test_settings_selector_dropdown_area_stays_in_main_area() {
        // Arrange
        let main_area = Rect::new(0, 0, 80, 18);
        let table_chunks = vec![Rect::new(0, 0, 80, 3), Rect::new(0, 3, 80, 8)];
        let selector_dropdown = SettingsSelectorDropdown {
            options: vec![
                SettingsSelectorDropdownOption {
                    label: "low".to_string(),
                },
                SettingsSelectorDropdownOption {
                    label: "medium".to_string(),
                },
                SettingsSelectorDropdownOption {
                    label: "high".to_string(),
                },
            ],
            row_index: 3,
            selected_index: 2,
        };

        // Act
        let area = settings_selector_dropdown_area(main_area, &table_chunks, 1, &selector_dropdown);

        // Assert
        assert!(area.x >= main_area.x);
        assert!(area.y >= main_area.y);
        assert!(area.x.saturating_add(area.width) <= main_area.x.saturating_add(main_area.width));
        assert!(area.y.saturating_add(area.height) <= main_area.y.saturating_add(main_area.height));
    }

    #[test]
    fn test_settings_footer_line_uses_inline_hint_while_overlay_is_open() {
        // Arrange
        let footer_hint = "Editing launch configurations";

        // Act
        let footer_line = settings_footer_line_for_mode(true, footer_hint);

        // Assert
        assert_eq!(footer_line, Line::from(footer_hint.to_string()));
    }

    #[test]
    fn test_settings_footer_line_uses_shared_actions_in_list_mode() {
        // Arrange
        let footer_hint = "unused while not editing";
        let expected_line =
            crate::ui::help_format::footer_line(&help_action::settings_footer_actions());

        // Act
        let footer_line = settings_footer_line_for_mode(false, footer_hint);

        // Assert
        assert_eq!(footer_line, expected_line);
    }
}
