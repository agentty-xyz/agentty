//! Session notes panel tests: the loaded document, a failed read, and the
//! panel drawn above the session view or the graph inspector.

use ag_session::{SessionDocumentAuthor, SessionId};

use crate::app::App;
use crate::app::execution_graph::tests::seed_session;
use crate::domain::input::InputState;
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode};
use crate::presentation::execution_graph::GraphView;
use crate::presentation::session_document::{ClearConfirmation, SessionDocumentRestore};
use crate::ui::style::ColorSupport;

#[tokio::test]
async fn the_notes_panel_draws_above_the_page_that_opened_it() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    app.sessions.state_mut().replace_sessions(vec![
        crate::test_support::SessionFixtureBuilder::new()
            .id("sess")
            .title(Some("Graph controls".into()))
            .build(),
    ]);
    let view = ConfirmationViewMode {
        scroll_offset: None,
        session_id: SessionId::from("sess"),
    };

    // Act
    app.open_session_document(SessionDocumentRestore::View(view.clone()))
        .await;
    let over_view = render(&app);
    app.open_session_document(SessionDocumentRestore::Graph(Box::new(GraphView::new(
        view,
    ))))
    .await;
    let over_graph = render(&app);

    // Assert
    for text in [&over_view, &over_graph] {
        assert!(text.contains("Session notes · Graph controls"));
        assert!(text.contains("No session notes yet."));
    }
    assert!(over_graph.contains("No saved execution graphs"));
    assert!(!over_view.contains("No saved execution graphs"));
}

/// Draws `app` through the production renderer and returns its text.
fn render(app: &App) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).expect("terminal");
    let cache = crate::ui::RenderCacheStore::default();
    let mut project_table = ratatui::widgets::TableState::default();
    let mut session_table = ratatui::widgets::TableState::default();
    terminal
        .draw(|frame| {
            crate::ui::render_app(
                &app.view_snapshot(),
                frame,
                ColorSupport::Enabled,
                &mut project_table,
                &cache,
                &mut session_table,
            );
        })
        .expect("render");

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[tokio::test]
async fn closing_the_notes_panel_leaves_any_other_mode_alone() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    app.mode = AppMode::ExecutionGraph(Box::new(GraphView::new(ConfirmationViewMode {
        scroll_offset: None,
        session_id: SessionId::from("sess"),
    })));

    // Act
    app.close_session_document();

    // Assert
    assert!(matches!(app.mode, AppMode::ExecutionGraph(_)));
}

#[tokio::test]
async fn the_notes_panel_opens_with_the_document_or_the_reason_it_failed() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let restore = || {
        SessionDocumentRestore::View(ConfirmationViewMode {
            scroll_offset: Some(3),
            session_id: SessionId::from("sess"),
        })
    };

    // Act
    app.open_session_document(restore()).await;
    let loaded = std::mem::replace(&mut app.mode, AppMode::List);
    sqlx::query("DROP TABLE session_document_entry")
        .execute(&pool)
        .await
        .expect("document reads should fail");
    app.open_session_document(restore()).await;
    app.clear_session_document(0).await;

    // Assert
    let AppMode::SessionDocument(loaded) = loaded else {
        unreachable!("the notes panel opens");
    };
    assert_eq!(
        loaded.document.as_ref().map(|document| document.version),
        Ok(0)
    );
    assert_eq!(loaded.scroll_offset, 0);
    assert!(matches!(
        loaded.restore.into_mode(),
        AppMode::View {
            scroll_offset: Some(3),
            ..
        }
    ));
    let AppMode::SessionDocument(failed) = &app.mode else {
        unreachable!("the notes panel opens after a failed read");
    };
    assert!(
        failed
            .document
            .as_ref()
            .is_err_and(|error| error.contains("session_document_entry"))
    );
    assert!(
        failed
            .notice
            .as_deref()
            .is_some_and(|notice| notice.starts_with("Could not clear the session notes:"))
    );
}

