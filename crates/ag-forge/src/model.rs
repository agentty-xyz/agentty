//! Shared forge review-request types.

use std::fmt;
use std::fmt::Write as _;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;

use url::Url;

/// Shared forge family enum reused by persistence and forge adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeKind {
    /// GitHub-hosted pull requests.
    GitHub,
    /// GitLab-hosted merge requests.
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

    /// Returns the persisted string representation for this forge kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
        }
    }

    /// Returns the forge-native review-request noun shown in user-facing copy.
    pub fn review_request_name(self) -> &'static str {
        match self {
            Self::GitHub => "pull request",
            Self::GitLab => "merge request",
        }
    }

    /// Returns the combined forge and review-request name for user-facing copy.
    pub fn review_request_display_name(self) -> String {
        format!("{} {}", self.display_name(), self.review_request_name())
    }

    /// Returns the short UI indicator label for one review request.
    pub fn review_request_short_name(self) -> &'static str {
        match self {
            Self::GitHub => "PR",
            Self::GitLab => "MR",
        }
    }

    /// Returns whether Agentty can fetch inline review-thread comments for
    /// this forge.
    ///
    /// GitHub pull-request and GitLab merge-request adapters both expose
    /// enough line-position data for the read-only comments preview.
    pub fn supports_review_comments_preview(self) -> bool {
        match self {
            Self::GitHub | Self::GitLab => true,
        }
    }
}

/// Returns whether `host` looks like one GitLab instance hostname.
pub fn is_gitlab_host(host: &str) -> bool {
    host == "gitlab.com"
        || host.ends_with(".gitlab.com")
        || host.starts_with("gitlab.")
        || host.contains(".gitlab.")
}

impl fmt::Display for ForgeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ForgeKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "GitHub" => Ok(Self::GitHub),
            "GitLab" => Ok(Self::GitLab),
            _ => Err(format!("Unknown review-request forge: {value}")),
        }
    }
}

/// Normalized remote lifecycle state for one linked review request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewRequestState {
    /// The linked review request is still open.
    Open,
    /// The linked review request was merged upstream.
    Merged,
    /// The linked review request was closed without merge.
    Closed,
}

impl ReviewRequestState {
    /// Returns the persisted string representation for this remote state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Merged => "Merged",
            Self::Closed => "Closed",
        }
    }
}

impl fmt::Display for ReviewRequestState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReviewRequestState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Open" => Ok(Self::Open),
            "Merged" => Ok(Self::Merged),
            "Closed" => Ok(Self::Closed),
            _ => Err(format!("Unknown review-request state: {value}")),
        }
    }
}

/// Normalized remote summary for one linked review request.
///
/// Local session lifecycle transitions such as `Rebasing`, `Done`, and
/// `Canceled` retain this metadata so the session can continue to reference the
/// same remote review request. Remote terminal outcomes are stored in
/// `state` instead of clearing the link; only an explicit unlink action or
/// session deletion should remove this metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRequestSummary {
    /// Provider display id such as GitHub `#123`.
    pub display_id: String,
    /// Forge family that owns the linked review request.
    pub forge_kind: ForgeKind,
    /// Source branch published for review.
    pub source_branch: String,
    /// Latest normalized remote lifecycle state.
    pub state: ReviewRequestState,
    /// Provider-specific condensed status text for UI display.
    pub status_summary: Option<String>,
    /// Target branch receiving the review request.
    pub target_branch: String,
    /// Remote review-request title.
    pub title: String,
    /// Browser-openable review-request URL.
    pub web_url: String,
}

