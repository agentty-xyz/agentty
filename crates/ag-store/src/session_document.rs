//! Session documents: each session's shared notes, written one transaction
//! at a time.

use std::sync::Arc;

use ag_session::{
    SESSION_DOCUMENT_BODY_MAX_BYTES, SESSION_DOCUMENT_NOTE_MAX_BYTES, SessionDocument,
    SessionDocumentAppend, SessionDocumentAuthor, SessionDocumentEntry, SessionDocumentEntryKind,
    SessionDocumentReplace, SessionDocumentTail,
};
use async_trait::async_trait;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::connection::{begin_read, begin_write};
use crate::{DbError, TimestampSource};

/// Persistence boundary for session documents.
///
/// Every write runs in one write transaction, which is the document's lock:
/// writes from overlapping nodes queue on it and land in commit order, each
/// taking the next version, so none is lost. Deleting a session deletes its
/// document.
#[async_trait]
pub trait SessionDocumentRepository: Send + Sync {
    /// Reads a session's document and every entry in version order. A
    /// session that never wrote one, or that does not exist, reads as the
    /// empty document at version zero.
    ///
    /// # Errors
    /// Fails when a stored entry names an unknown author or kind, or an
    /// author whose execution does not match its kind.
    async fn load_session_document(&self, session_id: &str) -> Result<SessionDocument, DbError>;

    /// Reads what nodes read of a session's document: its version, its
    /// body's size, and the newest replacement with every entry after it,
    /// or every entry when nothing replaced the body.
    ///
    /// # Errors
    /// Fails as [`Self::load_session_document`] does.
    async fn load_session_document_tail(
        &self,
        session_id: &str,
    ) -> Result<SessionDocumentTail, DbError>;

    /// Appends `notes`, in order, to a session's document in one
    /// transaction, and returns one outcome per note. Each accepted note
    /// adds a headed section naming its author and takes the next version.
    ///
    /// Each note is stored as [`SessionDocument::visible_text`] returns it,
    /// and measured after that. An author that ran as an execution adds
    /// notes once: when its execution already wrote any entry, a redelivered
    /// response reports [`SessionDocumentAppend::AlreadyAppended`] for every
    /// note and adds nothing, even when its notes differ. A note longer than
    /// [`SESSION_DOCUMENT_NOTE_MAX_BYTES`], or one that would grow the body
    /// past [`SESSION_DOCUMENT_BODY_MAX_BYTES`], is refused and not stored;
    /// later notes are still considered.
    ///
    /// # Errors
    /// Fails when a note is blank or the session is missing, writing
    /// nothing.
    async fn append_session_notes(
        &self,
        session_id: &str,
        author: &SessionDocumentAuthor,
        notes: &[String],
    ) -> Result<Vec<SessionDocumentAppend>, DbError>;

    /// Replaces the whole body of a session's document, as `author`, when
    /// it is still at `expected_version`; a session that never wrote one is
    /// at version zero. The body is stored as
    /// [`SessionDocument::visible_text`] returns it, and measured after
    /// that. A mismatch or a body longer than
    /// [`SESSION_DOCUMENT_BODY_MAX_BYTES`] changes nothing. An empty body
    /// clears the document.
    ///
    /// # Errors
    /// Fails when the session is missing.
    async fn replace_session_document(
        &self,
        session_id: &str,
        author: &SessionDocumentAuthor,
        expected_version: u64,
        body: &str,
    ) -> Result<SessionDocumentReplace, DbError>;
}

/// `SQLite` implementation of [`SessionDocumentRepository`].
#[derive(Clone)]
pub(crate) struct SqliteSessionDocumentRepository {
    pool: SqlitePool,
    timestamp_source: Arc<dyn TimestampSource>,
}

impl SqliteSessionDocumentRepository {
    /// Creates a session document repository backed by the provided pool and
    /// timestamp source.
    pub(crate) fn new(pool: SqlitePool, timestamp_source: Arc<dyn TimestampSource>) -> Self {
        Self {
            pool,
            timestamp_source,
        }
    }

    /// Reads the current body and version, or the empty document at version
    /// zero.
    async fn current(
        transaction: &mut Transaction<'_, Sqlite>,
        session_id: &str,
    ) -> Result<(String, u64), DbError> {
        let row = sqlx::query!(
            r"
SELECT body, version
FROM session_document
WHERE session_id = ?
",
            session_id
        )
        .fetch_optional(&mut **transaction)
        .await?;

        Ok(row.map_or_else(
            || (String::new(), 0),
            |row| (row.body, row.version.cast_unsigned()),
        ))
    }

