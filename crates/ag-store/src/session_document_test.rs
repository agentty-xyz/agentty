//! Session document repository tests: attribution, redelivery, bounds, and
//! refused writes.

use std::sync::Arc;

use ag_session::{
    SESSION_DOCUMENT_BODY_MAX_BYTES, SESSION_DOCUMENT_NOTE_MAX_BYTES, SessionDocument,
    SessionDocumentAppend, SessionDocumentAuthor, SessionDocumentEntry, SessionDocumentEntryKind,
    SessionDocumentReplace,
};

use crate::{Database, DbError};

#[tokio::test]
async fn notes_keep_their_author_and_a_redelivered_response_appends_once() {
    // Arrange
    let database = database_with_session().await;
    let documents = database.session_documents();
    let node = node_author("attempt-1");
    let notes = vec![
        "Parser lives in src/parse.rs.".to_string(),
        "Keep the API.".into(),
    ];

    // Act
    let first = documents
        .append_session_notes("session", &node, &notes)
        .await
        .expect("first delivery");
    let redelivered = documents
        .append_session_notes("session", &node, &notes)
        .await
        .expect("redelivery");
    // A redelivery with other notes, and more of them, adds nothing either.
    let changed = documents
        .append_session_notes(
            "session",
            &node,
            &["Changed.".into(), "Keep the API.".into(), "Extra.".into()],
        )
        .await
        .expect("changed redelivery");
    let from_user = documents
        .append_session_notes("session", &SessionDocumentAuthor::User, &notes[1..])
        .await
        .expect("user note");
    let document = documents
        .load_session_document("session")
        .await
        .expect("document");

    // Assert
    assert_eq!(
        first,
        [
            SessionDocumentAppend::Appended { version: 1 },
            SessionDocumentAppend::Appended { version: 2 },
        ]
    );
    assert_eq!(
        redelivered,
        [
            SessionDocumentAppend::AlreadyAppended { version: 1 },
            SessionDocumentAppend::AlreadyAppended { version: 2 },
        ]
    );
    assert_eq!(
        changed,
        [
            SessionDocumentAppend::AlreadyAppended { version: 1 },
            SessionDocumentAppend::AlreadyAppended { version: 2 },
            SessionDocumentAppend::AlreadyAppended { version: 2 },
        ]
    );
    assert_eq!(from_user, [SessionDocumentAppend::Appended { version: 3 }]);
    assert_eq!(document.version, 3);
    assert_eq!(
        document.entries,
        [
            note_entry(&node, "Parser lives in src/parse.rs.", 1),
            note_entry(&node, "Keep the API.", 2),
            note_entry(&SessionDocumentAuthor::User, "Keep the API.", 3),
        ]
    );
    assert!(
        document
            .body
            .ends_with("## Note 3 from the user\n\nKeep the API.")
    );
}

#[tokio::test]
async fn refused_notes_are_not_stored_and_later_notes_still_land() {
    // Arrange
    let database = database_with_session().await;
    let documents = database.session_documents();
    let note_limit = "n".repeat(SESSION_DOCUMENT_NOTE_MAX_BYTES);
    let notes = vec![
        format!("{note_limit}!"),
        note_limit.clone(),
        "Short hand-off.".into(),
    ];

    // Act
    let outcomes = documents
        .append_session_notes("session", &node_author("attempt-1"), &notes)
        .await
        .expect("append");
    let document = documents
        .load_session_document("session")
        .await
        .expect("document");

    // Assert
    assert_eq!(
        outcomes,
        [
            SessionDocumentAppend::NoteTooLong {
                bytes: SESSION_DOCUMENT_NOTE_MAX_BYTES + 1
            },
            SessionDocumentAppend::Appended { version: 1 },
            SessionDocumentAppend::Appended { version: 2 },
        ]
    );
    assert_eq!(
        document
            .entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        [note_limit.as_str(), "Short hand-off."]
    );
}

