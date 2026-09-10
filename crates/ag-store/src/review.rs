//! Session review-request persistence adapters and query helpers.

use ag_session::ReviewRequest;
use async_trait::async_trait;
use sqlx::{SqliteConnection, SqlitePool};

use crate::DbError;

/// Durable input for one review-comment resolution operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewSessionReviewCommentResolution {
    /// Full commit hash containing the reported fix, once auto-commit succeeds.
    pub commit_hash: Option<String>,
    /// Original agent-authored reply retained across retries.
    pub reply: String,
    /// Unguessable token embedded in the posted reply.
    pub reply_token: String,
    /// Persisted normalized resolution decision.
    pub resolution: String,
    /// Forge-native review-request identifier such as `#123` or `!123`.
    pub review_request_display_id: String,
    /// Forge-native review-thread identifier.
    pub thread_id: String,
}

/// Row returned for one unfinished review-comment resolution operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionReviewCommentResolutionRow {
    /// Full commit hash that must remain reachable from the pushed branch.
    pub commit_hash: Option<String>,
    /// Whether a forge reply attempt may already have exposed the token.
    pub is_posting: bool,
    /// Original agent-authored reply retained across retries.
    pub reply: String,
    /// Unguessable token embedded in the posted reply.
    pub reply_token: String,
    /// Persisted normalized resolution decision.
    pub resolution: String,
    /// Forge-native review-request identifier such as `#123` or `!123`.
    pub review_request_display_id: String,
    /// Forge-native review-thread identifier.
    pub thread_id: String,
}

/// Row returned when loading one `session_review_request`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionReviewRequestRow {
    /// Forge-native display identifier such as `#123` or `!123`.
    pub display_id: String,
    /// Persisted forge-family discriminator.
    pub forge_kind: String,
    /// Most recent successful refresh timestamp in Unix seconds.
    pub last_refreshed_at: i64,
    /// Review request source branch.
    pub source_branch: String,
    /// Persisted normalized lifecycle state.
    pub state: String,
    /// Optional normalized checks or merge-status summary.
    pub status_summary: Option<String>,
    /// Review request target branch.
    pub target_branch: String,
    /// Review request title.
    pub title: String,
    /// Browser-openable review request URL.
    pub web_url: String,
}

/// Review-request persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait ReviewRepository: Send + Sync {
    /// Binds newly inserted operations to the commit produced for their turn.
    async fn bind_session_review_comment_resolutions_to_commit(
        &self,
        id: &str,
        resolutions: &[NewSessionReviewCommentResolution],
        commit_hash: &str,
    ) -> Result<(), DbError>;

    /// Discards unfinished operations created with the supplied reply tokens.
    async fn discard_session_review_comment_resolutions(
        &self,
        id: &str,
        resolutions: &[NewSessionReviewCommentResolution],
    ) -> Result<(), DbError>;

    /// Inserts one atomic set of review-comment resolution operations.
    ///
    /// A bound operation for the same thread wins. A fresh agent turn may
    /// replace an unbound operation left by interrupted commit binding.
    async fn insert_session_review_comment_resolutions(
        &self,
        id: &str,
        resolutions: &[NewSessionReviewCommentResolution],
    ) -> Result<(), DbError>;

    /// Loads unfinished review-comment resolution operations for a session.
    async fn load_session_review_comment_resolutions(
        &self,
        id: &str,
    ) -> Result<Vec<SessionReviewCommentResolutionRow>, DbError>;

    /// Loads the persisted forge review-request linkage for a session.
    async fn load_session_review_request(
        &self,
        id: &str,
    ) -> Result<Option<SessionReviewRequestRow>, DbError>;

    /// Updates the persisted forge review-request linkage for a session.
    async fn update_session_review_request(
        &self,
        id: &str,
        review_request: Option<ReviewRequest>,
    ) -> Result<(), DbError>;

    /// Records that one operation is about to attempt its forge reply.
    async fn mark_session_review_comment_resolution_posting(
        &self,
        id: &str,
        reply_token: &str,
    ) -> Result<(), DbError>;

    /// Removes one operation after its requested forge effects finish.
    async fn remove_session_review_comment_resolution(
        &self,
        id: &str,
        reply_token: &str,
    ) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`ReviewRepository`].
#[derive(Clone)]
pub(crate) struct SqliteReviewRepository(SqlitePool);

