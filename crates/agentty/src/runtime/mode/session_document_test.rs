//! Session notes panel key tests: dismissal, scrolling, the clearing
//! confirmation, and the scroll bound.

use ag_session::{
    SessionDocument, SessionDocumentAuthor, SessionDocumentEntry, SessionDocumentEntryKind,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::{PanelAction, handle, max_scroll_offset, resolve_key};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode};
use crate::presentation::execution_graph::GraphView;
use crate::presentation::session_document::{SessionDocumentPanel, SessionDocumentRestore};
use crate::runtime::EventResult;
use crate::test_support::new_test_app_without_retained_base_dir;
use crate::ui::component::session_document_overlay::SessionDocumentLayoutCache;

/// Largest scroll offset the test panels can reach.
const MAX_SCROLL_OFFSET: u16 = 20;

/// Returns the restore target of the session view of `sess`.
fn view_restore() -> ConfirmationViewMode {
    ConfirmationViewMode {
        scroll_offset: Some(4),
        session_id: "sess".into(),
    }
}

/// Returns a document of `body` at version 1.
fn document(body: &str) -> SessionDocument {
    SessionDocument {
        body: body.into(),
        entries: Vec::new(),
        version: 1,
    }
}

/// Returns a panel over the session view of `sess` showing `document`.
fn panel(document: SessionDocument) -> SessionDocumentPanel {
    SessionDocumentPanel::new(Ok(document), SessionDocumentRestore::View(view_restore()))
}

/// Presses `code` on `panel`.
fn press(panel: &mut SessionDocumentPanel, code: KeyCode) -> PanelAction {
    resolve_key(
        panel,
        MAX_SCROLL_OFFSET,
        KeyEvent::new(code, KeyModifiers::NONE),
    )
}

/// Returns the panel's scroll offset after pressing `code` at
/// `scroll_offset`.
fn scroll_after(scroll_offset: u16, code: KeyCode) -> u16 {
    let mut panel = panel(SessionDocument::default());
    panel.scroll_offset = scroll_offset;
    press(&mut panel, code);

    panel.scroll_offset
}

#[test]
fn scroll_keys_move_by_a_line_or_a_page_within_the_bound() {
    // Arrange, Act, Assert
    assert_eq!(scroll_after(0, KeyCode::Char('j')), 1);
    assert_eq!(scroll_after(0, KeyCode::Down), 1);
    assert_eq!(scroll_after(2, KeyCode::Char('k')), 1);
    assert_eq!(scroll_after(2, KeyCode::Up), 1);
    assert_eq!(scroll_after(0, KeyCode::Up), 0);
    assert_eq!(scroll_after(0, KeyCode::PageDown), 10);
    assert_eq!(scroll_after(15, KeyCode::PageUp), 5);
    assert_eq!(scroll_after(15, KeyCode::PageDown), MAX_SCROLL_OFFSET);
    assert_eq!(scroll_after(5, KeyCode::Char('N')), 5);
}

#[test]
fn clearing_needs_the_typed_word_and_esc_backs_out() {
    // Arrange
    let mut empty = panel(SessionDocument::default());
    let mut notes = panel(document("## Note 1 from the user\n\nKeep the API."));

    // Act, Assert: an empty document offers nothing to clear.
    assert!(matches!(
        press(&mut empty, KeyCode::Char('x')),
        PanelAction::None
    ));
    assert!(empty.clear_confirmation.is_none());
    assert!(matches!(
        press(&mut notes, KeyCode::Char('x')),
        PanelAction::None
    ));
    for character in "cleaR".chars() {
        press(&mut notes, KeyCode::Char(character));
    }
    // A key the input has no command for leaves the typed word alone.
    press(&mut notes, KeyCode::F(5));
    assert_eq!(
        notes
            .clear_confirmation
            .as_ref()
            .map(|confirmation| confirmation.input.text()),
        Some("cleaR")
    );
    assert!(matches!(
        press(&mut notes, KeyCode::Enter),
        PanelAction::None
    ));
    assert_eq!(
        notes.notice.as_deref(),
        Some("Type clear to confirm clearing the session notes.")
    );
    press(&mut notes, KeyCode::Backspace);
    press(&mut notes, KeyCode::Char('r'));
    assert!(matches!(
        press(&mut notes, KeyCode::Enter),
        PanelAction::Clear(1)
    ));
    assert!(matches!(press(&mut notes, KeyCode::Esc), PanelAction::None));
    assert!(notes.clear_confirmation.is_none());
    assert!(matches!(
        press(&mut notes, KeyCode::Char('q')),
        PanelAction::Close
    ));
}

#[tokio::test]
async fn keys_close_the_panel_clear_the_document_and_ignore_other_modes() {
    // Arrange
    let mut app = new_test_app_without_retained_base_dir().await;
    let project = app
        .services
        .db()
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    app.services
        .db()
        .sessions()
        .insert_session("sess", "gpt-6.1-sol", "main", "Review", project)
        .await
        .expect("session");
    app.services
        .db()
        .session_documents()
        .append_session_notes("sess", &SessionDocumentAuthor::User, &["Kept.".into()])
        .await
        .expect("note");
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let mut inspector = GraphView::new(view_restore());
    inspector.selected_node = 2;
    app.open_session_document(SessionDocumentRestore::Graph(Box::new(inspector)))
        .await;

    // Act
    handle(&mut app, MAX_SCROLL_OFFSET, key(KeyCode::Char('x'))).await;
    for character in "clear".chars() {
        handle(&mut app, MAX_SCROLL_OFFSET, key(KeyCode::Char(character))).await;
    }
    let result = handle(&mut app, MAX_SCROLL_OFFSET, key(KeyCode::Enter)).await;
    let cleared = match &app.mode {
        AppMode::SessionDocument(panel) => panel.document.clone(),
        _ => Err("panel closed".into()),
    };
    handle(&mut app, MAX_SCROLL_OFFSET, key(KeyCode::Esc)).await;
    let restored = std::mem::replace(&mut app.mode, AppMode::List);
    handle(&mut app, MAX_SCROLL_OFFSET, key(KeyCode::Esc)).await;

    // Assert
    assert!(matches!(result, EventResult::Continue));
    let cleared = cleared.expect("cleared document");
    assert_eq!((cleared.version, cleared.body.as_str()), (2, ""));
    assert_eq!(cleared.entries[1].action_label(), "cleared the document");
    assert!(matches!(
        restored,
        AppMode::ExecutionGraph(ref view) if view.selected_node == 2
    ));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn the_scroll_bound_follows_the_open_panel() {
    // Arrange
    let mut app = new_test_app_without_retained_base_dir().await;
    let cache = SessionDocumentLayoutCache::default();
    let list_bound = max_scroll_offset(&app, &cache, Rect::new(0, 0, 100, 12));
    let entries = (1..=8)
        .map(|version| SessionDocumentEntry {
            author: SessionDocumentAuthor::User,
            created_at: 100,
            kind: SessionDocumentEntryKind::Note,
            text: format!("Finding {version}"),
            version,
        })
        .collect();
    app.mode = AppMode::SessionDocument(Box::new(panel(SessionDocument {
        body: String::new(),
        entries,
        version: 8,
    })));

    // Act
    let short_bound = max_scroll_offset(&app, &cache, Rect::new(0, 0, 100, 12));
    let tall_bound = max_scroll_offset(&app, &cache, Rect::new(0, 0, 100, 60));

    // Assert
    assert_eq!(list_bound, 0);
    assert!(short_bound > 0);
    assert_eq!(tall_bound, 0);
}
