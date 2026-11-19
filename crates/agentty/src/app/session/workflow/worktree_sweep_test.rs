//! Startup sweep of leftover archived-session worktrees.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_git::{GitError, MockGitClient};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::mpsc;

use super::ArchivedWorktreeSweep;
use crate::app::AppServices;
use crate::domain::agent::{AgentCliInfo, AgentKind};
use crate::domain::session::Status;
use crate::infra::db::AppRepositories;
use crate::infra::fs::RealFsClient;

/// Repository root the mocked Git boundary resolves for every worktree.
const REPO_ROOT: &str = "/repo";

/// Worktree base inside a disposable root, with room for paths outside it.
struct SweepFixture {
    base: PathBuf,
    db: AppRepositories,
    pool: SqlitePool,
    project_id: i64,
    root: TempDir,
}

impl SweepFixture {
    /// Creates an empty base and a project whose checkout does not exist.
    async fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let base = root.path().join("wt");
        fs::create_dir(&base).expect("worktree base");
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("database");
        let project_id = db
            .projects()
            .upsert_project(
                &root.path().join("missing-project").to_string_lossy(),
                Some("main".to_string()),
            )
            .await
            .expect("project");

        Self {
            base,
            db,
            pool,
            project_id,
            root,
        }
    }

    /// Inserts one session row in the fixture project.
    async fn insert_session(&self, session_id: &str, status: Status) {
        self.insert_session_in_project(session_id, status, self.project_id)
            .await;
    }

    /// Inserts one session row in `project_id`.
    async fn insert_session_in_project(&self, session_id: &str, status: Status, project_id: i64) {
        self.db
            .sessions()
            .insert_session(
                session_id,
                "gemini-3.8-flash",
                "main",
                &status.to_string(),
                project_id,
            )
            .await
            .expect("session");
    }

    /// Creates a linked-worktree-shaped folder with an ignored build tree.
    fn create_worktree(&self, folder_name: &str) -> PathBuf {
        let folder = self.create_plain_folder(folder_name);
        fs::write(folder.join(".git"), "gitdir: /repo/.git/worktrees/x\n").expect(".git file");

        folder
    }

    /// Creates a folder with files but no Git metadata.
    fn create_plain_folder(&self, folder_name: &str) -> PathBuf {
        let folder = self.base.join(folder_name);
        fs::create_dir_all(folder.join("target/debug")).expect("folder");
        fs::write(folder.join("target/debug/artifact"), "binary").expect("artifact");

        folder
    }

    /// Builds services that use the real filesystem under the fixture base.
    fn services(&self, git_client: MockGitClient) -> AppServices {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        AppServices::new_with_agent_clis(
            self.base.clone(),
            Arc::new(crate::infra::clock::RealClock),
            event_tx,
            crate::app::service::AppServiceDeps {
                session_run_factory: Arc::new(
                    crate::app::service::test_support::TestSessionRunFactory::default(),
                ),
                runtime_config: ag_worker::RuntimeConfig::with_app_server(
                    crate::test_support::mock_app_server(),
                ),
                available_agent_kinds: AgentKind::ALL.to_vec(),
                clipboard_image_client_override: None,
                fs_client: Arc::new(RealFsClient),
                git_client: Arc::new(git_client),
                run_client_override: None,
                agent_profile_catalog_client_override: None,
                repositories: self.db.clone(),
                review_request_client: Arc::new(ag_forge::MockReviewRequestClient::new()),
            },
            AgentCliInfo::from_kinds(AgentKind::ALL),
        )
    }

    /// Captures, spawns, and waits for the sweep as startup does.
    async fn sweep(&self, git_client: MockGitClient) {
        let services = self.services(git_client);
        let sweep = ArchivedWorktreeSweep::capture(&self.db).await;

        sweep.spawn(&services);
        services.wait_for_cleanup_tasks(None).await;
    }
}

