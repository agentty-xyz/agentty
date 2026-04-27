use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::app::setting::SettingsManager;
use crate::ui::state::help_action;
use crate::ui::{Page, style};

/// Uses row-background highlighting without a textual cursor glyph.
const ROW_HIGHLIGHT_SYMBOL: &str = "";
/// Horizontal spacing between settings-table columns.
const TABLE_COLUMN_SPACING: u16 = 2;

/// Renders the settings page table and inline editing hints.
pub struct SettingsPage<'a> {
    manager: &'a mut SettingsManager,
    project_name: Option<String>,
}

impl<'a> SettingsPage<'a> {
    /// Creates a settings page renderer bound to the active project settings.
    pub fn new(manager: &'a mut SettingsManager, project_name: Option<String>) -> Self {
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
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let main_area = chunks[0];
        let footer_area = chunks[1];

        let selected_style = Style::default().bg(style::palette::surface());
        let global_rows = settings_table_rows(self.manager.global_settings_rows());
        let project_rows = settings_table_rows(self.manager.project_settings_rows());

        let project_section_title = self.project_section_title();
        let global_section_title = "Global settings".to_string();

        let global_table_state =
            section_table_state(&self.manager.table_state, 0, global_rows.len());
        let project_table_state = section_table_state(
            &self.manager.table_state,
            global_rows.len(),
            project_rows.len(),
        );

        let section_heights = [
            settings_section_height(global_rows.len()),
            settings_section_height(project_rows.len()),
        ];
        let table_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(section_heights)
            .split(main_area);

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

        f.render_widget(footer, footer_area);
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
    table_state: &TableState,
    section_start: usize,
    section_len: usize,
) -> TableState {
    let mut section_table_state = TableState::default();
    let Some(selected) = table_state.selected() else {
        return section_table_state;
    };

    let section_end = section_start.saturating_add(section_len);
    if (section_start..section_end).contains(&selected) {
        section_table_state.select(Some(selected - section_start));
    }

    section_table_state
}

/// Returns the footer help content for settings mode.
///
/// Inline text editing keeps using the manager-provided hint string, while
/// list mode uses the shared styled help-action rendering.
fn settings_footer_line(manager: &SettingsManager) -> Line<'static> {
    settings_footer_line_for_mode(manager.is_editing_text_input(), manager.footer_hint())
}

/// Returns the footer help content for either list mode or inline-edit mode.
fn settings_footer_line_for_mode(is_editing_text_input: bool, footer_hint: &str) -> Line<'static> {
    if is_editing_text_input {
        return Line::from(footer_hint.to_string());
    }

    let actions = help_action::settings_footer_actions();

    help_action::footer_line(&actions)
}

/// Builds settings table rows with multiline-aware heights.
fn settings_table_rows(settings_rows: Vec<(&'static str, String)>) -> Vec<Row<'static>> {
    settings_rows
        .into_iter()
        .map(|(setting_name, setting_value)| {
            Row::new(vec![
                Cell::from(setting_name),
                Cell::from(setting_value.clone()),
            ])
            .style(Style::default().fg(style::palette::text()))
            .height(settings_row_height(&setting_value))
        })
        .collect()
}

/// Returns the row height needed to render a settings value.
fn settings_row_height(setting_value: &str) -> u16 {
    let row_line_count = setting_value.lines().count().max(1);

    u16::try_from(row_line_count).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let section_state = section_table_state(&table_state, 0, 1);

        // Assert
        assert_eq!(section_state.selected(), Some(0));
    }

    #[test]
    fn test_section_table_state_offsets_project_row_selection() {
        // Arrange
        let mut table_state = TableState::default();
        table_state.select(Some(3));

        // Act
        let section_state = section_table_state(&table_state, 1, 6);

        // Assert
        assert_eq!(section_state.selected(), Some(2));
    }

    #[test]
    fn test_section_table_state_leaves_unselected_rows_unhighlighted() {
        // Arrange
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        // Act
        let section_state = section_table_state(&table_state, 1, 6);

        // Assert
        assert_eq!(section_state.selected(), None);
    }

    #[test]
    fn test_settings_footer_line_uses_inline_hint_while_editing() {
        // Arrange
        let footer_hint = "Editing open commands";

        // Act
        let footer_line = settings_footer_line_for_mode(true, footer_hint);

        // Assert
        assert_eq!(footer_line, Line::from(footer_hint.to_string()));
    }

    #[test]
    fn test_settings_footer_line_uses_shared_actions_in_list_mode() {
        // Arrange
        let footer_hint = "unused while not editing";
        let expected_line = help_action::footer_line(&help_action::settings_footer_actions());

        // Act
        let footer_line = settings_footer_line_for_mode(false, footer_hint);

        // Assert
        assert_eq!(footer_line, expected_line);
    }

    #[test]
    fn test_settings_row_height_expands_for_multiline_values() {
        // Arrange
        let setting_value = "line one\nline two\nline three";

        // Act
        let row_height = settings_row_height(setting_value);

        // Assert
        assert_eq!(row_height, 3);
    }

    #[test]
    fn test_settings_row_height_keeps_empty_values_visible() {
        // Arrange
        let setting_value = "";

        // Act
        let row_height = settings_row_height(setting_value);

        // Assert
        assert_eq!(row_height, 1);
    }

    #[test]
    fn test_settings_row_height_saturates_at_u16_max() {
        // Arrange
        let setting_value = &"\n".repeat(usize::from(u16::MAX) + 1);

        // Act
        let row_height = settings_row_height(setting_value);

        // Assert
        assert_eq!(row_height, u16::MAX);
    }
}
