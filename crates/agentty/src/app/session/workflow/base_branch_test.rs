//! Base branch snapshot tests covering capture, moved and deleted bases, and
//! the sessions the check covers.

use std::path::Path;

use ag_contracts::{AgentError, TurnResult};
use ag_git::{GitError, MockGitClient};
use ag_protocol::AgentResponse;
use ag_session::test_support as model_fixture;

use super::{BaseBranchScope, BaseBranchSnapshot};
use crate::domain::agent::AgentKind;
use crate::infra::db::AppRepositories;

/// The branch the session worktree's `HEAD` resolves to before a turn.
const SESSION_HEAD: &str = "refs/heads/wt/worker";

#[tokio::test]
async fn harness_turns_that_move_or_delete_the_base_fail_before_publishing() {
    // Arrange
    let db = session_database("origin/main").await;
    // Capture, unchanged, moved, then deleted; a failed turn reads nothing.
    let git_client = base_git(
        "origin/main",
        &[Some("base-1"), Some("base-1"), Some("moved"), None],
        &[Some(SESSION_HEAD), Some(SESSION_HEAD)],
    );
    let scope = scope(&db, &git_client, "worker");

    // Act
    let snapshot = BaseBranchSnapshot::capture(&scope, AgentKind::Harness)
        .await
        .expect("capture")
        .expect("base branch snapshot");
    let unchanged = BaseBranchSnapshot::verify(Some(&snapshot), &scope, Ok(completed_turn())).await;
    let moved = BaseBranchSnapshot::verify(Some(&snapshot), &scope, Ok(completed_turn())).await;
    let failed = BaseBranchSnapshot::verify(
        Some(&snapshot),
        &scope,
        Err(AgentError::Backend("provider".to_string())),
    )
    .await;
    let deleted = BaseBranchSnapshot::verify(Some(&snapshot), &scope, Ok(completed_turn())).await;
    let without_snapshot = BaseBranchSnapshot::verify(None, &scope, Ok(completed_turn())).await;

    // Assert
    assert!(unchanged.is_ok());
    let moved = moved.expect_err("a moved base fails the turn").to_string();
    assert!(moved.contains("base branch `origin/main` moved"), "{moved}");
    assert!(moved.contains("restore it to `base-1`"), "{moved}");
    assert!(matches!(failed, Err(AgentError::Backend(message)) if message == "provider"));
    assert!(deleted.is_err());
    assert!(without_snapshot.is_ok());
}

#[tokio::test]
async fn harness_turns_refuse_to_run_on_a_base_that_does_not_resolve() {
    // Arrange
    let db = session_database("main").await;
    let git_client = base_git("main", &[None], &[]);
    let scope = scope(&db, &git_client, "worker");

    // Act
    let result = BaseBranchSnapshot::capture(&scope, AgentKind::Harness).await;

    // Assert
    let error = result.expect_err("an unresolvable base fails").to_string();
    assert!(
        error.contains("base branch `main` does not resolve"),
        "{error}"
    );
}

#[tokio::test]
async fn harness_turns_that_retarget_head_fail_before_publishing() {
    // Arrange
    let db = session_database("main").await;
    // Capture, then a `HEAD` pointing at `main`, then one that no longer
    // resolves; the base stays in place throughout.
    let git_client = base_git(
        "main",
        &[Some("base-1"), Some("base-1"), Some("base-1")],
        &[Some(SESSION_HEAD), Some("refs/heads/main"), None],
    );
    let scope = scope(&db, &git_client, "worker");
    let unresolved_git = base_git("main", &[Some("base-1")], &[None]);

    // Act
    let snapshot = BaseBranchSnapshot::capture(&scope, AgentKind::Harness)
        .await
        .expect("capture")
        .expect("base branch snapshot");
    let retargeted =
        BaseBranchSnapshot::verify(Some(&snapshot), &scope, Ok(completed_turn())).await;
    let unresolved =
        BaseBranchSnapshot::verify(Some(&snapshot), &scope, Ok(completed_turn())).await;
    let unresolved_capture = BaseBranchSnapshot::capture(
        &self::scope(&db, &unresolved_git, "worker"),
        AgentKind::Harness,
    )
    .await;

    // Assert
    let retargeted = retargeted
        .expect_err("a retargeted HEAD fails the turn")
        .to_string();
    assert!(
        retargeted.contains(&format!("no longer resolves to `{SESSION_HEAD}`")),
        "{retargeted}"
    );
    assert!(unresolved.is_err());
    let unresolved_capture = unresolved_capture
        .expect_err("an unresolvable HEAD fails before the turn")
        .to_string();
    assert!(
        unresolved_capture.contains("`HEAD` does not resolve"),
        "{unresolved_capture}"
    );
}

#[tokio::test]
async fn base_branch_snapshots_cover_only_harness_sessions_with_a_base() {
    // Arrange
    let db = session_database("main").await;
    let git_client = base_git("main", &[], &[]);

    // Act
    let other_agent =
        BaseBranchSnapshot::capture(&scope(&db, &git_client, "worker"), AgentKind::Codex).await;
    let without_session =
        BaseBranchSnapshot::capture(&scope(&db, &git_client, "missing"), AgentKind::Harness).await;

    // Assert
    assert_eq!(other_agent.expect("other agents pass"), None);
    assert_eq!(without_session.expect("no recorded base"), None);
}

/// Returns a database with session `worker` recorded on `base_branch`.
async fn session_database(base_branch: &str) -> AppRepositories {
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/project", None)
        .await
        .expect("project");
    db.sessions()
        .insert_session(
            "worker",
            model_fixture::CODEX_MODEL_ID,
            base_branch,
            "InProgress",
            project_id,
        )
        .await
        .expect("session");

    db
}

/// Returns a Git client whose `reference` resolves to `commits` in order and
/// whose `HEAD` resolves to `heads` in order, with `None` for one that does
/// not resolve.
fn base_git(
    reference: &'static str,
    commits: &'static [Option<&'static str>],
    heads: &'static [Option<&'static str>],
) -> MockGitClient {
    let mut git_client = MockGitClient::new();
    let mut remaining_heads = heads.iter();
    git_client
        .expect_head_reference()
        .times(heads.len())
        .returning(move |_| {
            let head = remaining_heads.next().copied().flatten();
            Box::pin(async move {
                head.map(str::to_string)
                    .ok_or_else(|| GitError::OutputParse("unknown revision".into()))
            })
        });
    let mut remaining = commits.iter();
    git_client
        .expect_ref_hash()
        .withf(move |_, resolved| resolved == reference)
        .times(commits.len())
        .returning(move |_, _| {
            let commit = remaining.next().copied().flatten();
            Box::pin(async move {
                commit
                    .map(str::to_string)
                    .ok_or_else(|| GitError::OutputParse("unknown revision".into()))
            })
        });

    git_client
}

/// Returns the scope of `session_id` in a fixed repository folder.
fn scope<'a>(
    db: &'a AppRepositories,
    git_client: &'a MockGitClient,
    session_id: &'a str,
) -> BaseBranchScope<'a> {
    BaseBranchScope {
        db,
        folder: Path::new("/project/worktree"),
        git_client,
        session_id,
    }
}

/// Returns a successful turn result.
fn completed_turn() -> TurnResult {
    TurnResult {
        assistant_message: AgentResponse::plain("done"),
        context_reset: false,
        harness_report: None,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: None,
    }
}
