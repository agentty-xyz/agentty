use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_forge as forge;
use ag_git::{self as git, BranchPushOutcome};
use ag_session::test_support as model_fixture;

use super::super::session;
use super::{
    BranchPublishTaskFailure, BranchPublishTaskSession, BranchPublishTaskSuccess,
    ReviewRequestCreationInfo, branch_push_success_message, create_or_refresh_review_request,
    detected_forge_kind_from_git_push_error, git_push_authentication_message,
    is_git_push_authentication_error, load_review_request_create_input, publish_pull_request,
    push_session_branch_to_remote, review_request_created_notice, review_request_queued_label,
    review_request_remote,
};
use crate::app::review_request::ReviewRequestGenerator;
use crate::domain::session::{PublishBranchAction, ReviewRequest, Status};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;
use crate::test_support::FixedClock;

#[tokio::test]
async fn manual_review_request_refresh_reconciles_both_forges_and_propagates_failures() {
    // Arrange
    for (forge_kind, outcome) in [
        (forge::ForgeKind::GitHub, "linked"),
        (forge::ForgeKind::GitLab, "discovered"),
        (forge::ForgeKind::GitHub, "concurrent"),
        (forge::ForgeKind::GitLab, "generation"),
        (forge::ForgeKind::GitHub, "missing_commit"),
        (forge::ForgeKind::GitLab, "head_error"),
        (forge::ForgeKind::GitHub, "metadata_error"),
    ] {
        let db = review_session_database().await;
        db.sessions()
            .update_session_published_upstream_ref("session-id", Some("origin/review/topic".into()))
            .await
            .expect("published branch");
        let summary = forge::ReviewRequestSummary {
            display_id: "42".into(),
            forge_kind,
            source_branch: "review/topic".into(),
            state: forge::ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".into(),
            title: "Stable title".into(),
            web_url: "https://example.com/review/42".into(),
        };
        let session = BranchPublishTaskSession {
            base_branch: "review/custom-parent".into(),
            folder: PathBuf::from("project"),
            id: "session-id".into(),
            published_upstream_ref: Some("origin/review/topic".into()),
            review_request: (outcome != "discovered").then(|| ReviewRequest {
                last_refreshed_at: 0,
                summary: summary.clone(),
            }),
            status: Status::Review,
        };
        let git_client = refresh_git_client(outcome);
        let client = refresh_forge_client(forge_kind, outcome, summary);
        let run_client = refresh_run_client(outcome);
        let remote = forge::detect_remote(match forge_kind {
            forge::ForgeKind::GitHub => "https://github.com/org/repo.git",
            forge::ForgeKind::GitLab => "https://gitlab.com/org/repo.git",
        })
        .expect("remote");
        let transcript = Mutex::new(SessionTranscript::default());
        let generator = ReviewRequestGenerator {
            db: &db,
            folder: &session.folder,
            git_client: &git_client,
            run_client: &run_client,
            session_agent: model_fixture::codex_selection(),
            session_id: "session-id",
            transcript: &transcript,
        };
        let clock: Arc<dyn crate::infra::clock::Clock> = Arc::new(FixedClock::unix_epoch());

        // Act
        let result = create_or_refresh_review_request(
            &session,
            &clock,
            Arc::new(client),
            &generator,
            remote,
            "review/topic".into(),
        )
        .await;

        // Assert
        if matches!(outcome, "linked" | "discovered") {
            assert_eq!(result.expect("refresh").summary.title, "Stable title");
        } else {
            assert!(result.is_err(), "{outcome}");
            assert!(
                db.reviews()
                    .load_session_review_request("session-id")
                    .await
                    .expect("review row")
                    .is_none()
            );
        }
    }
}

fn refresh_git_client(outcome: &'static str) -> Arc<dyn git::GitClient> {
    let mut git_client = git::MockGitClient::new();
    git_client
        .expect_head_commit_message()
        .once()
        .returning(move |_| {
            Box::pin(async move {
                match outcome {
                    "missing_commit" => Ok(None),
                    "head_error" => Err(git::GitError::CommandFailed {
                        command: "git log".into(),
                        stderr: "head unavailable".into(),
                    }),
                    _ => Ok(Some("Commit title\n\nCommit details".into())),
                }
            })
        });
    if !matches!(outcome, "missing_commit" | "head_error" | "metadata_error") {
        expect_published_commit_diff(&mut git_client, "release", "review/topic");
    }
    Arc::new(git_client)
}

