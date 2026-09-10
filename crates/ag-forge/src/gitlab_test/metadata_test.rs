use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{gitlab_remote, gitlab_view_json, reconciled_field, success_output};
use crate::adapter_common::ReviewRequestMetadataEdit;
use crate::client::ReviewRequestAdapter;
use crate::command::MockForgeCommandRunner;
use crate::gitlab::GitLabReviewRequestAdapter;
use crate::model::{ReviewRequestMetadata, UpdateReviewRequestInput};

#[tokio::test]
async fn authenticated_review_request_metadata_loads_current_merge_request() {
    // Arrange
    let remote = gitlab_remote();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .withf({
            let remote = remote.clone();

            move |command| command == &GitLabReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let metadata = adapter
        .authenticated_review_request_metadata(remote, "!42".to_string())
        .await
        .expect("GitLab metadata lookup should succeed");

    // Assert
    assert_eq!(
        metadata,
        ReviewRequestMetadata {
            body: "Current description.".to_string(),
            title: "Add forge review support".to_string(),
        }
    );
}

#[tokio::test]
async fn sync_authenticated_review_request_metadata_updates_changed_merge_request() {
    // Arrange
    let remote = gitlab_remote();
    let input = UpdateReviewRequestInput {
        body: Some(reconciled_field(
            "Current description.",
            "Updated description.",
        )),
        title: Some(reconciled_field(
            "Add forge review support",
            "Refine forge review support",
        )),
    };
    let edit = ReviewRequestMetadataEdit {
        body: Some("Updated description.".to_string()),
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

            move |command| command == &GitLabReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();
            let edit = edit.clone();

            move |command| {
                command
                    == &GitLabReviewRequestAdapter::update_metadata_command(&remote, "42", &edit)
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitLabReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let summary = adapter
        .sync_authenticated_review_request_metadata(remote, "!42".to_string(), input)
        .await
        .expect("GitLab metadata sync should succeed");

    // Assert
    assert_eq!(summary.display_id, "!42");
}

#[tokio::test]
async fn sync_authenticated_review_request_metadata_skips_update_when_unchanged() {
    // Arrange
    let remote = gitlab_remote();
    let input = UpdateReviewRequestInput {
        body: Some(reconciled_field(
            "Current description.",
            "Current description.",
        )),
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

            move |command| command == &GitLabReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitLabReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let summary = adapter
        .sync_authenticated_review_request_metadata(remote, "!42".to_string(), input)
        .await
        .expect("GitLab metadata sync should succeed");

    // Assert
    assert_eq!(summary.display_id, "!42");
}

#[test]
fn update_metadata_command_can_update_only_title() {
    // Arrange
    let remote = gitlab_remote();
    let edit = ReviewRequestMetadataEdit {
        body: None,
        title: Some("Manual-safe title".to_string()),
    };

    // Act
    let command = GitLabReviewRequestAdapter::update_metadata_command(&remote, "42", &edit);

    // Assert
    assert_eq!(
        command,
        GitLabReviewRequestAdapter::gitlab_command(
            &remote,
            "glab",
            vec![
                "mr".to_string(),
                "update".to_string(),
                "42".to_string(),
                "--repo".to_string(),
                remote.web_url.clone(),
                "--title".to_string(),
                "Manual-safe title".to_string(),
                "--yes".to_string(),
            ],
        )
    );
}

#[test]
fn update_metadata_command_can_update_only_description() {
    // Arrange
    let remote = gitlab_remote();
    let edit = ReviewRequestMetadataEdit {
        body: Some("Manual-safe description".to_string()),
        title: None,
    };

    // Act
    let command = GitLabReviewRequestAdapter::update_metadata_command(&remote, "42", &edit);

    // Assert
    assert_eq!(
        command,
        GitLabReviewRequestAdapter::gitlab_command(
            &remote,
            "glab",
            vec![
                "mr".to_string(),
                "update".to_string(),
                "42".to_string(),
                "--repo".to_string(),
                remote.web_url.clone(),
                "--description".to_string(),
                "Manual-safe description".to_string(),
                "--yes".to_string(),
            ],
        )
    );
}
