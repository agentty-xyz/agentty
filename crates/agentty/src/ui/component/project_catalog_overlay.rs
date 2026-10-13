//! Project access dialogs with explicit scope and opt-in result selection.

use std::path::{Path, PathBuf};

use ag_tui_text::text_util::wrap_lines;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use crate::presentation::project_catalog::ProjectCatalogDialog;
use crate::ui::component::chat_input::ChatInput;
use crate::ui::style::palette;
use crate::ui::{Component, overlay};

/// Renders one project catalog dialog over the current list page.
pub(crate) struct ProjectCatalogOverlay<'a> {
    pub(crate) dialog: &'a ProjectCatalogDialog,
}

impl ProjectCatalogOverlay<'_> {
    /// Paints the scope explanation and the explicit Discover/Skip choices.
    fn render_suggestion(f: &mut Frame, area: Rect, root: &Path, selected: usize) {
        let message = format!(
            "Discover Git repositories in your home directory?\n{}\n\nThis scans subfolders and \
             may trigger macOS folder-access prompts. Nothing is scanned if you skip. You can \
             discover in a chosen folder from Projects later.",
            root.display()
        );
        let width = area.width.min(70);
        let content_width = overlay::overlay_content_width(width);
        let lines = wrap_lines(&message, content_width);
        let labels = [" Discover ", " Skip "]
            .into_iter()
            .enumerate()
            .map(|(index, label)| {
                let style = if index == selected {
                    Style::default()
                        .fg(palette::surface_overlay())
                        .bg(palette::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette::text_muted())
                };
                Span::styled(label, style)
            })
            .collect::<Vec<_>>();
        let help = wrap_lines(
            "y: discover | n/Esc: skip | Left/Right: choose | Enter: confirm",
            content_width,
        );
        let line_count = lines
            .iter()
            .map(|line| line.width().div_ceil(content_width).max(1))
            .sum::<usize>()
            + help.len()
            + 3;
        let popup = overlay::centered_popup_area(
            area,
            0,
            0,
            width,
            overlay::overlay_required_height(line_count),
        );
        let block = overlay::overlay_block("Discover repositories?", palette::accent());
        let inner = block.inner(popup);
        let help_height = u16::try_from(help.len())
            .unwrap_or(u16::MAX)
            .min(inner.height.saturating_sub(1));
        let gap = u16::from(inner.height > help_height.saturating_add(3));
        let [message_area, _, choices_area, _, help_area] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(gap),
            Constraint::Length(1),
            Constraint::Length(gap),
            Constraint::Length(help_height),
        ])
        .areas(inner);
        overlay::clear_popup_area(f, popup);
        f.render_widget(block, popup);
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }),
            message_area,
        );
        f.render_widget(
            Paragraph::new(Line::from(labels)).alignment(Alignment::Center),
            choices_area,
        );
        f.render_widget(
            Paragraph::new(help).style(Style::default().fg(palette::text_muted())),
            help_area,
        );
    }

    /// Paints a standard dialog frame and help, returning its content areas.
    fn render_dialog_frame(f: &mut Frame, area: Rect, title: &str, help: &str) -> [Rect; 2] {
        let popup = overlay::centered_popup_area(area, 80, 65, 50, 12);
        let block = overlay::overlay_block(title, palette::accent());
        let inner = block.inner(popup);
        overlay::clear_popup_area(f, popup);
        f.render_widget(block, popup);
        let [content_area, details_area, help_area] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(inner);
        f.render_widget(
            Paragraph::new(help).style(Style::default().fg(palette::text_muted())),
            help_area,
        );

        [content_area, details_area]
    }

    /// Paints result selection and any registration failure without saving
    /// rows.
    fn render_results(
        f: &mut Frame,
        list_area: Rect,
        message_area: Rect,
        paths: &[(PathBuf, bool)],
        selected: usize,
        error: Option<&str>,
    ) {
        let rows = paths
            .iter()
            .map(|(path, checked)| {
                ListItem::new(format!(
                    "[{}] {}",
                    if *checked { "x" } else { " " },
                    path.display()
                ))
            })
            .collect::<Vec<_>>();
        let list = List::new(rows)
            .highlight_style(
                Style::default()
                    .fg(palette::accent())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");
        let mut state = ListState::default().with_selected(Some(selected));
        f.render_stateful_widget(list, list_area, &mut state);
        f.render_widget(
            Paragraph::new(error.unwrap_or(
                "Only selected repositories will be saved. Hidden/inaccessible folders may be \
                 skipped; limit: 5 levels / 200 repos.",
            ))
            .wrap(Wrap { trim: true }),
            message_area,
        );
    }
}

impl Component for ProjectCatalogOverlay<'_> {
    fn render(&self, f: &mut Frame, area: Rect) {
        match self.dialog {
            ProjectCatalogDialog::SuggestHome { root, selected } => {
                Self::render_suggestion(f, area, root, *selected);
            }
            ProjectCatalogDialog::PathInput {
                discover,
                error,
                input,
            } => {
                let title = if *discover {
                    "Discover projects in folder"
                } else {
                    "Add project"
                };
                let [message_area, input_area] =
                    Self::render_dialog_frame(f, area, title, "Enter: submit | Esc: cancel");
                let message = error.as_deref().unwrap_or(if *discover {
                    "Scan only this folder and its subfolders. This may trigger folder-access \
                     prompts. Select results before saving."
                } else {
                    "Enter a Git repository root. Relative paths start from the current project; ~ \
                     means your home directory."
                });
                f.render_widget(
                    Paragraph::new(message).wrap(Wrap { trim: true }),
                    message_area,
                );
                ChatInput::new("Path", input.text(), input.cursor).render(f, input_area);
            }
            ProjectCatalogDialog::Scanning { root } => {
                let [message_area, _] = Self::render_dialog_frame(
                    f,
                    area,
                    "Discovering repositories",
                    "Esc: cancel scan",
                );
                f.render_widget(
                    Paragraph::new(format!(
                        "Scanning {}\nUp to 5 levels and 200 repositories. Hidden and \
                         inaccessible folders may be skipped.",
                        root.display()
                    ))
                    .wrap(Wrap { trim: true }),
                    message_area,
                );
            }
            ProjectCatalogDialog::Results {
                paths,
                selected,
                error,
            } => {
                let [list_area, message_area] = Self::render_dialog_frame(
                    f,
                    area,
                    "Select repositories to add",
                    "j/k: navigate | Space: select | Enter: add selected | Esc: cancel",
                );
                Self::render_results(
                    f,
                    list_area,
                    message_area,
                    paths,
                    *selected,
                    error.as_deref(),
                );
            }
            ProjectCatalogDialog::Message { message } => {
                let [message_area, _] =
                    Self::render_dialog_frame(f, area, "Projects", "Enter/Esc: close");
                f.render_widget(
                    Paragraph::new(message.as_str()).wrap(Wrap { trim: true }),
                    message_area,
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "project_catalog_overlay_test.rs"]
mod tests;