fn expect_published_commit_diff(
    client: &mut git::MockGitClient,
    base_branch: &'static str,
    source_branch: &'static str,
) {
    client
        .expect_fetch_remote_branch_commits()
        .once()
        .withf(move |_, remote, base, source| {
            remote == "origin" && base == base_branch && source == source_branch
        })
        .returning(|_, _, _, _| Box::pin(async { Ok(("1".repeat(40), "2".repeat(40))) }));
    client
        .expect_committed_diff()
        .once()
        .withf(|_, base, source| base == &"1".repeat(40) && source == &"2".repeat(40))
        .returning(|_, _, _| Box::pin(async { Ok("cumulative change".into()) }));
}

fn refresh_forge_client(
    forge_kind: forge::ForgeKind,
    outcome: &'static str,
    summary: forge::ReviewRequestSummary,
) -> forge::MockReviewRequestClient {
    let mut client = forge::MockReviewRequestClient::new();
    if outcome == "discovered" {
        let found = summary.clone();
        client
            .expect_find_by_source_branch()
            .once()
            .return_once(move |_, _| Box::pin(async move { Ok(Some(found)) }));
    }
    let mut refreshed = summary.clone();
    refreshed.target_branch = "release".into();
    client
        .expect_refresh_review_request()
        .once()
        .return_once(move |_, _| Box::pin(async move { Ok(refreshed) }));
    if !matches!(outcome, "missing_commit" | "head_error") {
        client
            .expect_review_request_metadata()
            .once()
            .returning(move |_, _| {
                Box::pin(async move {
                    if outcome == "metadata_error" {
                        return Err(forge::ReviewRequestError::OperationFailed {
                            forge_kind,
                            message: "read failed".into(),
                        });
                    }
                    Ok(forge::ReviewRequestMetadata {
                        title: "Stable title".into(),
                        body: "- [ ] Keep user note #42\n\nCo-Authored-By: Agentty\nText below \
                               legacy footer"
                            .into(),
                    })
                })
            });
    }
    if matches!(outcome, "linked" | "discovered" | "concurrent") {
        client
            .expect_sync_review_request_metadata()
            .once()
            .withf(|_, _, input| {
                let body = input.body.as_ref().expect("body update");
                let title = input.title.as_ref().expect("title update");
                body.current.contains("Co-Authored-By: Agentty")
                    && !body.desired.contains("Co-Authored-By: Agentty")
                    && body.desired.ends_with("Text below legacy footer")
                    && title.current == "Stable title"
                    && title.desired == "Stable title"
            })
            .return_once(move |_, _, _| {
                Box::pin(async move {
                    if outcome == "concurrent" {
                        return Err(forge::ReviewRequestError::OperationFailed {
                            forge_kind,
                            message: "remote body changed concurrently".into(),
                        });
                    }
                    Ok(summary)
                })
            });
    }
    client
}

fn refresh_run_client(outcome: &'static str) -> ag_worker::MockRunClient {
    let mut run_client = ag_worker::MockRunClient::new();
    if !matches!(outcome, "missing_commit" | "head_error" | "metadata_error") {
        run_client.expect_submit().once().returning(move |_| {
            if outcome == "generation" {
                return Err(ag_contracts::OneShotError::new("provider unavailable"));
            }
            Ok(ag_contracts::OneShotSubmission { response: ag_protocol::AgentResponse::plain(serde_json::json!({
                "title": "Cosmetic change", "is_title_change_significant": false,
                "description": "## Why?\nClearer reviews.\n\n## What?\nUpdate descriptions.\n\n## Impact?\nPreserved context.\n\n- [ ] Keep user note #42\nText below legacy footer",
            }).to_string()), stats: ag_contracts::SessionStats::default() })
        });
    }

    run_client
}

