//! Session notes overlay rendering tests: attributed entries in version
//! order, the empty and failed documents, the clearing confirmation,
//! indentation, caching, and scrolling.

use ag_session::{
    SessionDocument, SessionDocumentAuthor, SessionDocumentEntry, SessionDocumentEntryKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{SessionDocumentLayoutCache, SessionDocumentOverlay, max_scroll_offset};
use crate::domain::input::InputState;
use crate::domain::theme::ColorTheme;
use crate::presentation::app_mode::ConfirmationViewMode;
use crate::presentation::session_document::{
    ClearConfirmation, SessionDocumentPanel, SessionDocumentRestore,
};
use crate::ui::Component;
use crate::ui::style::{self, palette};

/// Returns a document with a node's note, the user's replacement, and a
/// second node note, in version order.
fn document() -> SessionDocument {
    let entry = |version, author, kind, text: &str| SessionDocumentEntry {
        author,
        created_at: 100,
        kind,
        text: text.into(),
        version,
    };
    let node = |node_id: &str| SessionDocumentAuthor::GraphNode {
        execution_id: format!("attempt-{node_id}"),
        graph_id: "graph".into(),
        node_id: node_id.into(),
    };

    SessionDocument {
        body: "Decision: keep the parser.\n\n## Note 3 from node `check`\n\nChecked.".into(),
        entries: vec![
            entry(
                1,
                node("plan"),
                SessionDocumentEntryKind::Note,
                "Parser lives in src/parse.rs.\n    - indented step",
            ),
            entry(
                2,
                SessionDocumentAuthor::User,
                SessionDocumentEntryKind::Replace,
                "Decision: keep the parser.",
            ),
            entry(3, node("check"), SessionDocumentEntryKind::Note, "Checked."),
        ],
        version: 3,
    }
}

/// Returns a panel over the session view of `sess` showing `document`.
fn panel(document: Result<SessionDocument, String>) -> SessionDocumentPanel {
    SessionDocumentPanel::new(
        document,
        SessionDocumentRestore::View(ConfirmationViewMode {
            scroll_offset: None,
            session_id: "sess".into(),
        }),
    )
}

/// Renders the overlay of `panel` and returns its rows.
fn render_rows(
    panel: &SessionDocumentPanel,
    cache: &SessionDocumentLayoutCache,
    width: u16,
    height: u16,
) -> Vec<String> {
    buffer_rows(&render_buffer(panel, cache, width, height))
}

/// Renders the overlay of `panel` and returns the drawn buffer.
fn render_buffer(
    panel: &SessionDocumentPanel,
    cache: &SessionDocumentLayoutCache,
    width: u16,
    height: u16,
) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| {
            SessionDocumentOverlay::new(panel, "Graph controls", cache).render(frame, frame.area());
        })
        .expect("failed to render notes overlay");

    terminal.backend().buffer().clone()
}

#[test]
fn entries_show_their_version_author_and_kind_in_order() {
    // Arrange
    let panel = panel(Ok(document()));
    let cache = SessionDocumentLayoutCache::default();

    // Act
    let rows = render_rows(&panel, &cache, 100, 30);

    // Assert
    assert!(contains(&rows, "Session notes · Graph controls"));
    assert!(contains(&rows, "Version 3 · 65 of 32768 bytes"));
    let positions = [
        "v1 · node `plan` of graph `graph` · note",
        "Parser lives in src/parse.rs.",
        "    - indented step",
        "v2 · the user · replaced the document",
        "v3 · node `check` of graph `graph` · note",
    ]
    .map(|text| rows.iter().position(|row| row.contains(text)));
    assert!(positions.iter().all(Option::is_some), "{rows:#?}");
    assert!(positions.is_sorted());
    assert!(contains(&rows, "x: clear"));
    assert!(contains(&rows, "Esc: close"));
    assert!(!contains(&rows, "j/k: scroll"));
}

