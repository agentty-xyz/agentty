use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use super::ProjectCatalogOverlay;
use crate::domain::input::InputState;
use crate::presentation::project_catalog::ProjectCatalogDialog;
use crate::ui::Component;
use crate::ui::style::palette;

#[test]
fn project_dialogs_render_scope_choices_selection_errors_and_controls() {
    // Arrange
    let cases = project_dialog_cases();
    for (dialog, expected) in cases {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        // Act
        terminal
            .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
            .expect("draw");
        // Assert
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        for expected_text in expected {
            assert!(
                text.contains(expected_text),
                "missing {expected_text}: {text}"
            );
        }
    }
}

#[test]
fn startup_suggestion_visually_selects_no() {
    // Arrange
    let dialog = ProjectCatalogDialog::SuggestHome {
        root: PathBuf::from("/home/test"),
        selected: 1,
    };
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    // Act
    terminal
        .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
        .expect("draw");
    // Assert
    let cells = terminal.backend().buffer().content();
    let no = cells
        .windows(4)
        .find(|span| {
            span.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                == " No "
        })
        .expect("No choice");
    assert!(no.iter().all(|cell| cell.modifier.contains(Modifier::BOLD)));
}

#[test]
fn startup_suggestion_fits_content_and_stays_centered() {
    // Arrange
    for (width, height) in [(80, 24), (120, 40), (200, 60)] {
        let dialog = ProjectCatalogDialog::SuggestHome {
            root: PathBuf::from("/home/test"),
            selected: 1,
        };
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");

        // Act
        terminal
            .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
            .expect("draw");

        // Assert
        let buffer = terminal.backend().buffer();
        let left = (width - 70) / 2;
        let top = (height - 13) / 2;
        assert_eq!(buffer[(left, top)].symbol(), "╭");
        assert_eq!(buffer[(left + 69, top + 12)].symbol(), "╯");
        let text = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        for expected in ["/home/test", "Projects later.", "Yes", "No"] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("Enter: confirm"));
        assert!(!text.contains("n/Esc: skip"));
        for row in [top + 1, top + 2] {
            assert!((left + 1..left + 69).all(|column| buffer[(column, row)].symbol() == " "));
        }
        assert_eq!(buffer[(left + 3, top + 3)].symbol(), "D");
        for column in left + 3..left + 13 {
            let cell = &buffer[(column, top + 4)];
            assert_eq!(cell.fg, palette::accent());
            assert!(cell.modifier.contains(Modifier::BOLD));
        }
    }
}

