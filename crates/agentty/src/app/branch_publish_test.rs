use std::path::PathBuf;
use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::super::session;
use super::{
    BranchPublishTaskFailure, BranchPublishTaskSession, ReviewRequestCreationInfo,
    branch_push_success_message, detected_forge_kind_from_git_push_error,
    git_push_authentication_message, is_git_push_authentication_error,
    push_session_branch_to_remote, review_request_created_notice, review_request_queued_label,
    review_request_remote,
};
use crate::domain::session::{PublishBranchAction, ReviewRequest, Status};
use crate::infra::db::AppRepositories;

#[test]
fn review_request_queued_label_describes_waiting_without_loading_punctuation() {
    // Arrange, Act
    let label = review_request_queued_label();

    // Assert
    assert_eq!(label, "review request — publish after this turn");
}

fn expect_safe_session_branch_push(mock_git_client: &mut git::MockGitClient, session_id: &str) {
    let expected_branch = session::session_branch(session_id);
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(move |_| {
            let expected_branch = expected_branch.clone();

            Box::pin(async move { Some(expected_branch) })
        });
}

async fn push_session_branch_to_remote_with_mock(
    mock_git_client: git::MockGitClient,
) -> Result<String, BranchPublishTaskFailure> {
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");

    push_session_branch_to_remote(
        &database,
        PathBuf::from("/tmp/session-worktree"),
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        None,
        Some("origin/wt/session-id"),
    )
    .await
}

#[tokio::test]
async fn review_request_remote_attaches_session_worktree_to_detected_remote() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let branch_publish_session = BranchPublishTaskSession {
        base_branch: "main".to_string(),
        folder: session_folder.clone(),
        id: "session-id".into(),
        published_upstream_ref: None,
        review_request: None,
        status: Status::Review,
    };
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_repo_url()
        .once()
        .withf({
            let session_folder = session_folder.clone();
            move |candidate_folder| candidate_folder == &session_folder
        })
        .returning(|_| {
            Box::pin(async { Ok("https://gitlab.com/agentty-xyz/agentty.git".to_string()) })
        });
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .withf(|repo_url| repo_url == "https://gitlab.com/agentty-xyz/agentty.git")
        .returning(|_| {
            Ok(forge::ForgeRemote {
                command_working_directory: None,
                forge_kind: forge::ForgeKind::GitLab,
                host: "gitlab.com".to_string(),
                namespace: "agentty-xyz".to_string(),
                project: "agentty".to_string(),
                repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
                web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
            })
        });

    // Act
    let remote = review_request_remote(
        &branch_publish_session,
        Arc::new(mock_git_client),
        &mock_review_request_client,
    )
    .await
    .expect("remote should resolve");

    // Assert
    assert_eq!(remote.command_working_directory, Some(session_folder));
    assert_eq!(remote.forge_kind, forge::ForgeKind::GitLab);
}

#[tokio::test]
async fn push_session_branch_to_remote_persists_upstream_reference() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let expected_session_folder = session_folder.clone();
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| Box::pin(async { Ok(false) }));
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_push_current_branch_to_new_remote_branch()
        .once()
        .withf(move |folder, remote_branch_name| {
            folder == &expected_session_folder && remote_branch_name == "wt/session-id"
        })
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));

    // Act
    let upstream_reference = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        Some("wt/session-id"),
        None,
    )
    .await
    .expect("branch push should succeed");
    let persisted_session = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load sessions")
        .into_iter()
        .find(|session| session.id == "session-id")
        .expect("missing session row");

    // Assert
    assert_eq!(upstream_reference, "origin/wt/session-id");
    assert_eq!(
        persisted_session.published_upstream_ref.as_deref(),
        Some("origin/wt/session-id")
    );
}

/// Describes one auth-guidance parsing scenario for `branch_push_failure`.
struct AuthGuidanceCase {
    error: &'static str,
    expected_cli_guidance: Option<&'static str>,
    name: &'static str,
}

/// Verifies branch-push auth guidance uses detected forge hints when the
/// error text includes a recognizable host.
#[test]
fn branch_push_failure_uses_detected_forge_guidance() {
    // Arrange
    let error = "git push failed: could not read username for \
                 'https://github.com/openai/agentty': terminal prompts disabled";

    // Act
    let failure = branch_push_failure(PublishBranchAction::Push, error);

    // Assert
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("gh auth login"));
    assert!(failure.message.contains("push the branch again"));
}

