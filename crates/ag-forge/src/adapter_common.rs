//! Shared helpers used by forge review-request adapters.
//!
//! Each supported forge (GitHub, GitLab) needs the same normalization for
//! authentication failures, host-resolution failures, status-summary joining,
//! provider label casing, and spawn-time error mapping. Keeping these in one
//! module avoids divergence between adapters.

use std::sync::Arc;

use super::{
    ForgeCommand, ForgeCommandError, ForgeCommandOutput, ForgeCommandRunner, ForgeFuture,
    ForgeKind, ForgeRemote, ReviewRequestError, ReviewRequestMetadata, ReviewRequestSummary,
    UpdateReviewRequestInput, command_output_detail,
};

/// Provider-neutral partial edit produced after a best-effort recheck that the
/// remote fields still match the values used during semantic reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestMetadataEdit {
    pub(crate) body: Option<String>,
    pub(crate) title: Option<String>,
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

#[cfg(test)]
#[path = "adapter_common_test.rs"]
mod tests;