#[test]
fn startup_suggestion_wraps_and_highlights_long_paths_in_narrow_terminals() {
    // Arrange
    let root = format!("/home/{}/last-folder", "界".repeat(60));
    let dialog = ProjectCatalogDialog::SuggestHome {
        root: PathBuf::from(&root),
        selected: 0,
    };
    let mut terminal = Terminal::new(TestBackend::new(50, 40)).expect("terminal");

    // Act
    terminal
        .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
        .expect("draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let text = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    let content = (0..40)
        .flat_map(|row| (3..47).map(move |column| buffer[(column, row)].symbol()))
        .collect::<String>();
    let unwrapped = content
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(unwrapped.contains(&root), "complete path survives wrapping");
    for expected in ["Projects later.", "Yes", "No"] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert_eq!(text.matches('界').count(), 60);
    let bottom = buffer
        .content()
        .iter()
        .position(|cell| cell.symbol() == "╰")
        .expect("bottom border")
        / 50;
    for cell in buffer.content().iter().filter(|cell| cell.symbol() == "界") {
        assert_eq!(cell.fg, palette::accent());
        assert!(cell.modifier.contains(Modifier::BOLD));
    }
    let choices = buffer
        .content()
        .windows(3)
        .position(|cells| {
            cells
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                == "Yes"
        })
        .expect("Yes choice")
        / 50;
    assert_eq!(bottom, choices + 2, "only padding follows the choices");
}

#[test]
fn startup_suggestion_clamps_to_tiny_terminals() {
    // Arrange
    let dialog = ProjectCatalogDialog::SuggestHome {
        root: PathBuf::from("/home/test"),
        selected: 1,
    };
    for (width, height) in [(0, 0), (1, 1), (20, 6)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");

        // Act
        terminal
            .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
            .expect("draw");

        // Assert
        assert_eq!(terminal.backend().buffer().area.width, width);
        assert_eq!(terminal.backend().buffer().area.height, height);
    }
}

#[test]
fn startup_suggestion_preserves_controls_in_short_and_resized_viewports() {
    // Arrange
    for selected in [0, 1] {
        let dialog = ProjectCatalogDialog::SuggestHome {
            root: PathBuf::from("/home/test"),
            selected,
        };
        let selected_label = ["Yes", "No"][selected];
        let mut terminal = Terminal::new(TestBackend::new(80, 11)).expect("terminal");
        for height in [11, 24, 11, 8, 6, 24] {
            // Act
            terminal.backend_mut().resize(80, height);
            terminal
                .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
                .expect("draw");

            // Assert
            let cells = terminal.backend().buffer().content();
            let text = cells
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            for expected in ["Yes", "No"] {
                assert!(
                    text.contains(expected),
                    "missing {expected} at height {height}: {text}"
                );
            }
            assert!(
                cells.windows(selected_label.len()).any(|span| {
                    span.iter()
                        .map(ratatui::buffer::Cell::symbol)
                        .collect::<String>()
                        == selected_label
                        && span.iter().all(|cell| {
                            cell.bg == palette::accent() && cell.modifier.contains(Modifier::BOLD)
                        })
                }),
                "selected action must remain visible at height {height}"
            );
        }
    }
}

/// Representative content and controls for every project dialog variant.
fn project_dialog_cases() -> [(ProjectCatalogDialog, [&'static str; 3]); 9] {
    [
        (
            ProjectCatalogDialog::SuggestHome {
                root: PathBuf::from("/home/test"),
                selected: 1,
            },
            ["Discover repositories?", " No ", "/home/test"],
        ),
        (
            ProjectCatalogDialog::SuggestHome {
                root: PathBuf::from("/home/test"),
                selected: 0,
            },
            ["Discover repositories?", " Yes ", "/home/test"],
        ),
        (
            ProjectCatalogDialog::PathInput {
                discover: false,
                error: None,
                input: InputState::with_text("~/repo".to_string()),
            },
            ["Add project", "~/repo", "Enter: submit | Esc: cancel"],
        ),
        (
            ProjectCatalogDialog::PathInput {
                discover: true,
                error: None,
                input: InputState::default(),
            },
            [
                "Discover projects in folder",
                "folder-access",
                "Enter: submit | Esc: cancel",
            ],
        ),
        (
            ProjectCatalogDialog::PathInput {
                discover: true,
                error: Some("Choose a directory".to_string()),
                input: InputState::default(),
            },
            [
                "Discover projects in folder",
                "Choose a directory",
                "Enter: submit | Esc: cancel",
            ],
        ),
        (
            ProjectCatalogDialog::Scanning {
                root: PathBuf::from("/chosen/code"),
            },
            [
                "Discovering repositories",
                "Scanning /chosen/code",
                "Esc: cancel scan",
            ],
        ),
        (
            ProjectCatalogDialog::Results {
                error: None,
                paths: vec![
                    (PathBuf::from("/first"), false),
                    (PathBuf::from("/second"), true),
                ],
                selected: 1,
            },
            [
                "Select repositories to add",
                "[x] /second",
                "Enter: add selected | Esc: cancel",
            ],
        ),
        (
            ProjectCatalogDialog::Results {
                error: Some("Cannot save".to_string()),
                paths: Vec::new(),
                selected: 0,
            },
            [
                "Select repositories to add",
                "Cannot save",
                "Enter: add selected | Esc: cancel",
            ],
        ),
        (
            ProjectCatalogDialog::Message {
                message: "No repositories found".to_string(),
            },
            ["Projects", "No repositories found", "Enter/Esc: close"],
        ),
    ]
}