/// Boxed async result used by review-request trait methods.
pub type ForgeFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Normalized repository remote metadata for one supported forge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeRemote {
    /// Repository worktree used when forge CLI commands need local git
    /// context.
    pub command_working_directory: Option<PathBuf>,
    /// Forge family inferred from the repository remote.
    pub forge_kind: ForgeKind,
    /// Forge hostname used for browser and API calls.
    ///
    /// HTTPS remotes keep any explicit web/API port, while SSH transport ports
    /// are stripped during remote normalization.
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
    /// Returns one remote copy that runs forge CLI commands from
    /// `working_directory`.
    #[must_use]
    pub fn with_command_working_directory(mut self, working_directory: PathBuf) -> Self {
        self.command_working_directory = Some(working_directory);

        self
    }

    /// Returns the `<namespace>/<project>` path used by forge CLIs and URLs.
    pub fn project_path(&self) -> String {
        format!("{}/{}", self.namespace, self.project)
    }

    /// Returns the browser-openable URL that starts one new pull request or
    /// review request for `source_branch` into `target_branch`.
    ///
    /// # Errors
    /// Returns [`ReviewRequestError::OperationFailed`] when the stored
    /// repository web URL is invalid or cannot be converted into a forge
    /// review-request creation URL.
    pub fn review_request_creation_url(
        &self,
        source_branch: &str,
        target_branch: &str,
    ) -> Result<String, ReviewRequestError> {
        match self.forge_kind {
            ForgeKind::GitHub => {
                github_review_request_creation_url(self, source_branch, target_branch)
            }
            ForgeKind::GitLab => {
                gitlab_review_request_creation_url(self, source_branch, target_branch)
            }
        }
    }
}

/// One inline review comment emitted by a reviewer on a forge review thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewComment {
    /// Reviewer login or display name.
    pub author: String,
    /// Markdown body as authored by the reviewer.
    pub body: String,
}

/// Diff side used to anchor one inline review-thread comment.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReviewCommentAnchorSide {
    /// A file-level thread that is not attached to a specific diff line.
    File,
    /// A thread anchored to the new/right side of the diff.
    New,
    /// A thread anchored to the old/left side of the diff.
    Old,
}

/// One review thread anchored to a line of the review request diff.
///
/// Threads group chronological `comments` that share the same anchor. v1 of
/// Agentty's comments preview renders these read-only, grouped by file and
/// sorted by `(path, line)` before display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCommentThread {
    /// Diff side used with `line` when placing this thread inline.
    pub anchor_side: ReviewCommentAnchorSide,
    /// Chronological reviewer comments attached to this thread.
    pub comments: Vec<ReviewComment>,
    /// Whether newer changes made the thread's original diff position stale,
    /// when the forge exposes that state.
    pub is_outdated: Option<bool>,
    /// Whether the thread has been marked resolved on the forge.
    pub is_resolved: bool,
    /// Anchor line number on `anchor_side`, when the forge exposes one.
    pub line: Option<u32>,
    /// File path the thread is anchored to, relative to the repository root.
    pub path: String,
    /// Optional first line for a multi-line thread on `anchor_side`.
    pub start_line: Option<u32>,
}

/// Full review-comments payload captured for one review request.
///
/// Separates forge-native `threads` (anchored to a file + line) from
/// `pr_level_comments` (review-request-wide discussion comments that do not
/// anchor to the diff). The UI renders the two categories side-by-side with a
/// synthetic "General discussion" entry on top of the comments file tree.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReviewCommentSnapshot {
    /// Chronological review-request-wide comments that do not anchor to a file
    /// or line.
    pub pr_level_comments: Vec<ReviewComment>,
    /// Inline threads grouped by the file and line they are anchored to.
    pub threads: Vec<ReviewCommentThread>,
}

/// Input required to create a review request on one forge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateReviewRequestInput {
    /// Optional body or description submitted with the review request.
    pub body: Option<String>,
    /// Source branch that should be reviewed.
    pub source_branch: String,
    /// Target branch that receives the review request.
    pub target_branch: String,
    /// Title shown in the forge review-request UI.
    pub title: String,
}

