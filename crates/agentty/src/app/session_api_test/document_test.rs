//! Session API reads of a session's shared notes document.

use ag_session::{
    SessionDocument, SessionDocumentAuthor, SessionError, SessionId, SessionOperation,
};

use crate::app::App;
use crate::app::execution_graph::tests::seed_session;
use crate::test_support::new_test_app;

/// Reads the document of `session_id` through the session API.
async fn request_document(
    app: &mut App,
    session_id: &str,
) -> Result<SessionDocument, SessionError> {
    let service = app.session_service();
    let session_id = SessionId::from(session_id);

    app.drive_session_request(async move { service.get_session_document(&session_id).await })
        .await
}

#[tokio::test]
async fn runtime_backend_reads_the_session_document_with_its_attributed_entries() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let author = SessionDocumentAuthor::GraphNode {
        execution_id: "attempt-1".into(),
        graph_id: "graph".into(),
        node_id: "plan".into(),
    };
    app.services
        .db()
        .session_documents()
        .append_session_notes("sess", &author, &["Parser lives in src/parse.rs.".into()])
        .await
        .expect("note");

    // Act
    let document = request_document(&mut app, "sess")
        .await
        .expect("document should load");
    let unwritten = request_document(&mut app, "missing").await;

    // Assert
    assert!(
        app.session_service()
            .supports(SessionOperation::SessionDocument)
    );
    assert_eq!(document.version, 1);
    assert_eq!(document.entries[0].author, author);
    assert_eq!(
        document.body,
        "## Note 1 from node `plan` of graph `graph`\n\nParser lives in src/parse.rs."
    );
    // The terminal and in-process callers reach every session; a paired
    // device's scope check is covered with the other session reads.
    assert_eq!(unwritten, Ok(SessionDocument::default()));
}

#[tokio::test]
async fn runtime_backend_reports_an_unreadable_session_document() {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    sqlx::query("DROP TABLE session_document_entry")
        .execute(&pool)
        .await
        .expect("document reads should fail");

    // Act
    let document = request_document(&mut app, "sess").await;

    // Assert
    assert!(
        matches!(document, Err(SessionError::Operation(_))),
        "{document:?}"
    );
}
