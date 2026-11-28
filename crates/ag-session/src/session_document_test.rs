//! Session document section, size, author, and kind tests.

use super::{
    SESSION_DOCUMENT_BODY_MAX_BYTES, SessionDocument, SessionDocumentAuthor, SessionDocumentEntry,
    SessionDocumentEntryKind, SessionDocumentTail,
};

/// Returns the `plan` node of graph `graph` running as `attempt-1`.
fn plan_node() -> SessionDocumentAuthor {
    SessionDocumentAuthor::GraphNode {
        execution_id: "attempt-1".into(),
        graph_id: "graph".into(),
        node_id: "plan".into(),
    }
}

#[test]
fn appended_notes_are_headed_sections_separated_by_blank_lines() {
    // Arrange
    let first = SessionDocument::note_section(1, &plan_node(), "Parser lives in src/parse.rs.");
    let second = SessionDocument::note_section(2, &SessionDocumentAuthor::User, "Keep the API.");

    // Act
    let body = SessionDocument::appended_body(&SessionDocument::appended_body("", &first), &second);

    // Assert
    assert_eq!(
        body,
        "## Note 1 from node `plan` of graph `graph`\n\nParser lives in src/parse.rs.\n\n## Note \
         2 from the user\n\nKeep the API."
    );
    assert_eq!(
        SessionDocument::appended_len(first.len(), 2, &SessionDocumentAuthor::User, 13),
        body.len()
    );
    assert_eq!(
        SessionDocument::appended_len(0, 1, &plan_node(), 29),
        first.len()
    );
}

#[test]
fn a_tail_has_room_until_one_more_note_would_pass_the_body_limit() {
    // Arrange
    let author = SessionDocumentAuthor::MainChat;
    let heading = SessionDocument::note_section(4, &author, "").len();
    let roomy = SessionDocumentTail {
        body_bytes: SESSION_DOCUMENT_BODY_MAX_BYTES - heading - 3,
        entries: Vec::new(),
        version: 3,
    };
    let full = SessionDocumentTail {
        body_bytes: roomy.body_bytes + 1,
        ..roomy.clone()
    };

    // Act, Assert
    assert!(roomy.has_room_for(&author));
    assert!(!full.has_room_for(&author));
}

#[test]
fn authors_and_kinds_round_trip_their_stored_names() {
    // Arrange
    let execution = || Some(("attempt-1".into(), "graph".into(), "plan".into()));

    // Act, Assert
    for author in [
        plan_node(),
        SessionDocumentAuthor::MainChat,
        SessionDocumentAuthor::User,
    ] {
        let stored = author
            .execution()
            .map(|(execution, graph, node)| (execution.into(), graph.into(), node.into()));
        assert_eq!(
            SessionDocumentAuthor::from_stored(author.kind_name(), stored),
            Some(author.clone())
        );
    }
    assert_eq!(SessionDocumentAuthor::MainChat.label(), "the main chat");
    assert_eq!(
        SessionDocumentAuthor::from_stored("user", execution()),
        None
    );
    assert_eq!(SessionDocumentAuthor::from_stored("graph_node", None), None);
    assert_eq!(SessionDocumentAuthor::from_stored("robot", None), None);
    for kind in [
        SessionDocumentEntryKind::Note,
        SessionDocumentEntryKind::Replace,
    ] {
        assert_eq!(
            SessionDocumentEntryKind::from_stored(kind.name()),
            Some(kind)
        );
    }
    assert_eq!(SessionDocumentEntryKind::from_stored("delete"), None);
    assert_eq!(
        serde_json::json!(plan_node()),
        serde_json::json!({
            "kind": "graph_node",
            "execution_id": "attempt-1",
            "graph_id": "graph",
            "node_id": "plan",
        })
    );
}

#[test]
fn entries_describe_notes_replacements_and_clears() {
    // Arrange
    let entry = |kind, text: &str| SessionDocumentEntry {
        author: SessionDocumentAuthor::User,
        created_at: 0,
        kind,
        text: text.into(),
        version: 1,
    };

    // Act, Assert
    assert_eq!(
        entry(SessionDocumentEntryKind::Note, "Kept.").action_label(),
        "note"
    );
    assert_eq!(
        entry(SessionDocumentEntryKind::Replace, "Compacted.").action_label(),
        "replaced the document"
    );
    assert_eq!(
        entry(SessionDocumentEntryKind::Replace, "").action_label(),
        "cleared the document"
    );
}

#[test]
fn visible_text_drops_hidden_characters_and_keeps_line_breaks() {
    // Arrange
    let text = " \u{202E}Keep\u{200B} the\tAPI\r\nnext\u{2028}line\u{E0041}\u{FFF0} \n";

    // Act
    let visible = SessionDocument::visible_text(text);

    // Assert
    assert_eq!(visible, "Keep the    API\nnext\nline");
    assert_eq!(SessionDocument::visible_text("\u{200B}\u{E0020}"), "");
}
