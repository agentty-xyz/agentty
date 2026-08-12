//! Shared helpers used by forge review-request adapters.
//!
//! Each supported forge (GitHub, GitLab) needs the same normalization for
//! authentication failures, host-resolution failures, status-summary joining,
//! provider label casing, and spawn-time error mapping. Keeping these in one
//! module avoids divergence between adapters.

use std::sync::Arc;

use super::{
    ForgeCommand, ForgeCommandError, ForgeCommandOutput, ForgeCommandRunner, ForgeFuture,
    ForgeKind, ForgeRemote, ReviewCommentSnapshot, ReviewRequestError, ReviewRequestMetadata,
    ReviewRequestSummary, UpdateReviewRequestInput, command_output_detail,
};

/// Provider-neutral partial edit produced after a best-effort recheck that the
/// remote fields still match the values used during semantic reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestMetadataEdit {
    pub(crate) body: Option<String>,
    pub(crate) title: Option<String>,
}

/// Best-effort conditional edit calculated from the last observed metadata.
struct ReviewRequestMetadataPlan {
    edit: ReviewRequestMetadataEdit,
}

impl ReviewRequestMetadataPlan {
    /// Builds a field-level edit when the last observed remote value matches
    /// the value used by the semantic reconciliation step.
    fn new(metadata: &ReviewRequestMetadata, input: &UpdateReviewRequestInput) -> Self {
        let body = input.body.as_ref().and_then(|field| {
            (metadata.body == field.current && metadata.body != field.desired)
                .then(|| field.desired.clone())
        });
        let title = input.title.as_ref().and_then(|field| {
            (metadata.title == field.current && metadata.title != field.desired)
                .then(|| field.desired.clone())
        });

        Self {
            edit: ReviewRequestMetadataEdit { body, title },
        }
    }

    /// Returns whether at least one reconciled field needs a remote edit.
    fn requires_edit(&self) -> bool {
        self.edit.body.is_some() || self.edit.title.is_some()
    }
}

/// Shared command runner and operation flow used by forge adapters.
#[derive(Clone)]
pub(crate) struct ReviewRequestOperations {
    command_runner: Arc<dyn ForgeCommandRunner>,
}

impl ReviewRequestOperations {
    /// Builds shared review-request operations from one command runner.
    pub(crate) fn new(command_runner: Arc<dyn ForgeCommandRunner>) -> Self {
        Self { command_runner }
    }

    /// Verifies CLI authentication and normalizes common auth failures.
    pub(crate) async fn ensure_authenticated(
        &self,
        remote: &ForgeRemote,
        command: ForgeCommand,
    ) -> Result<(), ReviewRequestError> {
        let output = self
            .command_runner
            .run(command)
            .await
            .map_err(|error| Self::map_spawn_error(remote, error))?;
        if output.success() {
            return Ok(());
        }

        let detail = command_output_detail(&output);
        if Self::looks_like_host_resolution_failure(&detail) {
            return Err(ReviewRequestError::HostResolutionFailed {
                forge_kind: remote.forge_kind,
                host: remote.host.clone(),
            });
        }

        Err(ReviewRequestError::AuthenticationRequired {
            detail: Some(detail),
            forge_kind: remote.forge_kind,
            host: remote.host.clone(),
        })
    }

    /// Builds an authentication future for adapter trait implementations.
    pub(crate) fn ensure_authenticated_future(
        &self,
        remote: ForgeRemote,
        auth_status_command: fn(&ForgeRemote) -> ForgeCommand,
    ) -> ForgeFuture<Result<(), ReviewRequestError>> {
        let operations = self.clone();

        Box::pin(async move {
            operations
                .ensure_authenticated(&remote, auth_status_command(&remote))
                .await
        })
    }

