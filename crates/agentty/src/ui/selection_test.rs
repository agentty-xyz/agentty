//! Rendering regressions for selection visibility across pages and controls.

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::TableState;

use crate::app::Tab;
use crate::domain::agent::ReasoningLevel;
use crate::domain::harness::HarnessAvailability;
use crate::domain::project::{Project, ProjectListItem};
use crate::domain::session::Status;
use crate::domain::theme::ColorTheme;
use crate::presentation::setting::{
    LaunchConfigurationListEditorMode, LaunchConfigurationListEditorSnapshot,
};
use crate::test_support::titled_session_fixture;
use crate::ui::component::chat_input::{ChatInput, SuggestionItem, SuggestionList};
use crate::ui::component::confirmation_overlay::ConfirmationOverlay;
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::component::info_overlay::InfoOverlay;
use crate::ui::component::launch_configuration_list_editor::LaunchConfigurationListEditor;
use crate::ui::component::launch_configuration_overlay::LaunchConfigurationOverlay;
use crate::ui::component::project_switcher_overlay::ProjectSwitcherOverlay;
use crate::ui::component::session_creation_overlay::SessionCreationOverlay;
use crate::ui::component::stack_append_parent_overlay::StackAppendParentOverlay;
use crate::ui::component::tab::Tabs;
use crate::ui::page::project_list::ProjectListPage;
use crate::ui::page::session_list::SessionListPage;
use crate::ui::{Component, Page, style};

#[test]
fn option_highlights_follow_selection_across_themes_and_widths() {
    for theme in ColorTheme::ALL {
        for width in [60, 80, 100] {
            for selected_index in [0, 1] {
                // Arrange
                let _theme_scope = style::scoped_active_theme(theme);
                let projects = [project(1, "Alpha"), project(2, "Beta")];
                let project_refs = projects.iter().collect::<Vec<_>>();
                let sessions = [
                    titled_session_fixture("Alpha", Status::Review),
                    titled_session_fixture("Beta", Status::Review),
                ];
                let session_refs = sessions.iter().collect::<Vec<_>>();
                let commands = vec!["Alpha".to_string(), "Beta".to_string()];
                let editor = LaunchConfigurationListEditorSnapshot {
                    commands: commands.clone(),
                    input: None,
                    mode: LaunchConfigurationListEditorMode::Browse,
                    selected_index,
                };
                let project_switcher =
                    ProjectSwitcherOverlay::new(&project_refs, 1, selected_index);
                let parent_picker = StackAppendParentOverlay::new(&session_refs, selected_index);
                let launch_picker = LaunchConfigurationOverlay::new(&commands)
                    .selected_command_index(selected_index);
                let launch_editor = LaunchConfigurationListEditor::new(&editor);
                let confirmation = ConfirmationOverlay::new("Choose", "Select an option")
                    .option_labels("Alpha", "Beta")
                    .selected_first(selected_index == 0);
                let creation = SessionCreationOverlay::new(
                    selected_index,
                    false,
                    false,
                    HarnessAvailability::Hidden,
                    true,
                );
                let info = InfoOverlay::new("Notice", "Ready");
                let suggestions = SuggestionList {
                    items: commands
                        .iter()
                        .map(|label| SuggestionItem {
                            badge: None,
                            detail: None,
                            label: label.clone(),
                            metadata: None,
                        })
                        .collect(),
                    selected_index,
                    title: "Suggestions".to_string(),
                };
                let chat_input = ChatInput::new("Prompt", "", 0).suggestion_list(&suggestions);
                let selected_label = ["Alpha", "Beta"][selected_index];
                let controls: [(&dyn Component, &str); 8] = [
                    (&project_switcher, selected_label),
                    (&parent_picker, selected_label),
                    (&launch_picker, selected_label),
                    (&launch_editor, selected_label),
                    (&confirmation, selected_label),
                    (&creation, ["Regular", "Draft"][selected_index]),
                    (&info, "OK"),
                    (&chat_input, selected_label),
                ];
                for (control, label) in controls {
                    let mut terminal = ratatui::Terminal::new(TestBackend::new(width, 30))
                        .expect("create terminal");

                    // Act
                    terminal
                        .draw(|frame| control.render(frame, frame.area()))
                        .expect("draw control");

                    // Assert
                    assert_selection(terminal.backend().buffer(), label);
                }
            }
        }
    }
}

