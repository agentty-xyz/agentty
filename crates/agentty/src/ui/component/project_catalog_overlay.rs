//! Project access dialogs with explicit scope and opt-in result selection.

use std::path::{Path, PathBuf};

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
    fn render_suggestion(
        f: &mut Frame,
        message_area: Rect,
        choices_area: Rect,
        root: &Path,
        selected: usize,
    ) {
        let message = format!(
            "Discover Git repositories in your home directory?\n{}\n\nThis scans subfolders and \
             may trigger macOS folder-access prompts. Nothing is scanned if you skip. You can \
             discover in a chosen folder from Projects later.",
            root.display()
        );
        f.render_widget(
            Paragraph::new(message).wrap(Wrap { trim: true }),
            message_area,
        );
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
        f.render_widget(
            Paragraph::new(Line::from(labels)).alignment(Alignment::Center),
            choices_area,
        );
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
        let popup = overlay::centered_popup_area(area, 80, 65, 50, 12);
        let title = match self.dialog {
            ProjectCatalogDialog::SuggestHome { .. } => "Discover repositories?",
            ProjectCatalogDialog::PathInput {
                discover: false, ..
            } => "Add project",
            ProjectCatalogDialog::PathInput { discover: true, .. } => "Discover projects in folder",
            ProjectCatalogDialog::Scanning { .. } => "Discovering repositories",
            ProjectCatalogDialog::Results { .. } => "Select repositories to add",
            ProjectCatalogDialog::Message { .. } => "Projects",
        };
        let block = overlay::overlay_block(title, palette::accent());
        let inner = block.inner(popup);
        overlay::clear_popup_area(f, popup);
        f.render_widget(block, popup);
        let sections = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);
        let muted = Style::default().fg(palette::text_muted());
        let mut help = "Enter/Esc: close";
        match self.dialog {
            ProjectCatalogDialog::SuggestHome { root, selected } => {
                Self::render_suggestion(f, sections[0], sections[1], root, *selected);
                help = "y: discover | n/Esc: skip | Left/Right: choose | Enter: confirm";
            }
            ProjectCatalogDialog::PathInput {
                discover,
                error,
                input,
            } => {
                let message = error.as_deref().unwrap_or(if *discover {
                    "Scan only this folder and its subfolders. This may trigger folder-access \
                     prompts. Select results before saving."
                } else {
                    "Enter a Git repository root. Relative paths start from the current project; ~ \
                     means your home directory."
                });
                f.render_widget(
                    Paragraph::new(message).wrap(Wrap { trim: true }),
                    sections[0],
                );
                ChatInput::new("Path", input.text(), input.cursor).render(f, sections[1]);
                help = "Enter: submit | Esc: cancel";
            }
            ProjectCatalogDialog::Scanning { root } => {
                f.render_widget(
                    Paragraph::new(format!(
                        "Scanning {}\nUp to 5 levels and 200 repositories. Hidden and \
                         inaccessible folders may be skipped.",
                        root.display()
                    ))
                    .wrap(Wrap { trim: true }),
                    sections[0],
                );
                help = "Esc: cancel scan";
            }
            ProjectCatalogDialog::Results {
                paths,
                selected,
                error,
            } => {
                Self::render_results(
                    f,
                    sections[0],
                    sections[1],
                    paths,
                    *selected,
                    error.as_deref(),
                );
                help = "j/k: navigate | Space: select | Enter: add selected | Esc: cancel";
            }
            ProjectCatalogDialog::Message { message } => {
                f.render_widget(
                    Paragraph::new(message.as_str()).wrap(Wrap { trim: true }),
                    sections[0],
                );
            }
        }
        f.render_widget(Paragraph::new(help).style(muted), sections[2]);
    }
}

#[cfg(test)]
#[path = "project_catalog_overlay_test.rs"]
mod tests;