#[tokio::test]
async fn a_full_body_refuses_notes_until_a_replace_makes_room() {
    // Arrange
    let database = database_with_session().await;
    let documents = database.session_documents();
    let near_full = "f".repeat(SESSION_DOCUMENT_BODY_MAX_BYTES - 10);
    documents
        .replace_session_document("session", &SessionDocumentAuthor::User, 0, &near_full)
        .await
        .expect("filled body");
    let note = vec!["Too much for the body.".to_string()];
    let author = node_author("attempt-1");

    // Act
    let refused = documents
        .append_session_notes("session", &author, &note)
        .await
        .expect("refused append");
    let compacted = documents
        .replace_session_document("session", &SessionDocumentAuthor::User, 1, "Compacted.")
        .await
        .expect("compaction");
    let retried = documents
        .append_session_notes("session", &author, &note)
        .await
        .expect("retried append");
    let document = documents
        .load_session_document("session")
        .await
        .expect("document");

    // Assert
    let full_length = SessionDocument::appended_body(
        &near_full,
        &SessionDocument::note_section(2, &author, &note[0]),
    )
    .len();
    assert_eq!(
        refused,
        [SessionDocumentAppend::DocumentFull { bytes: full_length }]
    );
    assert_eq!(compacted, SessionDocumentReplace::Replaced { version: 2 });
    assert_eq!(retried, [SessionDocumentAppend::Appended { version: 3 }]);
    assert_eq!(
        document.body,
        format!(
            "Compacted.\n\n{}",
            SessionDocument::note_section(3, &author, &note[0])
        )
    );
    assert_eq!(
        document
            .entries
            .iter()
            .map(|entry| entry.kind)
            .collect::<Vec<_>>(),
        [
            SessionDocumentEntryKind::Replace,
            SessionDocumentEntryKind::Replace,
            SessionDocumentEntryKind::Note,
        ]
    );
}

#[tokio::test]
async fn invalid_writes_change_nothing() {
    // Arrange
    let database = database_with_session().await;
    let documents = database.session_documents();
    let author = node_author("attempt-1");
    let too_long = "b".repeat(SESSION_DOCUMENT_BODY_MAX_BYTES + 1);

    // Act
    let blank = documents
        .append_session_notes("session", &author, &["Kept.".into(), " \n".into()])
        .await;
    let missing_session = documents
        .append_session_notes("missing", &author, &["Orphan.".into()])
        .await;
    let oversized = documents
        .replace_session_document("session", &author, 0, &too_long)
        .await
        .expect("oversized replace");
    let stale = documents
        .replace_session_document("session", &author, 4, "Stale.")
        .await
        .expect("stale replace");
    let missing_replace = documents
        .replace_session_document("missing", &author, 0, "Orphan.")
        .await;
    let document = documents
        .load_session_document("session")
        .await
        .expect("document");

    // Assert
    assert!(matches!(blank, Err(DbError::InvalidData { .. })));
    assert!(missing_session.is_err());
    assert_eq!(
        oversized,
        SessionDocumentReplace::TooLong {
            bytes: SESSION_DOCUMENT_BODY_MAX_BYTES + 1
        }
    );
    assert_eq!(
        stale,
        SessionDocumentReplace::VersionConflict { current: 0 }
    );
    assert!(missing_replace.is_err());
    assert_eq!(document, SessionDocument::default());
}

#[tokio::test]
async fn the_tail_starts_at_the_newest_replacement_and_counts_body_bytes() {
    // Arrange
    let database = database_with_session().await;
    let documents = database.session_documents();
    let empty = documents
        .load_session_document_tail("session")
        .await
        .expect("empty tail");
    documents
        .append_session_notes("session", &node_author("attempt-1"), &["Old.".into()])
        .await
        .expect("old note");
    let before_replace = documents
        .load_session_document_tail("session")
        .await
        .expect("tail before a replace");
    documents
        .replace_session_document("session", &SessionDocumentAuthor::User, 1, "Ünïcode base.")
        .await
        .expect("replace");
    documents
        .append_session_notes(
            "session",
            &SessionDocumentAuthor::MainChat,
            &["New.".into()],
        )
        .await
        .expect("new note");

    // Act
    let tail = documents
        .load_session_document_tail("session")
        .await
        .expect("tail");
    let document = documents
        .load_session_document("session")
        .await
        .expect("document");

    // Assert
    assert_eq!(empty, ag_session::SessionDocumentTail::default());
    assert_eq!(before_replace.entries.len(), 1);
    assert_eq!(tail.version, 3);
    assert_eq!(tail.body_bytes, document.body.len());
    assert_eq!(
        tail.entries
            .iter()
            .map(|entry| (entry.version, entry.text.as_str()))
            .collect::<Vec<_>>(),
        [(2, "Ünïcode base."), (3, "New.")]
    );
    assert_eq!(tail.entries[1].author, SessionDocumentAuthor::MainChat);
    assert_eq!(document.entries.len(), 3);
}