    /// Reads the entries from `since_version` on, in version order.
    async fn entries_since(
        transaction: &mut Transaction<'_, Sqlite>,
        session_id: &str,
        since_version: i64,
    ) -> Result<Vec<SessionDocumentEntry>, DbError> {
        let rows = sqlx::query_as!(
            EntryRow,
            r#"
SELECT version AS "version!: i64",
       kind,
       author_kind,
       graph_id,
       node_id,
       execution_id,
       text,
       created_at
FROM session_document_entry
WHERE session_id = ? AND version >= ?
ORDER BY version
"#,
            session_id,
            since_version
        )
        .fetch_all(&mut **transaction)
        .await?;

        rows.into_iter().map(EntryRow::into_entry).collect()
    }

    /// Returns the note position and version of every entry `author`'s
    /// execution already wrote, oldest first; none for an author that did
    /// not run as an execution.
    async fn execution_entries(
        transaction: &mut Transaction<'_, Sqlite>,
        session_id: &str,
        author: &SessionDocumentAuthor,
    ) -> Result<Vec<(Option<i64>, u64)>, DbError> {
        let Some((execution_id, _, _)) = author.execution() else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query!(
            r#"
SELECT note_index, version AS "version!: i64"
FROM session_document_entry
WHERE session_id = ? AND execution_id = ?
ORDER BY version
"#,
            session_id,
            execution_id
        )
        .fetch_all(&mut **transaction)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.note_index, row.version.cast_unsigned()))
            .collect())
    }

    /// Stores `body` at `version` together with the entry that produced it.
    async fn write(
        transaction: &mut Transaction<'_, Sqlite>,
        session_id: &str,
        write: DocumentWrite<'_>,
    ) -> Result<(), DbError> {
        let version = write.version.cast_signed();
        sqlx::query!(
            r"
INSERT INTO session_document (session_id, version, body, updated_at)
VALUES (?, ?, ?, ?)
ON CONFLICT (session_id) DO UPDATE
SET version = excluded.version, body = excluded.body, updated_at = excluded.updated_at
",
            session_id,
            version,
            write.body,
            write.created_at
        )
        .execute(&mut **transaction)
        .await?;
        let execution = write.author.execution();
        let (execution_id, graph_id, node_id) = (
            execution.map(|(execution_id, _, _)| execution_id),
            execution.map(|(_, graph_id, _)| graph_id),
            execution.map(|(_, _, node_id)| node_id),
        );
        // Only an execution's note has a position to deduplicate by.
        let note_index = write.note_index.filter(|_| execution.is_some());
        let author_kind = write.author.kind_name();
        let kind = write.kind.name();
        sqlx::query!(
            r"
INSERT INTO session_document_entry (
    session_id, version, kind, author_kind, graph_id, node_id, execution_id, note_index, text,
    created_at
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
",
            session_id,
            version,
            kind,
            author_kind,
            graph_id,
            node_id,
            execution_id,
            note_index,
            write.text,
            write.created_at
        )
        .execute(&mut **transaction)
        .await?;

        Ok(())
    }
}

/// One stored entry, as the loaders read it.
struct EntryRow {
    author_kind: String,
    created_at: i64,
    execution_id: Option<String>,
    graph_id: Option<String>,
    kind: String,
    node_id: Option<String>,
    text: String,
    version: i64,
}

impl EntryRow {
    /// Rebuilds the entry, refusing an unknown author or kind, and an
    /// author whose execution does not match its kind.
    fn into_entry(self) -> Result<SessionDocumentEntry, DbError> {
        let execution = self
            .execution_id
            .zip(self.graph_id)
            .zip(self.node_id)
            .map(|((execution_id, graph_id), node_id)| (execution_id, graph_id, node_id));
        let author = SessionDocumentAuthor::from_stored(&self.author_kind, execution)
            .ok_or_else(Self::invalid)?;
        let kind = SessionDocumentEntryKind::from_stored(&self.kind).ok_or_else(Self::invalid)?;

        Ok(SessionDocumentEntry {
            author,
            created_at: self.created_at,
            kind,
            text: self.text,
            version: self.version.cast_unsigned(),
        })
    }

    /// Returns the error for an entry the loaders cannot rebuild.
    fn invalid() -> DbError {
        DbError::InvalidData {
            entity: "session document entry",
            reason: "unknown author or kind, or an execution that does not match its author".into(),
        }
    }
}

/// One accepted write, as [`SqliteSessionDocumentRepository::write`] stores
/// it.
struct DocumentWrite<'a> {
    author: &'a SessionDocumentAuthor,
    body: &'a str,
    created_at: i64,
    kind: SessionDocumentEntryKind,
    note_index: Option<i64>,
    text: &'a str,
    version: u64,
}

#[async_trait]
impl SessionDocumentRepository for SqliteSessionDocumentRepository {
    async fn load_session_document(&self, session_id: &str) -> Result<SessionDocument, DbError> {
        let mut transaction = begin_read(&self.pool, "load session document").await?;
        let (body, version) = Self::current(&mut transaction, session_id).await?;
        let entries = Self::entries_since(&mut transaction, session_id, 0).await?;
        transaction.commit().await?;

        Ok(SessionDocument {
            body,
            entries,
            version,
        })
    }

