//! Shared forge review-request types.

use std::fmt;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;

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
    /// Provider display id such as GitHub `#123` or GitLab `!42`.
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
    /// Returns the `<namespace>/<project>` path used by forge CLIs and URLs.
    pub fn project_path(&self) -> String {
        format!("{}/{}", self.namespace, self.project)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_required_message_includes_original_cli_error_detail() {
        // Arrange
        let error = ReviewRequestError::AuthenticationRequired {
            detail: Some("HTTP 401 Unauthorized. Run `glab auth login`.".to_string()),
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.example.com".to_string(),
        };

        // Act
        let message = error.detail_message();

        // Assert
        assert!(message.contains("GitLab review requests require local CLI authentication"));
        assert!(message.contains("Run `glab auth login` and retry."));
        assert!(message.contains("Original `glab` error:"));
        assert!(message.contains("HTTP 401 Unauthorized. Run `glab auth login`."));
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
}
