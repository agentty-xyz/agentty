use ag_tui_text::text_util::truncate_with_ellipsis;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::domain::input::InputState;
use crate::presentation::setting::{
    LaunchConfigurationListEditorMode, LaunchConfigurationListEditorSnapshot,
};
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

const FOOTER_LINE_COUNT: usize = 3;
const MIN_OVERLAY_HEIGHT: u16 = 11;
const MIN_OVERLAY_WIDTH: u16 = 58;
/// Popup dimensions for editing the configured launch-configuration list.
const OVERLAY_DIMENSIONS: overlay::OverlayDimensions =
    overlay::OverlayDimensions::new(70, 42, MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT);

/// Centered popup that edits the project-scoped `Launch Configurations` setting
/// as a discrete command list.
pub struct LaunchConfigurationListEditor<'a> {
    editor: &'a LaunchConfigurationListEditorSnapshot,
}

impl<'a> LaunchConfigurationListEditor<'a> {
    /// Creates a launch-configuration list editor popup from render-ready
    /// state.
    pub fn new(editor: &'a LaunchConfigurationListEditorSnapshot) -> Self {
        Self { editor }
    }

    /// Returns all render lines for this popup.
    fn lines(&self, command_width: usize, popup_height: u16) -> Vec<Line<'static>> {
        match self.editor.mode {
            LaunchConfigurationListEditorMode::Browse => {
                self.browse_lines(command_width, popup_height)
            }
            LaunchConfigurationListEditorMode::Add | LaunchConfigurationListEditorMode::Edit => {
                self.input_lines(command_width)
            }
        }
    }

    /// Returns list-browsing render lines.
    fn browse_lines(&self, command_width: usize, popup_height: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if self.editor.commands.is_empty() {
            lines.push(
                Line::from(Span::styled(
                    "(no commands configured)",
                    Style::default().fg(palette::text_muted()),
                ))
                .alignment(Alignment::Center),
            );
        } else {
            let command_count = self.editor.commands.len();
            let selected_index = self
                .editor
                .selected_index
                .min(command_count.saturating_sub(1));
            let visible_command_count =
                visible_command_count(popup_height, command_count).min(command_count);
            let window_start =
                command_window_start(command_count, selected_index, visible_command_count);
            let window_end = window_start
                .saturating_add(visible_command_count)
                .min(command_count);

            lines.extend(
                self.editor
                    .commands
                    .iter()
                    .enumerate()
                    .skip(window_start)
                    .take(window_end.saturating_sub(window_start))
                    .map(|(command_index, command)| {
                        command_line(command, command_width, command_index == selected_index)
                    }),
            );
        }

        lines.push(Line::from(""));
        lines.push(
            Line::from(vec![Span::styled(
                "j/k: move | a: add | e/Enter: edit | d: delete",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        );
        lines.push(
            Line::from(vec![Span::styled(
                "J/K: reorder | Esc/q: close",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        );

        lines
    }

    /// Returns add/edit render lines with a single-line input field.
    fn input_lines(&self, command_width: usize) -> Vec<Line<'static>> {
        let input_title = match self.editor.mode {
            LaunchConfigurationListEditorMode::Add => "Add command",
            LaunchConfigurationListEditorMode::Edit => "Edit command",
            LaunchConfigurationListEditorMode::Browse => "Command",
        };
        let input = self.editor.input.clone().unwrap_or_default();
        let input_text =
            truncate_with_ellipsis(format_input_with_cursor(&input).as_str(), command_width);

        vec![
            Line::from(vec![Span::styled(
                input_title,
                Style::default()
                    .fg(palette::warning())
                    .add_modifier(Modifier::BOLD),
            )])
            .alignment(Alignment::Center),
            Line::from(""),
            Line::from(Span::styled(
                format!(" {input_text:<command_width$}"),
                Style::default()
                    .fg(palette::surface_overlay())
                    .bg(palette::accent())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Enter: save | Esc: cancel | Left/Right: cursor",
                Style::default().fg(palette::text_muted()),
            )])
            .alignment(Alignment::Center),
        ]
    }
}

impl Component for LaunchConfigurationListEditor<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        let popup_area = OVERLAY_DIMENSIONS.centered_popup_area(area);
        let command_width = overlay::overlay_content_width(popup_area.width)
            .saturating_sub(1)
            .max(1);
        let lines = self.lines(command_width, popup_area.height);

        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .block(overlay::overlay_block(
                "Launch Configurations",
                palette::accent(),
            ));

        overlay::clear_popup_area(f, popup_area);
        f.render_widget(paragraph, popup_area);
    }
}

/// Returns how many command rows fit inside the editor content area.
fn visible_command_count(popup_height: u16, command_count: usize) -> usize {
    let overlay_chrome_height = overlay::overlay_required_height(0);
    let content_height = popup_height.saturating_sub(overlay_chrome_height);
    let visible_count = usize::from(content_height).saturating_sub(FOOTER_LINE_COUNT);

    visible_count.max(1).min(command_count)
}

/// Returns the first command index for a bounded editor window centered near
/// the selected command.
fn command_window_start(
    command_count: usize,
    selected_index: usize,
    visible_command_count: usize,
) -> usize {
    if command_count <= visible_command_count {
        return 0;
    }

    let centered_start = selected_index.saturating_sub(visible_command_count / 2);

    centered_start.min(command_count.saturating_sub(visible_command_count))
}

/// Builds one command row for the list editor.
fn command_line(command: &str, command_width: usize, is_selected: bool) -> Line<'static> {
    let command_label = truncate_with_ellipsis(command, command_width);

    if is_selected {
        return Line::from(Span::styled(
            format!(" {command_label:<command_width$}"),
            Style::default()
                .fg(palette::surface_overlay())
                .bg(palette::accent())
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(vec![
        Span::styled(" ", Style::default().fg(palette::text_subtle())),
        Span::styled(command_label, Style::default().fg(palette::text())),
    ])
}

/// Renders text with a `|` cursor marker at the input cursor position.
fn format_input_with_cursor(input: &InputState) -> String {
    let text = input.text();
    let mut rendered_text = String::with_capacity(text.len() + 1);
    let char_count = text.chars().count();
    let clamped_cursor_index = input.cursor.min(char_count);

    for (char_index, character) in text.chars().enumerate() {
        if char_index == clamped_cursor_index {
            rendered_text.push('|');
        }

        rendered_text.push(character);
    }

    if clamped_cursor_index == char_count {
        rendered_text.push('|');
    }

    rendered_text
}

#[cfg(test)]
#[path = "launch_configuration_list_editor_test.rs"]
mod tests;
