//! Public review-request trait boundary and production client wiring.

use std::sync::Arc;

use super::{
    CreateReviewRequestInput, ForgeCommandRunner, ForgeFuture, ForgeKind, ForgeRemote,
    GitHubReviewRequestAdapter, GitLabReviewRequestAdapter, RealForgeCommandRunner,
    RequestedReview, ReviewCommentSnapshot, ReviewRequestError, ReviewRequestMetadata,
    ReviewRequestSummary, UpdateReviewRequestInput, detect_remote,
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

    /// Lists open review requests asking the current authenticated user to
    /// review the selected repository.
    ///
    /// # Errors
    /// Returns a provider-specific review-request error when the list fetch
    /// cannot be completed.
    fn list_requested_reviews(
        &self,
        remote: ForgeRemote,
    ) -> ForgeFuture<Result<Vec<RequestedReview>, ReviewRequestError>>;
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

    fn list_requested_reviews(
        &self,
        remote: ForgeRemote,
    ) -> ForgeFuture<Result<Vec<RequestedReview>, ReviewRequestError>> {
        self.call_with_authenticated_adapter(remote, move |adapter, remote| {
            adapter.list_authenticated_requested_reviews(remote)
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

    /// Lists requested reviews after authentication.
    fn list_authenticated_requested_reviews(
        &self,
        remote: ForgeRemote,
    ) -> ForgeFuture<Result<Vec<RequestedReview>, ReviewRequestError>>;
}

#[cfg(test)]
mod tests {
    use mockall::Sequence;

    use super::*;
    use crate::command::{ForgeCommand, ForgeCommandOutput, MockForgeCommandRunner};
    use crate::{ForgeKind, ReviewRequestState};

    #[test]
    fn review_request_web_url_returns_error_when_summary_is_missing_url() {
        // Arrange
        let client = RealReviewRequestClient::default();
        let review_request = ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Mergeable".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge boundary".to_string(),
            web_url: String::new(),
        };

        // Act
        let error = client
            .review_request_web_url(&review_request)
            .expect_err("missing URL should be rejected");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "review request summary is missing a web URL".to_string(),
            }
        );
    }

    #[test]
    fn review_request_web_url_returns_gitlab_url_without_provider_routing() {
        // Arrange
        let client = RealReviewRequestClient::default();
        let review_request = ReviewRequestSummary {
            display_id: "!42".to_string(),
            forge_kind: ForgeKind::GitLab,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge boundary".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
        };

        // Act
        let web_url = client
            .review_request_web_url(&review_request)
            .expect("gitlab review-request URL should be returned directly");

        // Assert
        assert_eq!(
            web_url,
            "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
        );
    }

    #[tokio::test]
    async fn find_by_source_branch_authenticates_once_before_github_lookup() {
        // Arrange
        let remote = github_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "gh",
                    &["auth", "status", "--hostname", "github.com"],
                )
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "gh",
                    &[
                        "api",
                        "--hostname",
                        "github.com",
                        "--method",
                        "GET",
                        "repos/agentty-xyz/agentty/pulls",
                        "-f",
                        "head=agentty-xyz:feature/forge",
                        "-f",
                        "state=open",
                        "-f",
                        "sort=created",
                        "-f",
                        "direction=desc",
                        "-f",
                        "per_page=1",
                    ],
                )
            })
            .returning(|_| {
                Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) })
            });
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "gh",
                    &[
                        "pr",
                        "view",
                        "42",
                        "--repo",
                        "agentty-xyz/agentty",
                        "--json",
                        "number,title,state,url,baseRefName,headRefName,isDraft,mergeStateStatus,\
                         reviewDecision,mergedAt",
                    ],
                )
            })
            .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
        let client = RealReviewRequestClient::new(Arc::new(command_runner));

        // Act
        let review_request = client
            .find_by_source_branch(remote, "feature/forge".to_string())
            .await
            .expect("GitHub lookup should succeed");

        // Assert
        assert_eq!(
            review_request,
            Some(ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "feature/forge".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Approved, Mergeable".to_string()),
                target_branch: "main".to_string(),
                title: "Add forge review support".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn review_request_metadata_authenticates_before_github_lookup() {
        // Arrange
        let remote = github_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "gh",
                    &["auth", "status", "--hostname", "github.com"],
                )
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "gh",
                    &[
                        "pr",
                        "view",
                        "42",
                        "--repo",
                        "agentty-xyz/agentty",
                        "--json",
                        "title,body",
                    ],
                )
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(success_output(
                        r#"{"title":"Current title","body":"Current body"}"#.to_string(),
                    ))
                })
            });
        let client = RealReviewRequestClient::new(Arc::new(command_runner));

        // Act
        let metadata = client
            .review_request_metadata(remote, "#42".to_string())
            .await
            .expect("GitHub metadata lookup should succeed");

        // Assert
        assert_eq!(
            metadata,
            ReviewRequestMetadata {
                body: "Current body".to_string(),
                title: "Current title".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn refresh_review_request_stops_on_github_authentication_error() {
        // Arrange
        let remote = github_remote();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .withf(|command| {
                command_arguments_are(
                    command,
                    "gh",
                    &["auth", "status", "--hostname", "github.com"],
                )
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(failure_output(
                        "You are not logged into any GitHub hosts. Run `gh auth login`."
                            .to_string(),
                    ))
                })
            });
        let client = RealReviewRequestClient::new(Arc::new(command_runner));

        // Act
        let error = client
            .refresh_review_request(remote, "#42".to_string())
            .await
            .expect_err("missing auth should stop before refresh");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::AuthenticationRequired {
                detail: Some(
                    "You are not logged into any GitHub hosts. Run `gh auth login`.".to_string()
                ),
                forge_kind: ForgeKind::GitHub,
                host: "github.com".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn review_thread_mutations_authenticate_and_route_to_github_adapter() {
        // Arrange
        let remote = github_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        for expected_mutation in ["addPullRequestReviewThreadReply", "resolveReviewThread"] {
            command_runner
                .expect_run()
                .once()
                .in_sequence(&mut sequence)
                .withf(|command| {
                    command_arguments_are(
                        command,
                        "gh",
                        &["auth", "status", "--hostname", "github.com"],
                    )
                })
                .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
            command_runner
                .expect_run()
                .once()
                .in_sequence(&mut sequence)
                .withf(move |command| {
                    command.executable == "gh"
                        && command
                            .arguments
                            .iter()
                            .any(|argument| argument.contains(expected_mutation))
                })
                .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        }
        let client = RealReviewRequestClient::new(Arc::new(command_runner));

        // Act
        let reply_result = client
            .reply_to_thread(
                remote.clone(),
                "#42".to_string(),
                "thread-1".to_string(),
                "Addressed.".to_string(),
            )
            .await;
        let resolution_result = client
            .resolve_thread(remote, "#42".to_string(), "thread-1".to_string())
            .await;

        // Assert
        assert_eq!(reply_result, Ok(()));
        assert_eq!(resolution_result, Ok(()));
    }

    #[tokio::test]
    async fn refresh_review_request_authenticates_before_gitlab_refresh() {
        // Arrange
        let remote = gitlab_remote();
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "glab",
                    &["auth", "status", "--hostname", "gitlab.com"],
                )
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "glab",
                    &[
                        "mr",
                        "view",
                        "42",
                        "--repo",
                        "https://gitlab.com/agentty-xyz/agentty",
                        "--output",
                        "json",
                    ],
                )
            })
            .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
        let client = RealReviewRequestClient::new(Arc::new(command_runner));

        // Act
        let review_request = client
            .refresh_review_request(remote, "!42".to_string())
            .await
            .expect("GitLab refresh should succeed");

        // Assert
        assert_eq!(review_request.display_id, "!42");
        assert_eq!(review_request.forge_kind, ForgeKind::GitLab);
    }

    /// Returns whether `command` exactly matches one expected CLI invocation.
    fn command_arguments_are(
        command: &ForgeCommand,
        executable: &'static str,
        arguments: &[&str],
    ) -> bool {
        let expected_arguments = arguments
            .iter()
            .map(|argument| (*argument).to_string())
            .collect::<Vec<_>>();

        command.executable == executable && command.arguments == expected_arguments
    }

    /// Builds one normalized GitHub remote for client routing tests.
    fn github_remote() -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Builds one normalized GitLab remote for client routing tests.
    fn gitlab_remote() -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Builds one successful command output with `stdout`.
    fn success_output(stdout: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout,
        }
    }

    /// Builds one failed command output with `stderr`.
    fn failure_output(stderr: String) -> ForgeCommandOutput {
        ForgeCommandOutput {
            exit_code: Some(1),
            stderr,
            stdout: String::new(),
        }
    }

    /// Returns one representative GitHub pull-request JSON response.
    fn github_view_json() -> String {
        r#"{
            "number": 42,
            "title": "Add forge review support",
            "state": "OPEN",
            "url": "https://github.com/agentty-xyz/agentty/pull/42",
            "baseRefName": "main",
            "headRefName": "feature/forge",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "reviewDecision": "APPROVED",
            "mergedAt": null
        }"#
        .to_string()
    }

    /// Returns one representative GitLab merge-request JSON response.
    fn gitlab_view_json() -> String {
        r#"{
            "draft": true,
            "detailed_merge_status": "can_be_merged",
            "iid": 42,
            "merge_status": "can_be_merged",
            "merged_at": null,
            "source_branch": "feature/forge",
            "state": "opened",
            "target_branch": "main",
            "title": "Add forge review support",
            "description": "Current description.",
            "web_url": "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
        }"#
        .to_string()
    }
}