impl SqliteReviewRepository {
    /// Creates a review repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

/// Inserts review-comment resolutions through the caller's transaction.
pub(crate) async fn insert_review_comment_resolutions(
    connection: &mut SqliteConnection,
    session_id: &str,
    resolutions: &[NewSessionReviewCommentResolution],
) -> Result<(), DbError> {
    for resolution in resolutions {
        sqlx::query!(
            r"
INSERT INTO session_review_comment_resolution (
    session_id,
    commit_hash,
    review_request_display_id,
    thread_id,
    reply,
    reply_token,
    resolution
)
VALUES (?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(session_id, review_request_display_id, thread_id)
DO UPDATE SET
    commit_hash = excluded.commit_hash,
    reply = excluded.reply,
    reply_token = excluded.reply_token,
    resolution = excluded.resolution,
    is_posting = 0
WHERE session_review_comment_resolution.commit_hash IS NULL
",
            session_id,
            resolution.commit_hash,
            resolution.review_request_display_id,
            resolution.thread_id,
            resolution.reply,
            resolution.reply_token,
            resolution.resolution
        )
        .execute(&mut *connection)
        .await?;
    }

    Ok(())
}

#[async_trait]
impl ReviewRepository for SqliteReviewRepository {
    async fn bind_session_review_comment_resolutions_to_commit(
        &self,
        id: &str,
        resolutions: &[NewSessionReviewCommentResolution],
        commit_hash: &str,
    ) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;
        for resolution in resolutions {
            sqlx::query!(
                r"
UPDATE session_review_comment_resolution
SET commit_hash = ?
WHERE session_id = ?
  AND reply_token = ?
  AND commit_hash IS NULL
",
                commit_hash,
                id,
                resolution.reply_token
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn discard_session_review_comment_resolutions(
        &self,
        id: &str,
        resolutions: &[NewSessionReviewCommentResolution],
    ) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;
        for resolution in resolutions {
            sqlx::query!(
                r"
DELETE FROM session_review_comment_resolution
WHERE session_id = ?
  AND reply_token = ?
",
                id,
                resolution.reply_token
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn insert_session_review_comment_resolutions(
        &self,
        id: &str,
        resolutions: &[NewSessionReviewCommentResolution],
    ) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;
        insert_review_comment_resolutions(&mut transaction, id, resolutions).await?;
        transaction.commit().await?;

        Ok(())
    }

    async fn load_session_review_comment_resolutions(
        &self,
        id: &str,
    ) -> Result<Vec<SessionReviewCommentResolutionRow>, DbError> {
        let resolutions = sqlx::query_as!(
            SessionReviewCommentResolutionRow,
            r#"
SELECT is_posting AS "is_posting: bool",
       commit_hash,
       reply,
       reply_token,
       resolution,
       review_request_display_id,
       thread_id
FROM session_review_comment_resolution
WHERE session_id = ?
ORDER BY rowid
"#,
            id
        )
        .fetch_all(&self.0)
        .await?;

        Ok(resolutions)
    }

    async fn load_session_review_request(
        &self,
        id: &str,
    ) -> Result<Option<SessionReviewRequestRow>, DbError> {
        let review_request = sqlx::query_as!(
            SessionReviewRequestRow,
            r"
SELECT display_id,
       forge_kind,
       last_refreshed_at,
       source_branch,
       state,
       status_summary,
       target_branch,
       title,
       web_url
FROM session_review_request
WHERE session_id = ?
",
            id
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(review_request)
    }

    async fn update_session_review_request(
        &self,
        id: &str,
        review_request: Option<ReviewRequest>,
    ) -> Result<(), DbError> {
        if let Some(review_request) = review_request.as_ref() {
            sqlx::query!(
                r"
INSERT INTO session_review_request (
    session_id,
    display_id,
    forge_kind,
    last_refreshed_at,
    source_branch,
    state,
    status_summary,
    target_branch,
    title,
    web_url
)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(session_id) DO UPDATE
SET display_id = excluded.display_id,
    forge_kind = excluded.forge_kind,
    last_refreshed_at = excluded.last_refreshed_at,
    source_branch = excluded.source_branch,
    state = excluded.state,
    status_summary = excluded.status_summary,
    target_branch = excluded.target_branch,
    title = excluded.title,
    web_url = excluded.web_url
",
                id,
                review_request.summary.display_id.as_str(),
                review_request.summary.forge_kind.as_str(),
                review_request.last_refreshed_at,
                review_request.summary.source_branch.as_str(),
                review_request.summary.state.as_str(),
                review_request.summary.status_summary.as_deref(),
                review_request.summary.target_branch.as_str(),
                review_request.summary.title.as_str(),
                review_request.summary.web_url.as_str()
            )
            .execute(&self.0)
            .await?;
        } else {
            sqlx::query!(
                r"
DELETE FROM session_review_request
WHERE session_id = ?
",
                id
            )
            .execute(&self.0)
            .await?;
        }

        Ok(())
    }

    async fn mark_session_review_comment_resolution_posting(
        &self,
        id: &str,
        reply_token: &str,
    ) -> Result<(), DbError> {
        let update_result = sqlx::query!(
            r"
UPDATE session_review_comment_resolution
SET is_posting = 1
WHERE session_id = ?
  AND reply_token = ?
  AND is_posting = 0
",
            id,
            reply_token
        )
        .execute(&self.0)
        .await?;
        if update_result.rows_affected() != 1 {
            return Err(DbError::InvalidData {
                entity: "review-comment operation",
                reason: format!("posting update matched no pending row for token `{reply_token}`"),
            });
        }

        Ok(())
    }

    async fn remove_session_review_comment_resolution(
        &self,
        id: &str,
        reply_token: &str,
    ) -> Result<(), DbError> {
        let delete_result = sqlx::query!(
            r"
DELETE FROM session_review_comment_resolution
WHERE session_id = ?
  AND reply_token = ?
",
            id,
            reply_token
        )
        .execute(&self.0)
        .await?;
        if delete_result.rows_affected() != 1 {
            return Err(DbError::InvalidData {
                entity: "review-comment operation",
                reason: format!("delete matched no row for token `{reply_token}`"),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "review_test.rs"]
mod tests;