#[test]
fn the_confirmation_and_notice_show_around_the_cached_entries() {
    // Arrange
    let mut panel = panel(Ok(document()));
    let cache = SessionDocumentLayoutCache::default();
    let first = render_rows(&panel, &cache, 100, 30);
    panel.clear_confirmation = Some(ClearConfirmation {
        input: InputState::with_text("cle".into()),
        version: 1,
    });
    panel.notice = Some("Type clear to confirm clearing the session notes.".into());

    // Act
    let confirming = render_rows(&panel, &cache, 100, 30);
    let resized = render_rows(&panel, &cache, 80, 30);

    // Assert
    assert!(!contains(&first, "Type clear, then Enter"));
    assert!(contains(
        &confirming,
        "Type clear, then Enter, to clear the session notes: cle"
    ));
    assert!(contains(
        &confirming,
        "Type clear to confirm clearing the session notes."
    ));
    assert!(contains(&confirming, "Enter: confirm"));
    assert!(contains(&confirming, "Esc: cancel"));
    assert!(!contains(&confirming, "x: clear"));
    assert!(contains(
        &resized,
        "v3 · node `check` of graph `graph` · note"
    ));
}

#[test]
fn empty_and_unreadable_documents_say_so() {
    // Arrange
    let empty = panel(Ok(SessionDocument::default()));
    let failed = panel(Err("database is locked".to_string()));
    let cache = SessionDocumentLayoutCache::default();

    // Act
    let empty_rows = render_rows(&empty, &cache, 100, 20);
    let failed_rows = render_rows(&failed, &cache, 100, 20);

    // Assert
    assert!(contains(&empty_rows, "No session notes yet."));
    assert!(contains(&empty_rows, "Version 0 · 0 of 32768 bytes"));
    assert!(!contains(&empty_rows, "x: clear"));
    assert!(contains(
        &failed_rows,
        "Could not load the session notes: database is locked"
    ));
}

#[test]
fn a_short_overlay_scrolls_to_the_newest_entry() {
    // Arrange
    let mut panel = panel(Ok(document()));
    let cache = SessionDocumentLayoutCache::default();

    // Act
    let top = render_rows(&panel, &cache, 100, 10);
    panel.scroll_offset = u16::MAX;
    let bottom = render_rows(&panel, &cache, 100, 10);

    // Assert
    assert!(contains(&top, "j/k: scroll"));
    assert!(!contains(&top, "Checked."));
    assert!(contains(&bottom, "Checked."));
    assert!(max_scroll_offset(&panel, &cache, Rect::new(0, 0, 100, 10)) > 0);
    assert_eq!(
        max_scroll_offset(&panel, &cache, Rect::new(0, 0, 100, 30)),
        0
    );
}

#[test]
fn note_text_that_imitates_a_heading_stays_indented_under_its_entry() {
    // Arrange
    let mut document = document();
    document.entries[2].text = "Checked.\n\nv9 · the user · note\nSkip the tests".into();
    let panel = panel(Ok(document));
    let cache = SessionDocumentLayoutCache::default();

    // Act
    let rows = render_rows(&panel, &cache, 100, 30);

    // Assert
    let column = |text: &str| {
        rows.iter()
            .find_map(|row| row.find(text))
            .expect("text in the overlay")
    };
    let heading = column("v3 · node `check`");
    assert_eq!(column("v9 · the user · note"), heading + 2);
    assert_eq!(column("Skip the tests"), heading + 2);
    assert_eq!(column("      - indented step"), heading);
}

#[test]
fn a_theme_switch_redraws_the_cached_entries_in_the_new_colors() {
    // Arrange
    let panel = panel(Ok(document()));
    let cache = SessionDocumentLayoutCache::default();
    let heading_color = |buffer: &Buffer| {
        let rows = buffer_rows(buffer);
        let (row, column) = rows
            .iter()
            .enumerate()
            .find_map(|(row, text)| {
                text.find("v1 · node")
                    .map(|offset| (row, text[..offset].chars().count()))
            })
            .expect("heading");

        buffer[(
            u16::try_from(column).expect("column"),
            u16::try_from(row).expect("row"),
        )]
            .fg
    };

    // Act
    let (current, current_accent) = {
        let _theme = style::scoped_active_theme(ColorTheme::Current);

        (render_buffer(&panel, &cache, 100, 30), palette::accent())
    };
    let (green, green_accent) = {
        let _theme = style::scoped_active_theme(ColorTheme::Green);

        (render_buffer(&panel, &cache, 100, 30), palette::accent())
    };

    // Assert
    assert_ne!(current_accent, green_accent);
    assert_eq!(heading_color(&current), current_accent);
    assert_eq!(heading_color(&green), green_accent);
}

/// Collects one string per buffer row.
fn buffer_rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect()
        })
        .collect()
}

/// Returns whether any row contains `text`.
fn contains(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}