/// Verifies auth guidance handles additional git push error formats
/// without regressing forge detection or fallback messaging.
#[test]
fn branch_push_failure_handles_multiple_auth_error_formats() {
    // Arrange
    let cases = vec![
            AuthGuidanceCase {
                name: "mixed-case https url",
                error: "Git push failed: fatal: could not read Username for 'HTTPS://GitHub.com/OpenAI/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "password prompt without scheme",
                error: "Git push failed: fatal: could not read Password for 'github.com/OpenAI/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "github url with port and subpath",
                error: "Git push failed: fatal: could not read Username for 'https://user@github.com:443/openai/agentty/path': terminal prompts disabled",
                expected_cli_guidance: Some("gh auth login"),
            },
            AuthGuidanceCase {
                name: "gitlab host uses glab guidance",
                error: "Git push failed: fatal: could not read Username for 'https://gitlab.com/openai/agentty': terminal prompts disabled",
                expected_cli_guidance: Some("glab auth login"),
            },
            AuthGuidanceCase {
                name: "self-hosted gitlab token uses glab guidance",
                error: "Git push failed: authentication failed while contacting gitlab.company.org for review branch",
                expected_cli_guidance: Some("glab auth login"),
            },
            AuthGuidanceCase {
                name: "non-forge host falls back to generic guidance",
                error: "Git push failed: fatal: could not read Username for 'https://example.com/openai/agentty': terminal prompts disabled",
                expected_cli_guidance: None,
            },
        ];

    // Act
    for case in cases {
        let failure = branch_push_failure(PublishBranchAction::Push, case.error);

        // Assert
        assert_eq!(failure.title, "Branch push blocked", "case: {}", case.name);
        assert!(
            failure.message.contains("push the branch again"),
            "case: {}",
            case.name
        );
        if let Some(expected_cli_guidance) = case.expected_cli_guidance {
            assert!(
                failure.message.contains(expected_cli_guidance),
                "case: {}",
                case.name
            );
        } else {
            assert!(
                !failure.message.contains("gh auth login"),
                "case: {}",
                case.name
            );
            assert!(
                !failure.message.contains("glab auth login"),
                "case: {}",
                case.name
            );
            assert!(
                failure.message.contains("PAT/SSH key or credential helper"),
                "case: {}",
                case.name
            );
        }
    }
}

#[test]
fn branch_push_success_message_uses_gitlab_merge_request_copy() {
    // Arrange
    let review_request_creation = ReviewRequestCreationInfo {
        forge_kind: forge::ForgeKind::GitLab,
        web_url: Some("https://gitlab.com/agentty-xyz/agentty/-/merge_requests/new".to_string()),
    };

    // Act
    let message = branch_push_success_message("wt/session-1", Some(&review_request_creation));

    // Assert
    assert!(message.contains("create the merge request"));
    assert!(message.contains("gitlab.com/agentty-xyz/agentty/-/merge_requests/new"));
}

#[test]
fn review_request_created_notice_uses_gitlab_short_name() {
    // Arrange
    let review_request = ReviewRequest {
        last_refreshed_at: 42,
        summary: forge::ReviewRequestSummary {
            display_id: "!24".to_string(),
            forge_kind: forge::ForgeKind::GitLab,
            source_branch: "wt/session-1".to_string(),
            state: forge::ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Add GitLab support".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24".to_string(),
        },
    };

    // Act
    let notice = review_request_created_notice(&review_request);

    // Assert
    assert_eq!(
            notice,
            "\n[Review Request] Created MR \
             https://gitlab.com/agentty-xyz/agentty/-/merge_requests/24\n"
        );
}

#[tokio::test]
async fn push_blocks_when_custom_remote_branch_already_exists() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| Box::pin(async { Ok(true) }));

    // Act
    let result = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        Some("feature/existing"),
        None,
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("already exists"));
}

#[tokio::test]
async fn push_uses_tracked_lease_when_upstream_ref_is_already_set() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let expected_folder = session_folder.clone();
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(move |folder, branch| folder == &expected_folder && branch == "feature/existing")
        .returning(|_, _| Box::pin(async { Ok("origin/feature/existing".to_string()) }));

    // Act
    let result = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        Some("feature/existing"),
        Some("origin/feature/existing"),
    )
    .await;

    // Assert
    let upstream = result.expect("push should succeed");
    assert_eq!(upstream, "origin/feature/existing");
}

#[tokio::test]
async fn push_uses_tracked_lease_for_default_session_branch_name() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let expected_folder = session_folder.clone();
    let expected_branch = session::session_branch("session-id");
    let expected_upstream = format!("origin/{expected_branch}");
    let expected_upstream_assertion = expected_upstream.clone();
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(move |folder, branch| folder == &expected_folder && branch == &expected_branch)
        .returning(move |_, _| {
            let expected_upstream = expected_upstream.clone();

            Box::pin(async move { Ok(expected_upstream) })
        });

    // Act
    let result = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        None,
        None,
    )
    .await;

    // Assert
    let upstream = result.expect("push should succeed");
    assert_eq!(upstream, expected_upstream_assertion);
}

#[tokio::test]
async fn push_blocks_while_session_branch_rebase_is_in_progress() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::Rebase)) }));
    mock_git_client.expect_detect_git_info().times(0);
    mock_git_client.expect_remote_branch_exists().times(0);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .times(0);
    mock_git_client
        .expect_push_current_branch_to_new_remote_branch()
        .times(0);

    // Act
    let result = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        Some("feature/new-branch"),
        None,
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("rebase is in progress"));
}

#[tokio::test]
async fn push_blocks_while_session_branch_merge_is_in_progress() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::Merge)) }));
    mock_git_client.expect_detect_git_info().times(0);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .times(0);

    // Act
    let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("merge is in progress"));
}