    /// Runs one authenticated forge CLI command and normalizes common
    /// failures.
    pub(crate) async fn run_review_command(
        &self,
        remote: &ForgeRemote,
        command: ForgeCommand,
        operation: &str,
    ) -> Result<ForgeCommandOutput, ReviewRequestError> {
        let output = self
            .command_runner
            .run(command)
            .await
            .map_err(|error| Self::map_spawn_error(remote, error))?;
        if output.success() {
            return Ok(output);
        }

        let detail = command_output_detail(&output);
        if Self::looks_like_host_resolution_failure(&detail) {
            return Err(ReviewRequestError::HostResolutionFailed {
                forge_kind: remote.forge_kind,
                host: remote.host.clone(),
            });
        }

        if Self::looks_like_authentication_failure(&detail, remote.forge_kind) {
            return Err(ReviewRequestError::AuthenticationRequired {
                detail: Some(detail),
                forge_kind: remote.forge_kind,
                host: remote.host.clone(),
            });
        }

        Err(operation_failed(
            remote.forge_kind,
            format!("{operation}: {detail}"),
        ))
    }

    /// Finds one review request by source branch, then builds an owned future
    /// that refreshes its full summary for adapter trait implementations.
    pub(crate) fn find_by_source_branch_future(
        &self,
        remote: ForgeRemote,
        source_branch: String,
        lookup_command: fn(&ForgeRemote, &str) -> ForgeCommand,
        operation: &'static str,
        parse_lookup_display_id: fn(&str) -> Result<Option<String>, String>,
        refresh_review_request: impl FnOnce(
            ForgeRemote,
            String,
        ) -> ForgeFuture<
            Result<ReviewRequestSummary, ReviewRequestError>,
        > + Send
        + 'static,
    ) -> ForgeFuture<Result<Option<ReviewRequestSummary>, ReviewRequestError>> {
        let operations = self.clone();

        Box::pin(async move {
            let lookup_command = lookup_command(&remote, &source_branch);
            let output = operations
                .run_review_command(&remote, lookup_command, operation)
                .await?;
            let display_id =
                map_parse_error(remote.forge_kind, parse_lookup_display_id(&output.stdout))?;

            let Some(display_id) = display_id else {
                return Ok(None);
            };

            refresh_review_request(remote, display_id).await.map(Some)
        })
    }

    /// Refreshes one review request by provider display id in an owned future
    /// for adapter trait implementations.
    pub(crate) fn refresh_review_request_future(
        &self,
        remote: ForgeRemote,
        display_id: String,
        parse_display_id: fn(&str) -> Result<String, ReviewRequestError>,
        view_command: fn(&ForgeRemote, &str) -> ForgeCommand,
        operation: &'static str,
        parse_view_response: fn(&str) -> Result<ReviewRequestSummary, String>,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        let operations = self.clone();

        Box::pin(async move {
            let command_display_id = parse_display_id(&display_id)?;
            let output = operations
                .run_review_command(
                    &remote,
                    view_command(&remote, &command_display_id),
                    operation,
                )
                .await?;

            map_parse_error(remote.forge_kind, parse_view_response(&output.stdout))
        })
    }

    /// Fetches current review-request metadata in an owned future for adapter
    /// trait implementations.
    pub(crate) fn review_request_metadata_future(
        &self,
        remote: ForgeRemote,
        display_id: String,
        config: &SyncReviewRequestMetadataConfig,
    ) -> ForgeFuture<Result<ReviewRequestMetadata, ReviewRequestError>> {
        let parse_display_id = config.parse_display_id;
        let parse_metadata_response = config.parse_metadata_response;
        let view_metadata_command = config.view_metadata_command;
        let view_operation = config.view_operation;
        let operations = self.clone();

        Box::pin(async move {
            let command_display_id = parse_display_id(&display_id)?;
            let output = operations
                .run_review_command(
                    &remote,
                    view_metadata_command(&remote, &command_display_id),
                    view_operation,
                )
                .await?;

            map_parse_error(remote.forge_kind, parse_metadata_response(&output.stdout))
        })
    }

