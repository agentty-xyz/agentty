//! Cross-forge review-request boundary and remote detection helpers.

use std::future::Future;
use std::pin::Pin;

/// Boxed async result used by [`ReviewRequestClient`] trait methods.
pub type ForgeFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Supported forge families that Agentty can target for review requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeKind {
    /// GitHub pull requests managed through the `gh` CLI.
    GitHub,
    /// GitLab merge requests managed through the `glab` CLI.
    GitLab,
}

impl ForgeKind {
    /// Returns the user-facing forge name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
        }
    }

    /// Returns the CLI executable name used for this forge.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::GitHub => "gh",
            Self::GitLab => "glab",
        }
    }

    /// Returns the login command users should run to authorize forge access.
    pub fn auth_login_command(self) -> &'static str {
        match self {
            Self::GitHub => "gh auth login",
            Self::GitLab => "glab auth login",
        }
    }
}

/// Normalized repository remote metadata for one supported forge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeRemote {
    /// Forge family inferred from the repository remote.
    pub forge_kind: ForgeKind,
    /// Remote hostname, optionally including a port.
    pub host: String,
    /// Repository namespace or owner path.
    pub namespace: String,
    /// Repository name without a trailing `.git` suffix.
    pub project: String,
    /// Original remote URL returned by git.
    pub repo_url: String,
    /// Browser-openable repository URL derived from the remote.
    pub web_url: String,
}

impl ForgeRemote {
    /// Returns the `<namespace>/<project>` path used by forge CLIs and URLs.
    pub fn project_path(&self) -> String {
        format!("{}/{}", self.namespace, self.project)
    }
}

/// Input required to create a review request on one forge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateReviewRequestInput {
    /// Optional body/description content submitted with the review request.
    pub body: Option<String>,
    /// Source branch that should be reviewed.
    pub source_branch: String,
    /// Target branch that receives the review request.
    pub target_branch: String,
    /// Title shown in the forge review request UI.
    pub title: String,
}

/// Normalized state for one forge review request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewRequestState {
    /// The review request is still open.
    Open,
    /// The review request was merged.
    Merged,
    /// The review request was closed without merge.
    Closed,
}

/// Normalized review-request summary used by the app layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRequestSummary {
    /// Provider display id such as GitHub `#123` or GitLab `!42`.
    pub display_id: String,
    /// Forge family that owns the review request.
    pub forge_kind: ForgeKind,
    /// Source branch associated with the review request.
    pub source_branch: String,
    /// Normalized lifecycle state.
    pub state: ReviewRequestState,
    /// Compact provider-specific status text shown in the UI.
    pub status_summary: Option<String>,
    /// Target branch associated with the review request.
    pub target_branch: String,
    /// Review request title.
    pub title: String,
    /// Browser-openable review request URL.
    pub web_url: String,
}

/// Review-request failures normalized for actionable UI messaging.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewRequestError {
    /// The required forge CLI is not available on the user's machine.
    CliNotInstalled { forge_kind: ForgeKind },
    /// The forge CLI is installed but not authorized for the target host.
    AuthenticationRequired { forge_kind: ForgeKind, host: String },
    /// The repository remote does not map to a supported forge.
    UnsupportedRemote { repo_url: String },
    /// The adapter wiring exists but the provider-specific operation is not
    /// implemented yet.
    OperationNotImplemented {
        forge_kind: ForgeKind,
        operation: &'static str,
    },
    /// A forge CLI command ran but failed.
    OperationFailed {
        forge_kind: ForgeKind,
        message: String,
    },
}

impl ReviewRequestError {
    /// Returns actionable user-facing copy for the failure.
    pub fn detail_message(&self) -> String {
        match self {
            Self::CliNotInstalled { forge_kind } => format!(
                "{} review requests require the `{}` CLI.\nInstall `{}` and run `{}`, then retry.",
                forge_kind.display_name(),
                forge_kind.cli_name(),
                forge_kind.cli_name(),
                forge_kind.auth_login_command(),
            ),
            Self::AuthenticationRequired { forge_kind, host } => format!(
                "{} review requests require local CLI authentication for `{host}`.\nRun `{}` and \
                 retry.",
                forge_kind.display_name(),
                forge_kind.auth_login_command(),
            ),
            Self::UnsupportedRemote { repo_url } => format!(
                "Review requests are only supported for GitHub and GitLab remotes.\nThis \
                 repository remote is not supported: `{repo_url}`."
            ),
            Self::OperationNotImplemented {
                forge_kind,
                operation,
            } => format!(
                "{} review-request support for `{operation}` is not implemented yet.",
                forge_kind.display_name(),
            ),
            Self::OperationFailed {
                forge_kind,
                message,
            } => format!(
                "{} review-request operation failed: {message}",
                forge_kind.display_name()
            ),
        }
    }
}