#[tokio::test]
async fn push_blocks_while_session_branch_cherry_pick_is_in_progress() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::CherryPick)) }));
    mock_git_client.expect_detect_git_info().times(0);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .times(0);

    // Act
    let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("cherry-pick is in progress"));
}

#[tokio::test]
async fn push_blocks_while_session_branch_revert_is_in_progress() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(git::InProgressGitOperation::Revert)) }));
    mock_git_client.expect_detect_git_info().times(0);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .times(0);

    // Act
    let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("revert is in progress"));
}

#[tokio::test]
async fn push_blocks_when_worktree_is_not_on_session_branch() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("main".to_string()) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .times(0);

    // Act
    let result = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        None,
        Some("origin/wt/session-id"),
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("worktree is on `main`"));
    assert!(failure.message.contains("instead of `wt/session-`"));
}

#[tokio::test]
async fn push_reports_folder_when_session_branch_cannot_be_detected() {
    // Arrange
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .times(0);

    // Act
    let result = push_session_branch_to_remote_with_mock(mock_git_client).await;

    // Assert
    let failure = result.expect_err("push should fail");
    assert_eq!(failure.title, "Branch push failed");
    assert!(
        failure
            .message
            .contains("`/tmp/session-worktree` before pushing")
    );
}

#[tokio::test]
async fn push_shows_auth_guidance_when_ls_remote_returns_auth_error() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_remote_branch_exists()
        .once()
        .returning(|_, _| {
            Box::pin(async move {
                Err(git::GitError::CommandFailed {
                    command: "git ls-remote".to_string(),
                    stderr: "fatal: could not read Username for 'https://github.com/org/repo': \
                             terminal prompts disabled"
                        .to_string(),
                })
            })
        });

    // Act
    let result = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        Some("feature/new-branch"),
        None,
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("Git push requires authentication"));
    assert!(failure.message.contains("push the branch again"));
    assert!(failure.message.contains("gh auth login"));
}

#[tokio::test]
async fn push_shows_auth_guidance_when_push_returns_auth_error() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session("session-id", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let expected_folder = session_folder.clone();
    let expected_branch = session::session_branch("session-id");
    let expected_push_command = format!("git push origin HEAD:{expected_branch}");
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(move |folder, branch| folder == &expected_folder && branch == &expected_branch)
        .returning(move |_, _| {
            let expected_push_command = expected_push_command.clone();

            Box::pin(async {
                Err(git::GitError::CommandFailed {
                    command: expected_push_command,
                    stderr: "fatal: Authentication failed for 'https://gitlab.com/org/repo.git/'"
                        .to_string(),
                })
            })
        });

    // Act
    let result = push_session_branch_to_remote(
        &database,
        session_folder,
        Arc::new(mock_git_client),
        PublishBranchAction::Push,
        "session-id",
        None,
        None,
    )
    .await;

    // Assert
    let failure = result.expect_err("push should be blocked");
    assert_eq!(failure.title, "Branch push blocked");
    assert!(failure.message.contains("Git push requires authentication"));
    assert!(failure.message.contains("push the branch again"));
    assert!(failure.message.contains("glab auth login"));
}

#[test]
fn with_action_preserves_blocked_distinction() {
    // Arrange
    let blocked =
        BranchPublishTaskFailure::blocked(PublishBranchAction::Push, "auth error".to_string());
    let failed =
        BranchPublishTaskFailure::failed(PublishBranchAction::Push, "generic error".to_string());

    // Act
    let adjusted_blocked = blocked.with_action(PublishBranchAction::PublishPullRequest);
    let adjusted_failed = failed.with_action(PublishBranchAction::PublishPullRequest);

    // Assert
    assert_eq!(adjusted_blocked.title, "Review request publish blocked");
    assert_eq!(adjusted_blocked.message, "auth error");
    assert_eq!(adjusted_failed.title, "Review request publish failed");
    assert_eq!(adjusted_failed.message, "generic error");
}

impl BranchPublishTaskFailure {
    /// Rebuilds the popup title for a different publish action while
    /// preserving the blocked/failed distinction and original message.
    pub(crate) fn with_action(self, publish_branch_action: PublishBranchAction) -> Self {
        if self.is_blocked {
            Self::blocked(publish_branch_action, self.message)
        } else {
            Self::failed(publish_branch_action, self.message)
        }
    }
}
/// Maps one branch-publish failure into blocked or failed popup copy.
pub(crate) fn branch_push_failure(
    publish_branch_action: PublishBranchAction,
    error: &str,
) -> BranchPublishTaskFailure {
    if !is_git_push_authentication_error(error) {
        return BranchPublishTaskFailure::failed(
            publish_branch_action,
            format!("Failed to publish session branch: {error}"),
        );
    }

    BranchPublishTaskFailure::blocked(
        publish_branch_action,
        git_push_authentication_message(
            detected_forge_kind_from_git_push_error(error),
            match publish_branch_action {
                PublishBranchAction::Push => "push the branch again",
                PublishBranchAction::PublishPullRequest => "publish the review request again",
            },
        ),
    )
}
