//! Shared forge review-request types.

use std::future::Future;
use std::pin::Pin;

/// Shared forge family enum reused by persistence and forge adapters.
pub use crate::domain::session::ForgeKind;
/// Shared review-request remote state reused by persistence and forge adapters.
pub use crate::domain::session::ReviewRequestState;
/// Shared normalized review-request summary reused by persistence and adapters.
pub use crate::domain::session::ReviewRequestSummary;

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
            } => authentication_required_message(forge_kind, host, detail.as_deref()),
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
    forge_kind: &ForgeKind,
    host: &str,
    detail: Option<&str>,
) -> String {
    let mut message = format!(
        "{} review requests require local CLI authentication for `{host}`.\nRun `{}` and retry.",
        forge_kind.display_name(),
        forge_kind.auth_login_command(),
    );

    if let Some(detail) = non_empty_detail(detail) {
        message.push_str(&format!(
            "\n\nOriginal `{}` error:\n```text\n{detail}",
            forge_kind.cli_name(),
        ));
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
