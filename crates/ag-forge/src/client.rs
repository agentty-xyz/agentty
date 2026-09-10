//! Public review-request trait boundary and production client wiring.

use std::sync::Arc;

use super::{
    CreateReviewRequestInput, ForgeCommandRunner, ForgeFuture, ForgeKind, ForgeRemote,
    GitHubReviewRequestAdapter, GitLabReviewRequestAdapter, RealForgeCommandRunner,
    ReviewCommentSnapshot, ReviewRequestError, ReviewRequestMetadata, ReviewRequestSummary,
    UpdateReviewRequestInput, detect_remote,
};

/// Async boundary used by app orchestration for forge review requests.
///
/// The app layer depends on this narrow contract so provider-specific request
/// formats remain isolated inside concrete adapters.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
pub trait ReviewRequestClient: Send + Sync {
    /// Detects whether `repo_url` belongs to one supported forge.
    ///
    /// # Errors
    /// Returns [`ReviewRequestError::UnsupportedRemote`] when the remote does
    /// not map to a supported forge.
    fn detect_remote(&self, repo_url: String) -> Result<ForgeRemote, ReviewRequestError>;

    /// Finds an existing review request for `source_branch`.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when the forge lookup
    /// cannot be completed.
    fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>>;

    /// Creates a new review request from `input`.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when creation fails.
    fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Refreshes one existing review request by provider display id.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when refresh fails.
    fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Loads the current title and body of one existing review request.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when metadata lookup
    /// fails.
    fn review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestMetadata, ReviewRequestError>>;

    /// Best-effort syncs reconciled metadata after rechecking that the remote
    /// fields match the values used during evaluation.
    ///
    /// The provider CLI update is not atomic with the recheck, so a later
    /// concurrent manual edit can still be overwritten.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when metadata lookup,
    /// update, or refresh fails.
    fn sync_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
        input: UpdateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Returns the browser-openable URL for one review request.
    ///
    /// # Errors
    /// Returns [`ReviewRequestError::OperationFailed`] when the summary does
    /// not carry a web URL.
    fn review_request_web_url(
        &self,
        review_request: &ReviewRequestSummary,
    ) -> Result<String, ReviewRequestError>;

    /// Fetches the review-comment snapshot for one open review request.
    ///
    /// Returns both inline threads and review-request-wide comments. Threads
    /// are grouped by `path` and sorted by `(path, line)` by callers; adapters
    /// return what the forge reports without enforcing an ordering.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when the snapshot fetch
    /// cannot be completed (including authentication and host failures).
    fn fetch_review_comment_snapshot(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewCommentSnapshot, ReviewRequestError>>;

    /// Adds one reply to an existing review thread.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when the reply cannot
    /// be posted.
    fn reply_to_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
        body: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>>;

    /// Marks one existing review thread resolved.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when the thread cannot
    /// be resolved.
    fn resolve_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>>;
}

/// Production [`ReviewRequestClient`] that routes to forge-specific adapters.
pub struct RealReviewRequestClient {
    command_runner: Arc<dyn ForgeCommandRunner>,
}

impl RealReviewRequestClient {
    /// Builds one review-request client from a forge command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self { command_runner }
    }

    /// Runs `call` on an authenticated adapter selected for `remote`.
    fn call_with_authenticated_adapter<T>(
        &self,
        remote: ForgeRemote,
        call: impl FnOnce(
            Arc<dyn ReviewRequestAdapter>,
            ForgeRemote,
        ) -> ForgeFuture<Result<T, ReviewRequestError>>
        + Send
        + 'static,
    ) -> ForgeFuture<Result<T, ReviewRequestError>>
    where
        T: Send + 'static,
    {
        let adapter = self.adapter_for(remote.forge_kind);

        Box::pin(async move {
            adapter.ensure_authenticated(&remote).await?;

            call(adapter, remote).await
        })
    }

    /// Returns one adapter implementation for `forge_kind`.
    fn adapter_for(&self, forge_kind: ForgeKind) -> Arc<dyn ReviewRequestAdapter> {
        match forge_kind {
            ForgeKind::GitHub => Arc::new(GitHubReviewRequestAdapter::new(Arc::clone(
                &self.command_runner,
            ))),
            ForgeKind::GitLab => Arc::new(GitLabReviewRequestAdapter::new(Arc::clone(
                &self.command_runner,
            ))),
        }
    }
}

impl Default for RealReviewRequestClient {
    fn default() -> Self {
        Self::new(Arc::new(RealForgeCommandRunner))
    }
}

impl ReviewRequestClient for RealReviewRequestClient {
    fn detect_remote(&self, repo_url: String) -> Result<ForgeRemote, ReviewRequestError> {
        detect_remote(&repo_url)
    }

    fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.find_authenticated_by_source_branch(remote, source_branch)
        })
    }

    fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.create_authenticated_review_request(remote, input)
        })
    }

    fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.refresh_authenticated_review_request(remote, display_id)
        })
    }

    fn review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestMetadata, ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.authenticated_review_request_metadata(remote, display_id)
        })
    }

    fn sync_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
        input: UpdateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.sync_authenticated_review_request_metadata(remote, display_id, input)
        })
    }

    fn review_request_web_url(
        &self,
        review_request: &ReviewRequestSummary,
    ) -> Result<String, ReviewRequestError> {
        if review_request.web_url.trim().is_empty() {
            return Err(ReviewRequestError::OperationFailed {
                forge_kind: review_request.forge_kind,
                message: "review request summary is missing a web URL".to_string(),
            });
        }

        Ok(review_request.web_url.clone())
    }

    fn fetch_review_comment_snapshot(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewCommentSnapshot, ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.fetch_authenticated_review_comment_snapshot(remote, display_id)
        })
    }

    fn reply_to_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
        body: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.reply_to_authenticated_thread(remote, display_id, thread_id, body)
        })
    }

    fn resolve_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.resolve_authenticated_thread(remote, display_id, thread_id)
        })
    }
}

/// Provider-specific operation boundary used after client-level authentication.
///
/// The production client selects one implementation, calls
/// [`ReviewRequestAdapter::ensure_authenticated`] once, and then invokes the
/// requested operation without provider-specific dispatch in each public
/// method.
pub(crate) trait ReviewRequestAdapter: Send + Sync {
    /// Verifies that CLI authentication succeeds for `remote`.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when the forge CLI is
    /// unavailable, unauthenticated, or cannot resolve the target host.
    fn ensure_authenticated(
        &self,
        remote: &ForgeRemote,
    ) -> ForgeFuture<Result<(), ReviewRequestError>>;

    /// Finds one review request after the production client has authenticated.
    fn find_authenticated_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>>;

    /// Creates one review request after the production client has
    /// authenticated.
    fn create_authenticated_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Refreshes one existing review request after authentication.
    fn refresh_authenticated_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Loads current review-request metadata after authentication.
    fn authenticated_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestMetadata, ReviewRequestError>>;

    /// Synchronizes review-request metadata after authentication.
    fn sync_authenticated_review_request_metadata(
        &self,
        remote: ForgeRemote,
        display_id: String,
        input: UpdateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Fetches a review-comment snapshot after authentication.
    fn fetch_authenticated_review_comment_snapshot(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewCommentSnapshot, ReviewRequestError>>;

    /// Adds one reply after authentication.
    fn reply_to_authenticated_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
        body: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>>;

    /// Resolves one review thread after authentication.
    fn resolve_authenticated_thread(
        &self,
        remote: ForgeRemote,
        display_id: String,
        thread_id: String,
    ) -> ForgeFuture<Result<(), ReviewRequestError>>;
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