#[tokio::test]
async fn storage_refuses_authors_the_loaders_would_reject() {
    // Arrange
    let database = database_with_session().await;
    let documents = database.session_documents();
    documents
        .append_session_notes("session", &SessionDocumentAuthor::User, &["Kept.".into()])
        .await
        .expect("user note");
    documents
        .append_session_notes("session", &node_author("attempt-1"), &["Found.".into()])
        .await
        .expect("node note");

    // Act
    let mut refused = Vec::new();
    for corruption in [
        "UPDATE session_document_entry SET author_kind = 'graph_node' WHERE version = 1",
        "UPDATE session_document_entry SET author_kind = 'user' WHERE version = 2",
        "UPDATE session_document_entry SET author_kind = 'main_chat' WHERE version = 2",
        "UPDATE session_document_entry SET execution_id = '' WHERE version = 2",
        "UPDATE session_document_entry SET graph_id = '' WHERE version = 2",
        "UPDATE session_document_entry SET node_id = '' WHERE version = 2",
    ] {
        refused.push(
            sqlx::query(corruption)
                .execute(database.pool())
                .await
                .is_err(),
        );
    }
    let document = documents.load_session_document("session").await;

    // Assert
    assert_eq!(refused, [true; 6]);
    assert_eq!(document.expect("document").entries.len(), 2);
}

#[test]
fn an_entry_whose_execution_does_not_match_its_author_is_invalid_data() {
    // Arrange
    let row = |author_kind: &str, kind: &str, execution: bool| super::EntryRow {
        author_kind: author_kind.into(),
        created_at: 100,
        execution_id: execution.then(|| "attempt-1".into()),
        graph_id: execution.then(|| "graph".into()),
        kind: kind.into(),
        node_id: execution.then(|| "plan".into()),
        text: "Kept.".into(),
        version: 1,
    };

    // Act, Assert
    for invalid in [
        row("graph_node", "note", false),
        row("user", "note", true),
        row("robot", "note", false),
        row("user", "append", false),
    ] {
        assert!(matches!(
            invalid.into_entry(),
            Err(DbError::InvalidData { .. })
        ));
    }
    assert!(row("graph_node", "note", true).into_entry().is_ok());
}

#[tokio::test]
async fn notes_and_bodies_are_stored_without_hidden_characters() {
    // Arrange
    let database = database_with_session().await;
    let documents = database.session_documents();
    // Hidden characters alone would push this note past the limit.
    let note = format!(
        "Keep\u{200B} the API.{}",
        "\u{E0041}".repeat(SESSION_DOCUMENT_NOTE_MAX_BYTES)
    );

    // Act
    let appended = documents
        .append_session_notes("session", &SessionDocumentAuthor::User, &[note])
        .await
        .expect("note");
    let hidden_only = documents
        .append_session_notes(
            "session",
            &SessionDocumentAuthor::User,
            &["\u{200B}".into()],
        )
        .await;
    let replaced = documents
        .replace_session_document("session", &SessionDocumentAuthor::User, 1, "\u{202E}Plan\t")
        .await
        .expect("replace");
    let document = documents
        .load_session_document("session")
        .await
        .expect("document");

    // Assert
    assert_eq!(appended, [SessionDocumentAppend::Appended { version: 1 }]);
    assert!(matches!(hidden_only, Err(DbError::InvalidData { .. })));
    assert_eq!(replaced, SessionDocumentReplace::Replaced { version: 2 });
    assert_eq!(document.entries[0].text, "Keep the API.");
    assert_eq!(
        (document.body.as_str(), document.entries[1].text.as_str()),
        ("Plan", "Plan")
    );
}

/// Opens a database with the `session` session persisted at time `100`.
async fn database_with_session() -> Database {
    let database = Database::in_memory_with_timestamp_source(Arc::new(|| 100))
        .await
        .expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await
        .expect("session");

    database
}

/// Returns the `plan` node of graph `graph` running as `execution_id`.
fn node_author(execution_id: &str) -> SessionDocumentAuthor {
    SessionDocumentAuthor::GraphNode {
        execution_id: execution_id.into(),
        graph_id: "graph".into(),
        node_id: "plan".into(),
    }
}

/// Returns the note entry `author` wrote at `version` and time `100`.
fn note_entry(author: &SessionDocumentAuthor, text: &str, version: u64) -> SessionDocumentEntry {
    SessionDocumentEntry {
        author: author.clone(),
        created_at: 100,
        kind: SessionDocumentEntryKind::Note,
        text: text.into(),
        version,
    }
}