#[tokio::test]
async fn the_open_panel_follows_new_notes_and_refuses_a_stale_clear() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let documents = app.services.db().session_documents();
    let note = |text: &str| vec![text.to_string()];
    let user = SessionDocumentAuthor::User;
    documents
        .append_session_notes("sess", &user, &note("First."))
        .await
        .expect("first note");
    let sess = std::collections::HashSet::from([SessionId::from("sess")]);
    let other = std::collections::HashSet::from([SessionId::from("other")]);
    app.refresh_notes_panel(&sess).await;
    app.clear_session_document(1).await;
    app.open_session_document(SessionDocumentRestore::View(ConfirmationViewMode {
        scroll_offset: None,
        session_id: SessionId::from("sess"),
    }))
    .await;
    app.services
        .db()
        .session_documents()
        .append_session_notes("sess", &user, &note("Second."))
        .await
        .expect("second note");

    // Act
    app.refresh_notes_panel(&other).await;
    let unrelated = panel_version(&app);
    app.services
        .db()
        .session_documents()
        .append_session_notes("sess", &user, &note("Third."))
        .await
        .expect("third note");
    app.clear_session_document(1).await;
    let stale = panel_version(&app);
    app.refresh_notes_panel(&sess).await;

    // Assert
    assert_eq!(unrelated, Some(1));
    assert_eq!(stale, Some(3));
    let AppMode::SessionDocument(panel) = &app.mode else {
        unreachable!("the notes panel stays open");
    };
    assert_eq!(
        panel.notice.as_deref(),
        Some("The session notes changed meanwhile; review them and clear again.")
    );
    assert_eq!(
        panel.document.as_ref().map(|document| document.version),
        Ok(3)
    );
}

#[tokio::test]
async fn a_note_arriving_while_clearing_is_confirmed_closes_the_confirmation() {
    // Arrange
    let (mut app, _directory) = crate::test_support::new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let user = SessionDocumentAuthor::User;
    let sess = std::collections::HashSet::from([SessionId::from("sess")]);
    app.services
        .db()
        .session_documents()
        .append_session_notes("sess", &user, &["First.".into()])
        .await
        .expect("first note");
    app.open_session_document(SessionDocumentRestore::View(ConfirmationViewMode {
        scroll_offset: None,
        session_id: SessionId::from("sess"),
    }))
    .await;
    let confirm = |app: &mut App| {
        let AppMode::SessionDocument(panel) = &mut app.mode else {
            unreachable!("the notes panel is open");
        };
        panel.clear_confirmation = panel.clearable_version().map(|version| ClearConfirmation {
            input: InputState::with_text("clear".into()),
            version,
        });
    };
    confirm(&mut app);
    app.refresh_notes_panel(&sess).await;
    let unchanged_is_open = matches!(
        &app.mode,
        AppMode::SessionDocument(panel) if panel.clear_confirmation.is_some()
    );

    // Act: a node publishes while the user is typing the word.
    app.services
        .db()
        .session_documents()
        .append_session_notes("sess", &user, &["Second.".into()])
        .await
        .expect("second note");
    app.refresh_notes_panel(&sess).await;
    let AppMode::SessionDocument(panel) = &app.mode else {
        unreachable!("the notes panel stays open");
    };
    let closed = panel.clear_confirmation.is_none();
    let notice = panel.notice.clone();
    // A clear the user had confirmed on the first version still refuses.
    app.clear_session_document(1).await;

    // Assert
    assert!(unchanged_is_open);
    assert!(closed);
    assert_eq!(
        notice.as_deref(),
        Some("The session notes changed meanwhile; review them and clear again.")
    );
    let document = app
        .services
        .db()
        .session_documents()
        .load_session_document("sess")
        .await
        .expect("document");
    assert_eq!((document.version, document.entries.len()), (2, 2));
    confirm(&mut app);
    let AppMode::SessionDocument(panel) = &app.mode else {
        unreachable!("the notes panel stays open");
    };
    assert_eq!(
        panel
            .clear_confirmation
            .as_ref()
            .map(|confirmation| confirmation.version),
        Some(2)
    );
}

/// Returns the version of the document the open notes panel shows.
fn panel_version(app: &App) -> Option<u64> {
    let AppMode::SessionDocument(panel) = &app.mode else {
        return None;
    };

    panel
        .document
        .as_ref()
        .ok()
        .map(|document| document.version)
}