#[tokio::test]
async fn review_request_creation_propagates_generation_and_commit_failures() {
    // Arrange
    for failure in ["head", "missing", "empty", "generation", "diff", "fetch"] {
        let db = review_session_database().await;
        db.sessions()
            .update_session_published_upstream_ref("session-id", Some("origin/review/topic".into()))
            .await
            .expect("published branch");
        let session = BranchPublishTaskSession {
            base_branch: "main".into(),
            folder: PathBuf::from("project"),
            id: "session-id".into(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::Review,
        };
        let mut git_client = git::MockGitClient::new();
        git_client
            .expect_head_commit_message()
            .once()
            .returning(move |_| {
                Box::pin(async move {
                    match failure {
                        "head" => Err(git::GitError::CommandFailed {
                            command: "git log".into(),
                            stderr: "unavailable".into(),
                        }),
                        "missing" => Ok(None),
                        "empty" => Ok(Some("\n".into())),
                        _ => Ok(Some("Commit title\n\nRaw commit body".into())),
                    }
                })
            });
        if matches!(failure, "generation" | "diff" | "fetch") {
            git_client
                .expect_fetch_remote_branch_commits()
                .once()
                .returning(move |_, _, _, _| {
                    Box::pin(async move {
                        if failure == "fetch" {
                            return Err(git::GitError::OutputParse(
                                "published commits unavailable".into(),
                            ));
                        }
                        Ok(("1".repeat(40), "2".repeat(40)))
                    })
                });
        }
        if matches!(failure, "generation" | "diff") {
            git_client
                .expect_committed_diff()
                .once()
                .returning(move |_, _, _| {
                    Box::pin(async move {
                        if failure == "diff" {
                            return Err(git::GitError::CommandFailed {
                                command: "git diff".into(),
                                stderr: "unavailable".into(),
                            });
                        }
                        Ok("cumulative change".into())
                    })
                });
        }
        let git_client: Arc<dyn git::GitClient> = Arc::new(git_client);
        let mut run_client = ag_worker::MockRunClient::new();
        if failure == "generation" {
            run_client
                .expect_submit()
                .once()
                .returning(|_| Err(ag_contracts::OneShotError::new("provider failed")));
        }
        let transcript = Mutex::new(SessionTranscript::default());
        let generator = ReviewRequestGenerator {
            db: &db,
            folder: &session.folder,
            git_client: &git_client,
            run_client: &run_client,
            session_agent: model_fixture::codex_selection(),
            session_id: "session-id",
            transcript: &transcript,
        };

        // Act
        let result =
            load_review_request_create_input(&session, &generator, "review/topic".into()).await;

        // Assert
        assert!(
            result.is_err(),
            "{failure} must not publish a raw commit body"
        );
    }
}

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

/// Creates the persisted review session used by branch publication tests.
async fn review_session_database() -> AppRepositories {
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    database
        .sessions()
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");

    database
}

async fn push_session_branch_to_remote_with_mock(
    mock_git_client: git::MockGitClient,
) -> Result<BranchPushOutcome, BranchPublishTaskFailure> {
    let database = review_session_database().await;

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
    let database = review_session_database().await;
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
    assert!(upstream_reference.updated);
    assert_eq!(
        upstream_reference.upstream_reference,
        "origin/wt/session-id"
    );
    assert_eq!(
        persisted_session.published_upstream_ref.as_deref(),
        Some("origin/wt/session-id")
    );
}

#[tokio::test]
async fn publish_pull_request_returns_pushed_upstream_and_persists_review_request() {
    // Arrange
    let database = review_session_database().await;
    let branch_publish_session = BranchPublishTaskSession {
        base_branch: "review/parent".to_string(),
        folder: PathBuf::from("/tmp/session-worktree"),
        id: "session-id".into(),
        published_upstream_ref: Some("origin/review/topic".to_string()),
        review_request: None,
        status: Status::Review,
    };
    let mut expected_review_request = published_review_request();
    expected_review_request.summary.target_branch = "review/parent".into();
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_push_current_branch_to_remote_branch_with_outcome()
        .once()
        .withf(|_, branch| branch == "review/topic")
        .returning(|_, _| {
            Box::pin(async {
                Ok(BranchPushOutcome {
                    updated: true,
                    upstream_reference: "origin/review/topic".to_string(),
                })
            })
        });
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    mock_git_client
        .expect_head_commit_message()
        .once()
        .returning(|_| Box::pin(async { Ok(Some("Publish changes".to_string())) }));
    expect_published_commit_diff(&mut mock_git_client, "review/parent", "review/topic");
    let run_client = publication_run_client();
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|repo_url| forge::detect_remote(&repo_url));
    mock_review_request_client
        .expect_find_by_source_branch()
        .once()
        .withf(|_, branch| branch == "review/topic")
        .returning(|_, _| Box::pin(async { Ok(None) }));
    let summary = expected_review_request.summary.clone();
    mock_review_request_client
        .expect_create_review_request()
        .once()
        .withf(|_, input| {
            input.source_branch == "review/topic"
                && input.target_branch == "review/parent"
                && input.title == "Publish changes"
                && input
                    .body
                    .as_ref()
                    .is_some_and(|body| body.starts_with("## Why?"))
        })
        .return_once(move |_, _| Box::pin(async move { Ok(summary) }));

    // Act
    let git_client: Arc<dyn git::GitClient> = Arc::new(mock_git_client);
    let result = publish_pull_request(
        &branch_publish_session,
        Arc::new(FixedClock::unix_epoch()),
        Arc::new(mock_review_request_client),
        &ReviewRequestGenerator {
            db: &database,
            folder: &branch_publish_session.folder,
            git_client: &git_client,
            run_client: &run_client,
            session_agent: model_fixture::codex_selection(),
            session_id: "session-id",
            transcript: &Mutex::new(SessionTranscript::default()),
        },
        Some("review/topic"),
    )
    .await
    .expect("review request should publish");

    // Assert
    assert_eq!(
        result,
        BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "review/topic".to_string(),
            review_request: expected_review_request.clone(),
            upstream_reference: "origin/review/topic".to_string(),
        }
    );
    let persisted_review_request = database
        .reviews()
        .load_session_review_request("session-id")
        .await
        .expect("review request should load")
        .expect("review request should be persisted");
    assert_eq!(persisted_review_request.display_id, "#42");
    assert_eq!(persisted_review_request.last_refreshed_at, 0);
    assert_eq!(
        persisted_review_request.web_url,
        expected_review_request.summary.web_url
    );
}