#[test]
fn project_and_session_highlights_move_without_shifting_columns() {
    for theme in ColorTheme::ALL {
        for width in [80, 100, 120] {
            // Arrange
            let _theme_scope = style::scoped_active_theme(theme);
            let projects = [project(1, "Alpha"), project(2, "Beta")];
            let sessions = [
                titled_session_fixture("Alpha", Status::Review),
                titled_session_fixture("Beta", Status::Review),
            ];
            let mut project_positions = Vec::new();
            let mut session_positions = Vec::new();
            for selected in [None, Some(0), Some(1)] {
                let mut state = TableState::default().with_selected(selected);
                let mut terminal =
                    ratatui::Terminal::new(TestBackend::new(width, 30)).expect("create terminal");

                // Act
                terminal
                    .draw(|frame| {
                        ProjectListPage::new(&projects, &[], &[], &mut state, 1, 0)
                            .render(frame, frame.area());
                    })
                    .expect("draw projects");

                // Assert
                let buffer = terminal.backend().buffer();
                project_positions.push(text_position(buffer, "Alpha").0);
                if let Some(index) = selected {
                    assert_selection(buffer, ["Alpha", "Beta"][index]);
                } else {
                    assert_eq!(selector_count(buffer), 0);
                }

                // Arrange
                let mut state = TableState::default().with_selected(selected);

                // Act
                terminal
                    .draw(|frame| {
                        SessionListPage::new(&sessions, &mut state, ReasoningLevel::default(), 0)
                            .render(frame, frame.area());
                    })
                    .expect("draw sessions");

                // Assert
                let buffer = terminal.backend().buffer();
                session_positions.push(text_position(buffer, "Alpha").0);
                if let Some(index) = selected {
                    let (column, row) = text_position(buffer, ["Alpha", "Beta"][index]);
                    let cell = &buffer[(column, row)];
                    assert_eq!(cell.fg, style::palette::text());
                    assert_eq!(cell.bg, style::palette::surface_selection());
                    assert!(cell.modifier.contains(Modifier::BOLD));
                    assert_eq!(selector_count(buffer), 0);
                } else {
                    assert_eq!(selector_count(buffer), 0);
                }
                assert_eq!(state.selected(), selected);
            }
            assert!(
                project_positions
                    .iter()
                    .all(|position| *position == project_positions[0])
            );
            assert!(
                session_positions
                    .iter()
                    .all(|position| *position == session_positions[0])
            );
        }
    }
}

#[test]
fn file_explorer_highlight_only_appears_when_focused_and_preserves_suffixes() {
    for theme in ColorTheme::ALL {
        for is_focused in [false, true] {
            // Arrange
            let _theme_scope = style::scoped_active_theme(theme);
            let mut terminal =
                ratatui::Terminal::new(TestBackend::new(24, 10)).expect("create terminal");
            let explorer = FileExplorer::from_cached_lines(
                Arc::from([Line::from(vec![
                    Span::raw("long-file-name.rs"),
                    Span::raw(" +2 -1"),
                ])]),
                1,
            )
            .focused(is_focused);

            // Act
            terminal
                .draw(|frame| explorer.render(frame, frame.area()))
                .expect("draw explorer");

            // Assert
            let buffer = terminal.backend().buffer();
            assert_eq!(selector_count(buffer), 0);
            text_position(buffer, "+2 -1");
            if is_focused {
                assert_selection(buffer, "long-file");
            } else {
                let position = text_position(buffer, "long-file");
                assert_ne!(buffer[position].bg, style::palette::surface_selection());
            }
        }
    }
}

#[test]
fn active_tabs_have_readable_colors_and_bold_text_without_an_underline() {
    for theme in ColorTheme::ALL {
        for tab in [Tab::Projects, Tab::Sessions, Tab::Settings] {
            // Arrange
            let _theme_scope = style::scoped_active_theme(theme);
            let mut terminal =
                ratatui::Terminal::new(TestBackend::new(100, 5)).expect("create terminal");
            let tabs = Tabs::new(tab, 0, &[]);

            // Act
            terminal
                .draw(|frame| tabs.render(frame, frame.area()))
                .expect("draw tabs");

            // Assert
            let position = text_position(terminal.backend().buffer(), tab.title());
            let label_width = u16::try_from(tab.title().len()).expect("short tab title");
            for column in position.0 - 1..=position.0 + label_width {
                let cell = &terminal.backend().buffer()[(column, position.1)];
                assert_eq!(cell.fg, style::palette::text_selection());
                assert_eq!(cell.bg, style::palette::surface_selection());
                assert!(cell.modifier.contains(Modifier::BOLD));
                assert!(!cell.modifier.contains(Modifier::UNDERLINED));
            }
        }
    }
}

fn project(id: i64, label: &str) -> ProjectListItem {
    ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
        project: Project {
            created_at: 0,
            display_name: Some(label.to_string()),
            git_branch: Some("main".to_string()),
            id,
            is_favorite: false,
            last_opened_at: None,
            path: PathBuf::from(label),
            updated_at: 0,
        },
        session_count: 0,
    }
}

fn assert_selection(buffer: &Buffer, label: &str) {
    assert_eq!(selector_count(buffer), 0, "no selector glyph for {label}");
    let (label_x, row_y) = text_position(buffer, label);
    let cell = &buffer[(label_x, row_y)];
    assert_eq!(
        cell.fg,
        style::palette::text_selection(),
        "foreground for {label}"
    );
    assert_eq!(
        cell.bg,
        style::palette::surface_selection(),
        "background for {label}"
    );
    assert!(cell.modifier.contains(Modifier::BOLD));
}

fn selector_count(buffer: &Buffer) -> usize {
    buffer
        .content()
        .iter()
        .filter(|cell| cell.symbol() == ">")
        .count()
}

fn text_position(buffer: &Buffer, text: &str) -> (u16, u16) {
    (0..buffer.area.height)
        .find_map(|row_y| {
            let row: String = (0..buffer.area.width)
                .map(|column| buffer[(column, row_y)].symbol())
                .collect();
            row.find(text).map(|byte_offset| {
                (
                    u16::try_from(row[..byte_offset].chars().count()).expect("column fits"),
                    row_y,
                )
            })
        })
        .expect("expected label in rendered buffer")
}