/// Expects Git cleanup for each listed branch and records deleted branches.
fn git_client_expecting_branches(branches: usize) -> (MockGitClient, Arc<Mutex<Vec<String>>>) {
    let deleted_branches = Arc::new(Mutex::new(Vec::new()));
    let mut git_client = MockGitClient::new();
    git_client
        .expect_main_repo_root()
        .times(branches)
        .returning(|_| Box::pin(async { Ok(PathBuf::from(REPO_ROOT)) }));
    git_client
        .expect_prune_worktrees()
        .times(branches)
        .withf(|repo_root| repo_root == Path::new(REPO_ROOT))
        .returning(|_| Box::pin(async { Ok(()) }));
    git_client
        .expect_delete_branch()
        .times(branches)
        .returning({
            let deleted_branches = Arc::clone(&deleted_branches);

            move |_, branch| {
                deleted_branches.lock().expect("branches").push(branch);

                Box::pin(async { Ok(()) })
            }
        });

    (git_client, deleted_branches)
}

#[tokio::test]
async fn sweep_removes_only_folders_of_archived_sessions_under_the_base() {
    // Arrange
    let fixture = SweepFixture::new().await;
    let checkout = fixture.root.path().join("project");
    fs::create_dir_all(checkout.join(".git")).expect("project checkout");
    let checkout_project_id = fixture
        .db
        .projects()
        .upsert_project(&checkout.to_string_lossy(), Some("main".to_string()))
        .await
        .expect("checkout project");
    fixture.insert_session("done0001-aaaa", Status::Done).await;
    fixture
        .insert_session("cancel01-bbbb", Status::Canceled)
        .await;
    fixture.insert_session("norepo01-cccc", Status::Done).await;
    fixture
        .insert_session_in_project("checkout-dddd", Status::Canceled, checkout_project_id)
        .await;
    let removed = [
        fixture.create_worktree("done0001"),
        fixture.create_worktree("cancel01"),
        fixture.create_plain_folder("norepo01"),
        fixture.create_plain_folder("checkout"),
        fixture.create_plain_folder("done0001-aaaa/.agentty"),
    ];
    let mut kept = Vec::new();
    for (index, status) in [
        Status::Draft,
        Status::InProgress,
        Status::Review,
        Status::AgentReview,
        Status::Watching,
        Status::Question,
        Status::Queued,
        Status::Rebasing,
        Status::Merging,
        Status::Merged,
    ]
    .into_iter()
    .enumerate()
    {
        let session_id = format!("keep{index:04}-eeee");
        fixture.insert_session(&session_id, status).await;
        kept.push(fixture.create_worktree(&session_id[..8]));
        kept.push(fixture.create_plain_folder(&format!("{session_id}/.agentty")));
    }
    fixture.insert_session("share001-ffff", Status::Done).await;
    fixture
        .insert_session("share001-gggg", Status::Review)
        .await;
    kept.push(fixture.create_worktree("share001"));
    kept.push(fixture.create_worktree("orphan01"));
    let outside = fixture.root.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::write(outside.join("keep"), "outside").expect("outside file");
    fixture.insert_session("link0001-hhhh", Status::Done).await;
    std::os::unix::fs::symlink(&outside, fixture.base.join("link0001")).expect("symlink");
    fixture.insert_session("../outside", Status::Done).await;
    let (git_client, deleted_branches) = git_client_expecting_branches(3);

    // Act
    fixture.sweep(git_client).await;

    // Assert
    for path in &removed {
        assert!(!path.exists(), "{} should be removed", path.display());
    }
    for path in &kept {
        assert!(
            path.join("target/debug/artifact").exists(),
            "{} kept",
            path.display()
        );
    }
    assert!(fixture.base.join("link0001").is_symlink());
    assert!(outside.join("keep").exists());
    let mut deleted_branches = deleted_branches.lock().expect("branches").clone();
    deleted_branches.sort();
    assert_eq!(
        deleted_branches,
        ["wt/cancel01", "wt/checkout", "wt/done0001"]
    );
}

