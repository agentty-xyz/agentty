use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{
    github_metadata_json, github_remote, github_view_json, reconciled_field, success_output,
};
use crate::adapter_common::ReviewRequestMetadataEdit;
use crate::client::ReviewRequestAdapter;
use crate::command::MockForgeCommandRunner;
use crate::github::GitHubReviewRequestAdapter;
use crate::model::{ReviewRequestMetadata, UpdateReviewRequestInput};

#[tokio::test]
async fn authenticated_review_request_metadata_loads_current_pull_request() {
    // Arrange
    let remote = github_remote();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::view_metadata_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_metadata_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let metadata = adapter
        .authenticated_review_request_metadata(remote, "#42".to_string())
        .await
        .expect("GitHub metadata lookup should succeed");

    // Assert
    assert_eq!(
        metadata,
        ReviewRequestMetadata {
            body: "Current body.".to_string(),
            title: "Add forge review support".to_string(),
        }
    );
}

#[tokio::test]
async fn sync_authenticated_review_request_metadata_edits_changed_pull_request() {
    // Arrange
    let remote = github_remote();
    let input = UpdateReviewRequestInput {
        body: Some(reconciled_field("Current body.", "Updated body.")),
        title: Some(reconciled_field(
            "Add forge review support",
            "Refine forge review support",
        )),
    };
    let edit = ReviewRequestMetadataEdit {
        body: Some("Updated body.".to_string()),
        title: Some("Refine forge review support".to_string()),
    };
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::view_metadata_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_metadata_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();
            let edit = edit.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::edit_metadata_command(&remote, "42", &edit)
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitHubReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let summary = adapter
        .sync_authenticated_review_request_metadata(remote, "#42".to_string(), input)
        .await
        .expect("GitHub metadata sync should succeed");

    // Assert
    assert_eq!(summary.display_id, "#42");
}

#[tokio::test]
async fn sync_authenticated_review_request_metadata_skips_edit_when_unchanged() {
    // Arrange
    let remote = github_remote();
    let input = UpdateReviewRequestInput {
        body: Some(reconciled_field("Current body.", "Current body.")),
        title: Some(reconciled_field(
            "Add forge review support",
            "Add forge review support",
        )),
    };
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::view_metadata_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_metadata_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitHubReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let summary = adapter
        .sync_authenticated_review_request_metadata(remote, "#42".to_string(), input)
        .await
        .expect("GitHub metadata sync should succeed");

    // Assert
    assert_eq!(summary.display_id, "#42");
}

#[test]
fn edit_metadata_command_can_update_only_title() {
    // Arrange
    let remote = github_remote();
    let edit = ReviewRequestMetadataEdit {
        body: None,
        title: Some("Manual-safe title".to_string()),
    };

    // Act
    let command = GitHubReviewRequestAdapter::edit_metadata_command(&remote, "42", &edit);

    // Assert
    assert_eq!(
        command,
        GitHubReviewRequestAdapter::github_command(
            &remote,
            vec![
                "pr".to_string(),
                "edit".to_string(),
                "42".to_string(),
                "--repo".to_string(),
                remote.project_path(),
                "--title".to_string(),
                "Manual-safe title".to_string(),
            ],
        )
    );
}

#[test]
fn edit_metadata_command_can_update_only_body() {
    // Arrange
    let remote = github_remote();
    let edit = ReviewRequestMetadataEdit {
        body: Some("Manual-safe body".to_string()),
        title: None,
    };

    // Act
    let command = GitHubReviewRequestAdapter::edit_metadata_command(&remote, "42", &edit);

    // Assert
    assert_eq!(
        command,
        GitHubReviewRequestAdapter::github_command(
            &remote,
            vec![
                "pr".to_string(),
                "edit".to_string(),
                "42".to_string(),
                "--repo".to_string(),
                remote.project_path(),
                "--body".to_string(),
                "Manual-safe body".to_string(),
            ],
        )
    );
}