fn published_review_request() -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 0,
        summary: forge::ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: forge::ForgeKind::GitHub,
            source_branch: "review/topic".to_string(),
            state: forge::ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Publish changes".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    }
}

fn publication_run_client() -> ag_worker::MockRunClient {
    let mut run_client = ag_worker::MockRunClient::new();
    run_client.expect_submit().once().returning(|request| {
        assert!(request.prompt.contains("cumulative change"));
        Ok(ag_contracts::OneShotSubmission {
            response: ag_protocol::AgentResponse::plain(r###"{"title":"Publish changes","description":"## Why?\nFix publication.\n\n## What?\nGenerate review descriptions.\n\n## Impact?\nReviewers receive structured context.","is_title_change_significant":false}"###),
            stats: ag_contracts::SessionStats::default(),
        })
    });

    run_client
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
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
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
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let expected_folder = session_folder.clone();
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_push_current_branch_to_remote_branch_with_outcome()
        .once()
        .withf(move |folder, branch| folder == &expected_folder && branch == "feature/existing")
        .returning(|_, _| {
            Box::pin(async {
                Ok(BranchPushOutcome {
                    updated: true,
                    upstream_reference: "origin/feature/existing".to_string(),
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
        Some("feature/existing"),
        Some("origin/feature/existing"),
    )
    .await;

    // Assert
    let upstream = result.expect("push should succeed");
    assert_eq!(upstream.upstream_reference, "origin/feature/existing");
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
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
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
        .expect_push_current_branch_to_remote_branch_with_outcome()
        .once()
        .withf(move |folder, branch| folder == &expected_folder && branch == &expected_branch)
        .returning(move |_, _| {
            let expected_upstream = expected_upstream.clone();

            Box::pin(async move {
                Ok(BranchPushOutcome {
                    updated: true,

                    upstream_reference: expected_upstream,
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
    let upstream = result.expect("push should succeed");
    assert_eq!(upstream.upstream_reference, expected_upstream_assertion);
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
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
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
        .expect_push_current_branch_to_remote_branch_with_outcome()
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
        .expect_push_current_branch_to_remote_branch_with_outcome()
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
        .expect_push_current_branch_to_remote_branch_with_outcome()
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
        .expect_push_current_branch_to_remote_branch_with_outcome()
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
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
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
        .expect_push_current_branch_to_remote_branch_with_outcome()
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
        .expect_push_current_branch_to_remote_branch_with_outcome()
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
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
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
        .insert_session(
            "session-id",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let session_folder = PathBuf::from("/tmp/session-worktree");
    let expected_folder = session_folder.clone();
    let expected_branch = session::session_branch("session-id");
    let expected_push_command = format!("git push origin HEAD:{expected_branch}");
    let mut mock_git_client = git::MockGitClient::new();
    expect_safe_session_branch_push(&mut mock_git_client, "session-id");
    mock_git_client
        .expect_push_current_branch_to_remote_branch_with_outcome()
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