    /// Rechecks reconciled fields immediately before a best-effort metadata
    /// update, then refreshes the full summary in an owned future.
    ///
    /// Provider CLI updates have no atomic version precondition, so a manual
    /// edit made after this recheck can still be overwritten.
    pub(crate) fn sync_review_request_metadata_future(
        &self,
        remote: ForgeRemote,
        display_id: String,
        input: UpdateReviewRequestInput,
        config: &SyncReviewRequestMetadataConfig,
        refresh_review_request: impl FnOnce(
            ForgeRemote,
            String,
        ) -> ForgeFuture<
            Result<ReviewRequestSummary, ReviewRequestError>,
        > + Send
        + 'static,
    ) -> ForgeFuture<Result<ReviewRequestSummary, ReviewRequestError>> {
        let edit_metadata_command = config.edit_metadata_command;
        let edit_operation = config.edit_operation;
        let parse_display_id = config.parse_display_id;
        let parse_metadata_response = config.parse_metadata_response;
        let view_metadata_command = config.view_metadata_command;
        let view_operation = config.view_operation;
        let operations = self.clone();

        Box::pin(async move {
            let command_display_id = parse_display_id(&display_id)?;
            let output = operations
                .run_review_command(
                    &remote,
                    view_metadata_command(&remote, &command_display_id),
                    view_operation,
                )
                .await?;
            let metadata =
                map_parse_error(remote.forge_kind, parse_metadata_response(&output.stdout))?;
            let plan = ReviewRequestMetadataPlan::new(&metadata, &input);

            if plan.requires_edit() {
                operations
                    .run_review_command(
                        &remote,
                        edit_metadata_command(&remote, &command_display_id, &plan.edit),
                        edit_operation,
                    )
                    .await?;
            }

            refresh_review_request(remote, display_id).await
        })
    }

    /// Fetches and parses one review-comment snapshot in an owned future for
    /// adapter trait implementations.
    pub(crate) fn fetch_review_comment_snapshot_future(
        &self,
        remote: ForgeRemote,
        display_id: String,
        parse_display_id: fn(&str) -> Result<String, ReviewRequestError>,
        snapshot_command: fn(&ForgeRemote, &str) -> ForgeCommand,
        operation: &'static str,
        parse_snapshot_response: fn(&str) -> Result<ReviewCommentSnapshot, String>,
    ) -> ForgeFuture<Result<ReviewCommentSnapshot, ReviewRequestError>> {
        let operations = self.clone();

        Box::pin(async move {
            let command_display_id = parse_display_id(&display_id)?;
            let output = operations
                .run_review_command(
                    &remote,
                    snapshot_command(&remote, &command_display_id),
                    operation,
                )
                .await?;

            map_parse_error(remote.forge_kind, parse_snapshot_response(&output.stdout))
        })
    }

    /// Maps one spawn-time failure into a normalized review-request error for
    /// the forge owning `remote`.
    fn map_spawn_error(remote: &ForgeRemote, error: ForgeCommandError) -> ReviewRequestError {
        let forge_kind = remote.forge_kind;

        match error {
            ForgeCommandError::ExecutableNotFound { .. } => {
                ReviewRequestError::CliNotInstalled { forge_kind }
            }
            ForgeCommandError::SpawnFailed { message, .. } => {
                if Self::looks_like_host_resolution_failure(&message) {
                    return ReviewRequestError::HostResolutionFailed {
                        forge_kind,
                        host: remote.host.clone(),
                    };
                }

                ReviewRequestError::OperationFailed {
                    forge_kind,
                    message: format!("failed to execute `{}`: {message}", forge_kind.cli_name()),
                }
            }
            ForgeCommandError::TimedOut {
                executable,
                timeout,
            } => ReviewRequestError::OperationFailed {
                forge_kind,
                message: format!(
                    "`{executable}` timed out after {} seconds while contacting {}",
                    timeout.as_secs(),
                    remote.host
                ),
            },
        }
    }