#[tokio::test]
async fn sweep_continues_after_a_failure_and_rechecks_each_row() {
    // Arrange
    let fixture = SweepFixture::new().await;
    for session_id in [
        "fail0001-aaaa",
        "next0001-bbbb",
        "gone0001-cccc",
        "reus0001-dddd",
    ] {
        fixture.insert_session(session_id, Status::Done).await;
    }
    let failing = fixture.create_worktree("fail0001");
    let locked_legacy_folder = fixture.create_plain_folder("fail0001-aaaa/locked");
    fs::set_permissions(&locked_legacy_folder, fs::Permissions::from_mode(0o500))
        .expect("lock legacy folder");
    let next = fixture.create_worktree("next0001");
    let deleted = fixture.create_worktree("gone0001");
    let reused = fixture.create_worktree("reus0001");
    let services_git_client = {
        let mut git_client = MockGitClient::new();
        git_client
            .expect_main_repo_root()
            .times(2)
            .returning(|_| Box::pin(async { Ok(PathBuf::from(REPO_ROOT)) }));
        git_client.expect_prune_worktrees().times(2).returning(|_| {
            Box::pin(async { Err(GitError::OutputParse("prune failed".to_string())) })
        });
        git_client
            .expect_delete_branch()
            .times(2)
            .returning(|_, branch| {
                Box::pin(async move {
                    if branch == "wt/fail0001" {
                        return Err(GitError::OutputParse("delete failed".to_string()));
                    }

                    Ok(())
                })
            });

        git_client
    };
    let services = fixture.services(services_git_client);
    let sweep = ArchivedWorktreeSweep::capture(&fixture.db).await;
    fixture
        .db
        .sessions()
        .delete_session("gone0001-cccc")
        .await
        .expect("delete session");
    fixture.insert_session("reus0001-eeee", Status::Draft).await;

    // Act
    sweep.run(&services).await;
    let legacy_folder_survived = locked_legacy_folder.exists();
    fs::set_permissions(&locked_legacy_folder, fs::Permissions::from_mode(0o700))
        .expect("unlock legacy folder");

    // Assert
    assert!(!failing.exists());
    assert!(legacy_folder_survived, "the legacy folder removal fails");
    assert!(!next.exists());
    assert!(deleted.exists(), "a deleted row leaves cleanup to deletion");
    assert!(reused.exists(), "a folder claimed after capture is kept");
}

#[tokio::test]
async fn sweep_keeps_folders_when_sessions_cannot_be_reloaded() {
    // Arrange
    let fixture = SweepFixture::new().await;
    fixture.insert_session("done0001-aaaa", Status::Done).await;
    let folder = fixture.create_worktree("done0001");
    let services = fixture.services(MockGitClient::new());
    let sweep = ArchivedWorktreeSweep::capture(&fixture.db).await;
    fixture.pool.close().await;

    // Act
    let capture_after_failure = ArchivedWorktreeSweep::capture(&fixture.db).await;
    sweep.run(&services).await;

    // Assert
    assert!(capture_after_failure.candidates.is_empty());
    assert!(folder.exists());
}

#[tokio::test]
async fn sweep_does_nothing_when_nothing_is_left_behind() {
    // Arrange
    let fixture = SweepFixture::new().await;
    fixture
        .insert_session("review01-aaaa", Status::Review)
        .await;
    let capture_without_archived = ArchivedWorktreeSweep::capture(&fixture.db).await;
    fixture.insert_session("done0001-bbbb", Status::Done).await;
    let services = fixture.services(MockGitClient::new());
    let sweep = ArchivedWorktreeSweep::capture(&fixture.db).await;
    // Every reload happens under the lane, so holding it proves that a
    // candidate without folders is skipped before any session reload.
    let lane = services.worktree_cleanup_lane();
    let _held_lane = lane.lock().await;

    // Act
    let finished = tokio::time::timeout(Duration::from_secs(5), sweep.run(&services)).await;

    // Assert
    assert!(capture_without_archived.candidates.is_empty());
    assert!(
        finished.is_ok(),
        "the sweep must not take the lane or reload rows"
    );
    assert_eq!(fs::read_dir(&fixture.base).expect("base").count(), 0);
}