/// Async boundary used by app orchestration for forge review requests.
///
/// The app layer depends on this one narrow contract so provider-specific
/// request formats remain isolated inside concrete adapters.
#[cfg_attr(test, mockall::automock)]
pub trait ReviewRequestClient: Send + Sync {
    /// Detects whether `repo_url` belongs to one supported forge.
    ///
    /// # Errors
    /// Returns [`ReviewRequestError::UnsupportedRemote`] when the remote does
    /// not map to GitHub or GitLab.
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

    /// Returns the browser-openable URL for one review request.
    ///
    /// # Errors
    /// Returns [`ReviewRequestError::OperationFailed`] when the summary does
    /// not carry a web URL.
    fn review_request_web_url(
        &self,
        review_request: &ReviewRequestSummary,
    ) -> Result<String, ReviewRequestError>;
}

/// Production [`ReviewRequestClient`] that routes to forge-specific adapters.
pub struct RealReviewRequestClient;

impl ReviewRequestClient for RealReviewRequestClient {
    fn detect_remote(&self, repo_url: String) -> Result<ForgeRemote, ReviewRequestError> {
        detect_remote(&repo_url)
    }

    fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>> {
        dispatch_by_forge(remote, move |adapter, remote| {
            adapter.find_by_source_branch(remote, source_branch)
        })
    }

    fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        dispatch_by_forge(remote, move |adapter, remote| {
            adapter.create_review_request(remote, input)
        })
    }

    fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        dispatch_by_forge(remote, move |adapter, remote| {
            adapter.refresh_review_request(remote, display_id)
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
}

/// Forge-specific adapter behavior hidden behind [`RealReviewRequestClient`].
trait ForgeAdapter {
    /// Finds an existing review request for `source_branch`.
    fn find_by_source_branch(
        &self,
        remote: ForgeRemote,
        source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>>;

    /// Creates a new review request from `input`.
    fn create_review_request(
        &self,
        remote: ForgeRemote,
        input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;

    /// Refreshes one existing review request by provider display id.
    fn refresh_review_request(
        &self,
        remote: ForgeRemote,
        display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>>;
}

/// GitHub pull-request adapter routed through the `gh` CLI.
struct GitHubReviewRequestAdapter;

impl ForgeAdapter for GitHubReviewRequestAdapter {
    fn find_by_source_branch(
        &self,
        _remote: ForgeRemote,
        _source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>> {
        Box::pin(async { Err(Self::not_implemented("find by source branch")) })
    }

    fn create_review_request(
        &self,
        _remote: ForgeRemote,
        _input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        Box::pin(async { Err(Self::not_implemented("create review request")) })
    }

    fn refresh_review_request(
        &self,
        _remote: ForgeRemote,
        _display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        Box::pin(async { Err(Self::not_implemented("refresh review request")) })
    }
}

impl GitHubReviewRequestAdapter {
    /// Returns normalized GitHub remote metadata when `repo_url` is supported.
    fn detect_remote(repo_url: &str) -> Option<ForgeRemote> {
        let parsed_remote = parse_remote_url(repo_url)?;
        if strip_port(&parsed_remote.host) != "github.com" {
            return None;
        }

        Some(parsed_remote.into_forge_remote(ForgeKind::GitHub))
    }

    /// Returns the staged not-implemented error for one GitHub operation.
    fn not_implemented(operation: &'static str) -> ReviewRequestError {
        ReviewRequestError::OperationNotImplemented {
            forge_kind: ForgeKind::GitHub,
            operation,
        }
    }
}

/// GitLab merge-request adapter routed through the `glab` CLI.
struct GitLabReviewRequestAdapter;

impl ForgeAdapter for GitLabReviewRequestAdapter {
    fn find_by_source_branch(
        &self,
        _remote: ForgeRemote,
        _source_branch: String,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>> {
        Box::pin(async { Err(Self::not_implemented("find by source branch")) })
    }

    fn create_review_request(
        &self,
        _remote: ForgeRemote,
        _input: CreateReviewRequestInput,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        Box::pin(async { Err(Self::not_implemented("create review request")) })
    }

    fn refresh_review_request(
        &self,
        _remote: ForgeRemote,
        _display_id: String,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        Box::pin(async { Err(Self::not_implemented("refresh review request")) })
    }
}

impl GitLabReviewRequestAdapter {
    /// Returns normalized GitLab remote metadata when `repo_url` is supported.
    fn detect_remote(repo_url: &str) -> Option<ForgeRemote> {
        let parsed_remote = parse_remote_url(repo_url)?;
        if !parsed_remote.host_is_gitlab() {
            return None;
        }

        Some(parsed_remote.into_forge_remote(ForgeKind::GitLab))
    }

    /// Returns the staged not-implemented error for one GitLab operation.
    fn not_implemented(operation: &'static str) -> ReviewRequestError {
        ReviewRequestError::OperationNotImplemented {
            forge_kind: ForgeKind::GitLab,
            operation,
        }
    }
}

/// Parsed remote components extracted from one git remote URL.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedRemote {
    host: String,
    namespace: String,
    project: String,
    repo_url: String,
    web_url: String,
}

impl ParsedRemote {
    /// Converts the parsed remote into one supported forge remote.
    fn into_forge_remote(self, forge_kind: ForgeKind) -> ForgeRemote {
        ForgeRemote {
            forge_kind,
            host: self.host,
            namespace: self.namespace,
            project: self.project,
            repo_url: self.repo_url,
            web_url: self.web_url,
        }
    }

    /// Returns whether the hostname clearly identifies a GitLab instance.
    fn host_is_gitlab(&self) -> bool {
        strip_port(&self.host)
            .split('.')
            .any(|segment| segment == "gitlab")
    }
}

/// Detects one supported forge remote from `repo_url`.
fn detect_remote(repo_url: &str) -> Result<ForgeRemote, ReviewRequestError> {
    if let Some(remote) = GitHubReviewRequestAdapter::detect_remote(repo_url) {
        return Ok(remote);
    }

    if let Some(remote) = GitLabReviewRequestAdapter::detect_remote(repo_url) {
        return Ok(remote);
    }

    Err(ReviewRequestError::UnsupportedRemote {
        repo_url: repo_url.to_string(),
    })
}

/// Routes one operation to the concrete adapter for `remote`.
fn dispatch_by_forge<T, Operation>(remote: ForgeRemote, operation: Operation) -> ForgeFuture<T>
where
    Operation: FnOnce(&dyn ForgeAdapter, ForgeRemote) -> ForgeFuture<T>,
{
    let github_adapter = GitHubReviewRequestAdapter;
    let gitlab_adapter = GitLabReviewRequestAdapter;

    match remote.forge_kind {
        ForgeKind::GitHub => operation(&github_adapter, remote),
        ForgeKind::GitLab => operation(&gitlab_adapter, remote),
    }
}

/// Parses a git remote URL into normalized hostname and repository components.
fn parse_remote_url(repo_url: &str) -> Option<ParsedRemote> {
    let trimmed_url = repo_url.trim().trim_end_matches('/');
    if trimmed_url.is_empty() {
        return None;
    }

    if let Some(ssh_remote) = trimmed_url.strip_prefix("git@") {
        let (host, path) = ssh_remote.split_once(':')?;

        return parsed_remote_from_parts(trimmed_url, host, path);
    }

    let (_, scheme_rest) = trimmed_url.split_once("://")?;
    let scheme_rest = scheme_rest.strip_prefix("git@").unwrap_or(scheme_rest);
    let (host, path) = scheme_rest.split_once('/')?;

    parsed_remote_from_parts(trimmed_url, host, path)
}

/// Builds a parsed remote from extracted host and path components.
fn parsed_remote_from_parts(repo_url: &str, host: &str, path: &str) -> Option<ParsedRemote> {
    let host = host.trim().trim_matches('/').to_ascii_lowercase();
    let path = path.trim().trim_matches('/').trim_end_matches(".git");
    if host.is_empty() || path.is_empty() {
        return None;
    }

    let (namespace, project) = path.rsplit_once('/')?;
    if namespace.is_empty() || project.is_empty() {
        return None;
    }

    Some(ParsedRemote {
        host: host.clone(),
        namespace: namespace.to_string(),
        project: project.to_string(),
        repo_url: repo_url.to_string(),
        web_url: format!("https://{host}/{path}"),
    })
}

/// Removes any `:port` suffix from `host`.
fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_remote_returns_github_remote_for_https_origin() {
        // Arrange
        let client = RealReviewRequestClient;

        // Act
        let remote = client
            .detect_remote("https://github.com/agentty-xyz/agentty.git".to_string())
            .expect("github remote should be supported");

        // Assert
        assert_eq!(
            remote,
            ForgeRemote {
                forge_kind: ForgeKind::GitHub,
                host: "github.com".to_string(),
                namespace: "agentty-xyz".to_string(),
                project: "agentty".to_string(),
                repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty".to_string(),
            }
        );
    }

    #[test]
    fn detect_remote_returns_github_remote_for_ssh_origin() {
        // Arrange
        let client = RealReviewRequestClient;

        // Act
        let remote = client
            .detect_remote("git@github.com:agentty-xyz/agentty.git".to_string())
            .expect("github ssh remote should be supported");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitHub);
        assert_eq!(remote.web_url, "https://github.com/agentty-xyz/agentty");
        assert_eq!(remote.project_path(), "agentty-xyz/agentty");
    }

    #[test]
    fn detect_remote_returns_gitlab_remote_for_https_origin() {
        // Arrange
        let client = RealReviewRequestClient;

        // Act
        let remote = client
            .detect_remote("https://gitlab.com/group/subgroup/project.git".to_string())
            .expect("gitlab remote should be supported");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitLab);
        assert_eq!(remote.host, "gitlab.com");
        assert_eq!(remote.namespace, "group/subgroup");
        assert_eq!(remote.project, "project");
    }

    #[test]
    fn detect_remote_returns_gitlab_remote_for_self_hosted_ssh_origin() {
        // Arrange
        let client = RealReviewRequestClient;

        // Act
        let remote = client
            .detect_remote("git@gitlab.example.com:team/project.git".to_string())
            .expect("self-hosted gitlab remote should be supported");

        // Assert
        assert_eq!(remote.forge_kind, ForgeKind::GitLab);
        assert_eq!(remote.host, "gitlab.example.com");
        assert_eq!(remote.web_url, "https://gitlab.example.com/team/project");
    }

    #[test]
    fn detect_remote_returns_unsupported_remote_error_for_non_forge_origin() {
        // Arrange
        let client = RealReviewRequestClient;

        // Act
        let error = client
            .detect_remote("https://example.com/team/project.git".to_string())
            .expect_err("non-forge remote should be rejected");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::UnsupportedRemote {
                repo_url: "https://example.com/team/project.git".to_string(),
            }
        );
        assert!(error.detail_message().contains("GitHub and GitLab"));
        assert!(error.detail_message().contains("example.com"));
    }

    #[tokio::test]
    async fn create_review_request_routes_to_github_adapter_stub() {
        // Arrange
        let client = RealReviewRequestClient;
        let remote = ForgeRemote {
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        };
        let input = CreateReviewRequestInput {
            body: None,
            source_branch: "feature/forge".to_string(),
            target_branch: "main".to_string(),
            title: "Add forge boundary".to_string(),
        };

        // Act
        let error = client
            .create_review_request(remote, input)
            .await
            .expect_err("github adapter should return a staged stub error");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationNotImplemented {
                forge_kind: ForgeKind::GitHub,
                operation: "create review request",
            }
        );
    }

    #[test]
    fn review_request_web_url_returns_error_when_summary_is_missing_url() {
        // Arrange
        let client = RealReviewRequestClient;
        let review_request = ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Checks pending".to_string()),
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
}