    /// Returns whether `detail` looks like a DNS or host-resolution failure.
    fn looks_like_host_resolution_failure(detail: &str) -> bool {
        let normalized_detail = detail.to_ascii_lowercase();

        normalized_detail.contains("no such host")
            || normalized_detail.contains("name or service not known")
            || normalized_detail.contains("temporary failure in name resolution")
            || normalized_detail.contains("could not resolve host")
            || normalized_detail.contains("lookup ")
    }

    /// Returns whether `detail` looks like a forge CLI authentication failure.
    ///
    /// Parameterized on `forge_kind` so the CLI-specific `{cli} auth login`
    /// marker stays accurate across forges while the remaining substrings are
    /// shared.
    fn looks_like_authentication_failure(detail: &str, forge_kind: ForgeKind) -> bool {
        let normalized_detail = detail.to_ascii_lowercase();
        let auth_login_marker = format!("{} auth login", forge_kind.cli_name());

        normalized_detail.contains(&auth_login_marker)
            || normalized_detail.contains("not logged in")
            || normalized_detail.contains("authentication failed")
            || normalized_detail.contains("authentication required")
            || normalized_detail.contains("http 401")
    }
}

/// Configuration for provider-specific metadata synchronization.
pub(crate) struct SyncReviewRequestMetadataConfig {
    /// Builds the edit command when metadata differs from desired input.
    pub(crate) edit_metadata_command:
        fn(&ForgeRemote, &str, &ReviewRequestMetadataEdit) -> ForgeCommand,
    /// User-facing operation prefix for edit failures.
    pub(crate) edit_operation: &'static str,
    /// Parses one provider display id into a CLI argument.
    pub(crate) parse_display_id: fn(&str) -> Result<String, ReviewRequestError>,
    /// Parses provider metadata JSON.
    pub(crate) parse_metadata_response: fn(&str) -> Result<ReviewRequestMetadata, String>,
    /// Builds the metadata view command.
    pub(crate) view_metadata_command: fn(&ForgeRemote, &str) -> ForgeCommand,
    /// User-facing operation prefix for metadata view failures.
    pub(crate) view_operation: &'static str,
}

/// Wraps a provider operation failure with its forge kind.
pub(crate) fn operation_failed(
    forge_kind: ForgeKind,
    message: impl Into<String>,
) -> ReviewRequestError {
    ReviewRequestError::OperationFailed {
        forge_kind,
        message: message.into(),
    }
}

/// Maps provider parser failures into a normalized operation error.
pub(crate) fn map_parse_error<T>(
    forge_kind: ForgeKind,
    result: Result<T, String>,
) -> Result<T, ReviewRequestError> {
    result.map_err(|message| operation_failed(forge_kind, message))
}

/// Joins one ordered list of status-summary parts into a comma-separated
/// label, returning `None` when `parts` is empty.
pub(crate) fn status_summary_parts(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        return None;
    }

    Some(parts.join(", "))
}