/// Review-request failures normalized for actionable UI messaging.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewRequestError {
    /// The required forge CLI is not available on the user's machine.
    CliNotInstalled { forge_kind: ForgeKind },
    /// The forge CLI is installed but not authorized for the target host.
    AuthenticationRequired {
        /// Forge family that reported the authentication failure.
        forge_kind: ForgeKind,
        /// Forge host the CLI attempted to access.
        host: String,
        /// Original CLI error detail captured from stdout or stderr.
        detail: Option<String>,
    },
    /// The forge host from the repository remote could not be resolved.
    HostResolutionFailed { forge_kind: ForgeKind, host: String },
    /// The repository remote does not map to a supported forge.
    UnsupportedRemote { repo_url: String },
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
            Self::AuthenticationRequired {
                forge_kind,
                host,
                detail,
            } => authentication_required_message(*forge_kind, host, detail.as_deref()),
            Self::HostResolutionFailed { forge_kind, host } => format!(
                "{} review requests could not reach `{host}`.\nCheck the repository remote host \
                 and your network or DNS setup, then retry.",
                forge_kind.display_name(),
            ),
            Self::UnsupportedRemote { repo_url } => format!(
                "Review requests are only supported for GitHub and GitLab remotes.\nThis \
                 repository remote is not supported: `{repo_url}`."
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

/// Returns actionable copy for one CLI authentication failure and preserves
/// the original CLI output when it is available.
fn authentication_required_message(
    forge_kind: ForgeKind,
    host: &str,
    detail: Option<&str>,
) -> String {
    let mut message = format!(
        "{} review requests require local CLI authentication for `{host}`.\nRun `{}` and retry.",
        forge_kind.display_name(),
        forge_kind.auth_login_command(),
    );

    if let Some(detail) = non_empty_detail(detail) {
        // Infallible: writing to a String cannot fail.
        let _ = write!(
            message,
            "\n\nOriginal `{}` error:\n```text\n{detail}",
            forge_kind.cli_name(),
        );
        if !detail.ends_with('\n') {
            message.push('\n');
        }
        message.push_str("```");
    }

    message
}

/// Returns one trimmed CLI error detail when the captured output is not empty.
fn non_empty_detail(detail: Option<&str>) -> Option<&str> {
    detail.and_then(|detail| {
        let trimmed_detail = detail.trim();
        (!trimmed_detail.is_empty()).then_some(trimmed_detail)
    })
}

/// Builds one GitHub compare URL that opens the new pull-request flow.
fn github_review_request_creation_url(
    remote: &ForgeRemote,
    source_branch: &str,
    target_branch: &str,
) -> Result<String, ReviewRequestError> {
    let mut url = parsed_remote_web_url(remote)?;
    let compare_target = if target_branch.trim().is_empty() {
        source_branch.to_string()
    } else {
        format!("{target_branch}...{source_branch}")
    };

    {
        let mut path_segments = url
            .path_segments_mut()
            .map_err(|()| invalid_web_url_error(remote))?;
        path_segments.pop_if_empty();
        path_segments.push("compare");
        path_segments.push(&compare_target);
    }

    url.query_pairs_mut().append_pair("expand", "1");

    Ok(url.into())
}

/// Builds one GitLab URL that opens the new merge-request flow.
fn gitlab_review_request_creation_url(
    remote: &ForgeRemote,
    source_branch: &str,
    target_branch: &str,
) -> Result<String, ReviewRequestError> {
    let mut url = parsed_remote_web_url(remote)?;

    {
        let mut path_segments = url
            .path_segments_mut()
            .map_err(|()| invalid_web_url_error(remote))?;
        path_segments.pop_if_empty();
        path_segments.push("-");
        path_segments.push("merge_requests");
        path_segments.push("new");
    }

    url.query_pairs_mut()
        .append_pair("merge_request[source_branch]", source_branch)
        .append_pair("merge_request[target_branch]", target_branch);

    Ok(url.into())
}

/// Parses the stored repository web URL for one forge remote.
fn parsed_remote_web_url(remote: &ForgeRemote) -> Result<Url, ReviewRequestError> {
    Url::parse(&remote.web_url).map_err(|_| invalid_web_url_error(remote))
}

/// Returns one normalized invalid-remote-url error for review-request links.
fn invalid_web_url_error(remote: &ForgeRemote) -> ReviewRequestError {
    ReviewRequestError::OperationFailed {
        forge_kind: remote.forge_kind,
        message: format!(
            "repository remote is missing a valid web URL: `{}`",
            remote.web_url
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_required_message_includes_original_cli_error_detail() {
        // Arrange
        let error = ReviewRequestError::AuthenticationRequired {
            detail: Some("HTTP 401 Unauthorized. Run `gh auth login`.".to_string()),
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
        };

        // Act
        let message = error.detail_message();

        // Assert
        assert!(message.contains("GitHub review requests require local CLI authentication"));
        assert!(message.contains("Run `gh auth login` and retry."));
        assert!(message.contains("Original `gh` error:"));
        assert!(message.contains("HTTP 401 Unauthorized. Run `gh auth login`."));
        assert!(message.contains("```text"));
    }

    #[test]
    fn authentication_required_message_omits_empty_original_cli_error_detail() {
        // Arrange
        let error = ReviewRequestError::AuthenticationRequired {
            detail: Some("   \n".to_string()),
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
        };

        // Act
        let message = error.detail_message();

        // Assert
        assert!(message.contains("Run `gh auth login` and retry."));
        assert!(!message.contains("Original `gh` error:"));
    }

    #[test]
    fn review_request_creation_url_returns_github_compare_link() {
        // Arrange
        let remote = ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "git@github.com:agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        };

        // Act
        let url = remote
            .review_request_creation_url("review/custom-branch", "main")
            .expect("github compare URL should be created");

        // Assert
        assert_eq!(
            url,
            "https://github.com/agentty-xyz/agentty/compare/main...review%2Fcustom-branch?expand=1"
        );
    }

    #[test]
    fn review_request_creation_url_rejects_invalid_web_url() {
        // Arrange
        let remote = ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "git@github.com:agentty-xyz/agentty.git".to_string(),
            web_url: "not a url".to_string(),
        };

        // Act
        let error = remote
            .review_request_creation_url("review/custom-branch", "main")
            .expect_err("invalid web URL should be rejected");

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "repository remote is missing a valid web URL: `not a url`".to_string(),
            }
        );
    }

    #[test]
    fn forge_kind_from_str_gitlab() {
        // Arrange
        let raw_forge_kind = "GitLab";

        // Act
        let forge_kind = raw_forge_kind
            .parse::<ForgeKind>()
            .expect("gitlab forge kind should parse");

        // Assert
        assert_eq!(forge_kind, ForgeKind::GitLab);
        assert_eq!(forge_kind.cli_name(), "glab");
        assert_eq!(forge_kind.review_request_name(), "merge request");
        assert_eq!(forge_kind.review_request_short_name(), "MR");
    }

    #[test]
    fn supports_review_comments_preview_returns_true_for_supported_forges() {
        // Arrange / Act / Assert
        assert!(ForgeKind::GitHub.supports_review_comments_preview());
        assert!(ForgeKind::GitLab.supports_review_comments_preview());
    }

    #[test]
    fn review_request_creation_url_returns_gitlab_merge_request_link() {
        // Arrange
        let remote = ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "git@gitlab.com:agentty-xyz/agentty.git".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
        };

        // Act
        let url = remote
            .review_request_creation_url("review/custom-branch", "main")
            .expect("gitlab merge-request URL should be created");

        // Assert
        assert_eq!(
            url,
            "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/new?merge_request%5Bsource_branch%5D=review%2Fcustom-branch&merge_request%5Btarget_branch%5D=main"
        );
    }
}
