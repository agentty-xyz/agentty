//! Session document contracts through the public, migrated SQLite adapter:
//! concurrent appends, version-checked replacement, and session deletion.

use std::error::Error;

use ag_session::{
    SessionDocument, SessionDocumentAppend, SessionDocumentAuthor, SessionDocumentReplace,
};
use ag_store::Database;

/// Overlapping nodes, each with its own connection pool.
const WRITERS: usize = 8;

#[tokio::test]
async fn overlapping_nodes_append_concurrently_without_losing_a_note() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("session-document-")
        .tempdir_in(".")?;
    let path = directory.path().join("document.sqlite");
    let database = Database::open(&path).await?;
    seed_session(&database).await?;
    let mut writers = tokio::task::JoinSet::new();
    for writer in 0..WRITERS {
        let path = path.clone();
        writers.spawn(async move {
            let database = Database::open(&path).await?;

            database
                .session_documents()
                .append_session_notes(
                    "session",
                    &node(&format!("node-{writer}")),
                    &[format!("Finding {writer}")],
                )
                .await
        });
    }

    // Act
    let mut versions = Vec::new();
    while let Some(outcome) = writers.join_next().await {
        match outcome??.as_slice() {
            [SessionDocumentAppend::Appended { version }] => versions.push(*version),
            other => return Err(format!("Unexpected append outcome {other:?}").into()),
        }
    }
    let document = database
        .session_documents()
        .load_session_document("session")
        .await?;

    // Assert
    versions.sort_unstable();
    assert_eq!(versions, (1..=WRITERS as u64).collect::<Vec<_>>());
    assert_eq!(document.version, WRITERS as u64);
    let mut body = String::new();
    for (entry, version) in document.entries.iter().zip(1..) {
        assert_eq!(entry.version, version);
        body = SessionDocument::appended_body(
            &body,
            &SessionDocument::note_section(version, &entry.author, &entry.text),
        );
    }
    assert_eq!(document.body, body);
    let mut notes = document
        .entries
        .iter()
        .map(|entry| entry.text.clone())
        .collect::<Vec<_>>();
    notes.sort_unstable();
    let mut expected = (0..WRITERS)
        .map(|writer| format!("Finding {writer}"))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(notes, expected);

    Ok(())
}

#[tokio::test]
async fn a_replace_needs_the_current_version_and_the_document_goes_with_its_session()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed_session(&database).await?;
    let documents = database.session_documents();
    let author = node("plan");
    documents
        .append_session_notes("session", &author, &["Plan: change the parser.".into()])
        .await?;

    // Act
    let stale = documents
        .replace_session_document("session", &author, 0, "Overwritten.")
        .await?;
    let replaced = documents
        .replace_session_document("session", &author, 1, "Decision: keep the parser.")
        .await?;
    let appended = documents
        .append_session_notes("session", &node("check"), &["Checked.".into()])
        .await?;
    let before_deletion = documents.load_session_document("session").await?;
    database.sessions().delete_session("session").await?;
    let after_deletion = documents.load_session_document("session").await?;

    // Assert
    assert_eq!(
        stale,
        SessionDocumentReplace::VersionConflict { current: 1 }
    );
    assert_eq!(replaced, SessionDocumentReplace::Replaced { version: 2 });
    assert_eq!(appended, [SessionDocumentAppend::Appended { version: 3 }]);
    assert!(
        before_deletion
            .body
            .starts_with("Decision: keep the parser.\n\n## Note 3 from node `check`")
    );
    assert_eq!(before_deletion.entries.len(), 3);
    assert_eq!(after_deletion, SessionDocument::default());

    Ok(())
}

#[tokio::test]
async fn the_main_chat_writes_without_an_execution_and_reads_back_as_itself()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed_session(&database).await?;
    let documents = database.session_documents();
    let note = vec!["Plan the parser rewrite in two graphs.".to_string()];

    // Act
    let first = documents
        .append_session_notes("session", &SessionDocumentAuthor::MainChat, &note)
        .await?;
    let second = documents
        .append_session_notes("session", &SessionDocumentAuthor::MainChat, &note)
        .await?;
    let replaced = documents
        .replace_session_document("session", &SessionDocumentAuthor::MainChat, 2, "Compacted.")
        .await?;
    let document = documents.load_session_document("session").await?;

    // Assert: with no execution to identify it, each write is a new entry.
    assert_eq!(first, [SessionDocumentAppend::Appended { version: 1 }]);
    assert_eq!(second, [SessionDocumentAppend::Appended { version: 2 }]);
    assert_eq!(replaced, SessionDocumentReplace::Replaced { version: 3 });
    assert!(
        document
            .entries
            .iter()
            .all(|entry| entry.author == SessionDocumentAuthor::MainChat)
    );
    assert!(document.body.starts_with("Compacted."));

    Ok(())
}

async fn seed_session(database: &Database) -> Result<(), Box<dyn Error>> {
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;

    Ok(())
}

/// Returns node `node_id` of graph `graph` running as its own execution.
fn node(node_id: &str) -> SessionDocumentAuthor {
    SessionDocumentAuthor::GraphNode {
        execution_id: format!("attempt-{node_id}"),
        graph_id: "graph".into(),
        node_id: node_id.into(),
    }
}