/// Formats one provider enum-like label into sentence case words.
pub(crate) fn normalize_provider_label(label: &str) -> String {
    let lowercase = label.replace('_', " ").to_ascii_lowercase();
    let mut characters = lowercase.chars();
    let Some(first_character) = characters.next() else {
        return String::new();
    };

    let mut normalized = first_character.to_uppercase().collect::<String>();
    normalized.push_str(characters.as_str());

    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_plan_updates_fields_that_still_match_reconciled_values() {
        // Arrange
        let metadata = ReviewRequestMetadata {
            body: "Current body.".to_string(),
            title: "Current title".to_string(),
        };
        let input = UpdateReviewRequestInput {
            body: Some(crate::ReviewRequestMetadataFieldUpdate {
                current: "Current body.".to_string(),
                desired: "New body.".to_string(),
            }),
            title: Some(crate::ReviewRequestMetadataFieldUpdate {
                current: "Current title".to_string(),
                desired: "New title".to_string(),
            }),
        };

        // Act
        let plan = ReviewRequestMetadataPlan::new(&metadata, &input);

        // Assert
        assert_eq!(plan.edit.body.as_deref(), Some("New body."));
        assert_eq!(plan.edit.title.as_deref(), Some("New title"));
        assert!(plan.requires_edit());
    }

    #[test]
    fn metadata_plan_skips_fields_changed_after_reconciliation() {
        // Arrange
        let metadata = ReviewRequestMetadata {
            body: "New remote body.".to_string(),
            title: "New remote title".to_string(),
        };
        let input = UpdateReviewRequestInput {
            body: Some(crate::ReviewRequestMetadataFieldUpdate {
                current: "Earlier body.".to_string(),
                desired: "Desired body.".to_string(),
            }),
            title: Some(crate::ReviewRequestMetadataFieldUpdate {
                current: "Earlier title".to_string(),
                desired: "Desired title".to_string(),
            }),
        };

        // Act
        let plan = ReviewRequestMetadataPlan::new(&metadata, &input);

        // Assert
        assert_eq!(plan.edit.body, None);
        assert_eq!(plan.edit.title, None);
        assert!(!plan.requires_edit());
    }

    #[test]
    fn metadata_plan_omits_unchanged_reconciled_fields() {
        // Arrange
        let metadata = ReviewRequestMetadata {
            body: "Current body.".to_string(),
            title: "Current title".to_string(),
        };
        let input = UpdateReviewRequestInput {
            body: Some(crate::ReviewRequestMetadataFieldUpdate {
                current: "Current body.".to_string(),
                desired: "Current body.".to_string(),
            }),
            title: Some(crate::ReviewRequestMetadataFieldUpdate {
                current: "Current title".to_string(),
                desired: "Current title".to_string(),
            }),
        };

        // Act
        let plan = ReviewRequestMetadataPlan::new(&metadata, &input);

        // Assert
        assert!(!plan.requires_edit());
    }

    #[test]
    fn looks_like_authentication_failure_matches_github_cli_login_prompt() {
        // Arrange
        let detail = "You are not logged into any GitHub hosts. Run `gh auth login`.";

        // Act
        let matched =
            ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitHub);

        // Assert
        assert!(matched);
    }

    #[test]
    fn looks_like_authentication_failure_matches_gitlab_cli_login_prompt() {
        // Arrange
        let detail = "You are not logged in. Run `glab auth login`.";

        // Act
        let matched =
            ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitLab);

        // Assert
        assert!(matched);
    }

    #[test]
    fn looks_like_authentication_failure_matches_http_401() {
        // Arrange
        let detail = "HTTP 401 Unauthorized";

        // Act
        let matched_github =
            ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitHub);
        let matched_gitlab =
            ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitLab);

        // Assert
        assert!(matched_github);
        assert!(matched_gitlab);
    }

    #[test]
    fn looks_like_authentication_failure_returns_false_for_unrelated_detail() {
        // Arrange
        let detail = "Request failed: rate limit exceeded";

        // Act
        let matched =
            ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitHub);

        // Assert
        assert!(!matched);
    }

    #[test]
    fn looks_like_host_resolution_failure_matches_common_dns_errors() {
        // Arrange
        let details = [
            "dial tcp: lookup github.com: no such host",
            "Name or service not known",
            "Temporary failure in name resolution",
            "Could not resolve host: gitlab.example.internal",
        ];

        // Act & Assert
        for detail in details {
            assert!(
                ReviewRequestOperations::looks_like_host_resolution_failure(detail),
                "expected `{detail}` to match",
            );
        }
    }

    #[test]
    fn looks_like_host_resolution_failure_returns_false_for_unrelated_detail() {
        // Arrange
        let detail = "HTTP 500 Internal Server Error";

        // Act
        let matched = ReviewRequestOperations::looks_like_host_resolution_failure(detail);

        // Assert
        assert!(!matched);
    }

    #[test]
    fn operation_failed_preserves_forge_kind_and_message() {
        // Arrange
        let forge_kind = ForgeKind::GitLab;
        let message = "merge request lookup failed";

        // Act
        let error = operation_failed(forge_kind, message);

        // Assert
        assert_eq!(
            error,
            ReviewRequestError::OperationFailed {
                forge_kind,
                message: message.to_string(),
            }
        );
    }

    #[test]
    fn status_summary_parts_returns_none_for_empty_input() {
        // Arrange
        let parts: Vec<String> = Vec::new();

        // Act
        let summary = status_summary_parts(&parts);

        // Assert
        assert_eq!(summary, None);
    }

    #[test]
    fn status_summary_parts_joins_values_with_commas() {
        // Arrange
        let parts = vec![
            "Draft".to_string(),
            "Approved".to_string(),
            "Mergeable".to_string(),
        ];

        // Act
        let summary = status_summary_parts(&parts);

        // Assert
        assert_eq!(summary.as_deref(), Some("Draft, Approved, Mergeable"));
    }

    #[test]
    fn normalize_provider_label_capitalizes_first_letter_and_replaces_underscores() {
        // Arrange
        let label = "CHANGES_REQUESTED";

        // Act
        let normalized = normalize_provider_label(label);

        // Assert
        assert_eq!(normalized, "Changes requested");
    }

    #[test]
    fn normalize_provider_label_returns_empty_string_for_empty_input() {
        // Arrange
        let label = "";

        // Act
        let normalized = normalize_provider_label(label);

        // Assert
        assert_eq!(normalized, String::new());
    }

    #[test]
    fn map_spawn_error_maps_executable_not_found_to_cli_not_installed() {
        // Arrange
        let remote = sample_remote(ForgeKind::GitHub);
        let error = ForgeCommandError::ExecutableNotFound {
            executable: "gh".to_string(),
        };

        // Act
        let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

        // Assert
        assert_eq!(
            review_request_error,
            ReviewRequestError::CliNotInstalled {
                forge_kind: ForgeKind::GitHub,
            }
        );
    }

    #[test]
    fn map_spawn_error_maps_host_resolution_failure_for_gitlab() {
        // Arrange
        let remote = sample_remote(ForgeKind::GitLab);
        let error = ForgeCommandError::SpawnFailed {
            executable: "glab".to_string(),
            message: "dial tcp: lookup gitlab.example.internal: no such host".to_string(),
        };

        // Act
        let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

        // Assert
        assert_eq!(
            review_request_error,
            ReviewRequestError::HostResolutionFailed {
                forge_kind: ForgeKind::GitLab,
                host: "gitlab.example.internal".to_string(),
            }
        );
    }

    #[test]
    fn map_spawn_error_falls_back_to_operation_failed_with_cli_name() {
        // Arrange
        let remote = sample_remote(ForgeKind::GitHub);
        let error = ForgeCommandError::SpawnFailed {
            executable: "gh".to_string(),
            message: "permission denied".to_string(),
        };

        // Act
        let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

        // Assert
        assert_eq!(
            review_request_error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "failed to execute `gh`: permission denied".to_string(),
            }
        );
    }

    #[test]
    fn map_spawn_error_reports_command_timeout() {
        // Arrange
        let remote = sample_remote(ForgeKind::GitHub);
        let error = ForgeCommandError::TimedOut {
            executable: "gh".to_string(),
            timeout: std::time::Duration::from_secs(30),
        };

        // Act
        let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

        // Assert
        assert_eq!(
            review_request_error,
            ReviewRequestError::OperationFailed {
                forge_kind: ForgeKind::GitHub,
                message: "`gh` timed out after 30 seconds while contacting github.com".to_string(),
            }
        );
    }

    fn sample_remote(forge_kind: ForgeKind) -> ForgeRemote {
        let host = match forge_kind {
            ForgeKind::GitHub => "github.com",
            ForgeKind::GitLab => "gitlab.example.internal",
        };
        ForgeRemote {
            command_working_directory: None,
            forge_kind,
            host: host.to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: format!("https://{host}/agentty-xyz/agentty.git"),
            web_url: format!("https://{host}/agentty-xyz/agentty"),
        }
    }
}