    async fn load_session_document_tail(
        &self,
        session_id: &str,
    ) -> Result<SessionDocumentTail, DbError> {
        let mut transaction = begin_read(&self.pool, "load session document tail").await?;
        let state = sqlx::query!(
            r#"
SELECT document.version AS "version!: i64",
       length(CAST(document.body AS BLOB)) AS "body_bytes!: i64",
       (
           SELECT coalesce(max(entry.version), 0)
           FROM session_document_entry AS entry
           WHERE entry.session_id = document.session_id AND entry.kind = 'replace'
       ) AS "since_version!: i64"
FROM session_document AS document
WHERE document.session_id = ?
"#,
            session_id
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(state) = state else {
            return Ok(SessionDocumentTail::default());
        };
        let entries =
            Self::entries_since(&mut transaction, session_id, state.since_version).await?;
        transaction.commit().await?;

        Ok(SessionDocumentTail {
            body_bytes: usize::try_from(state.body_bytes).unwrap_or(usize::MAX),
            entries,
            version: state.version.cast_unsigned(),
        })
    }

    async fn append_session_notes(
        &self,
        session_id: &str,
        author: &SessionDocumentAuthor,
        notes: &[String],
    ) -> Result<Vec<SessionDocumentAppend>, DbError> {
        let notes = notes
            .iter()
            .map(|note| SessionDocument::visible_text(note))
            .collect::<Vec<_>>();
        if notes.iter().any(String::is_empty) {
            return Err(DbError::InvalidData {
                entity: "session document note",
                reason: "a note must not be blank".into(),
            });
        }
        let mut transaction = begin_write(&self.pool, "append session notes").await?;
        let published = Self::execution_entries(&mut transaction, session_id, author).await?;
        if let Some(&(_, newest)) = published.last() {
            transaction.commit().await?;

            return Ok((0_i64..)
                .take(notes.len())
                .map(|note_index| {
                    let version = published
                        .iter()
                        .find(|(index, _)| *index == Some(note_index))
                        .map_or(newest, |&(_, version)| version);

                    SessionDocumentAppend::AlreadyAppended { version }
                })
                .collect());
        }
        let (mut body, mut version) = Self::current(&mut transaction, session_id).await?;
        let created_at = self.timestamp_source.now_timestamp_seconds();
        let mut outcomes = Vec::with_capacity(notes.len());
        for (note_index, note) in (0_i64..).zip(&notes) {
            if note.len() > SESSION_DOCUMENT_NOTE_MAX_BYTES {
                outcomes.push(SessionDocumentAppend::NoteTooLong { bytes: note.len() });
                continue;
            }
            let next_version = version + 1;
            let next_body = SessionDocument::appended_body(
                &body,
                &SessionDocument::note_section(next_version, author, note),
            );
            if next_body.len() > SESSION_DOCUMENT_BODY_MAX_BYTES {
                outcomes.push(SessionDocumentAppend::DocumentFull {
                    bytes: next_body.len(),
                });
                continue;
            }
            Self::write(
                &mut transaction,
                session_id,
                DocumentWrite {
                    author,
                    body: &next_body,
                    created_at,
                    kind: SessionDocumentEntryKind::Note,
                    note_index: Some(note_index),
                    text: note,
                    version: next_version,
                },
            )
            .await?;
            (body, version) = (next_body, next_version);
            outcomes.push(SessionDocumentAppend::Appended { version });
        }
        transaction.commit().await?;

        Ok(outcomes)
    }

    async fn replace_session_document(
        &self,
        session_id: &str,
        author: &SessionDocumentAuthor,
        expected_version: u64,
        body: &str,
    ) -> Result<SessionDocumentReplace, DbError> {
        let body = SessionDocument::visible_text(body);
        let body = body.as_str();
        if body.len() > SESSION_DOCUMENT_BODY_MAX_BYTES {
            return Ok(SessionDocumentReplace::TooLong { bytes: body.len() });
        }
        let mut transaction = begin_write(&self.pool, "replace session document").await?;
        let (_, current) = Self::current(&mut transaction, session_id).await?;
        if current != expected_version {
            return Ok(SessionDocumentReplace::VersionConflict { current });
        }
        let version = current + 1;
        Self::write(
            &mut transaction,
            session_id,
            DocumentWrite {
                author,
                body,
                created_at: self.timestamp_source.now_timestamp_seconds(),
                kind: SessionDocumentEntryKind::Replace,
                note_index: None,
                text: body,
                version,
            },
        )
        .await?;
        transaction.commit().await?;

        Ok(SessionDocumentReplace::Replaced { version })
    }
}

#[cfg(test)]
#[path = "session_document_test.rs"]
mod tests;
