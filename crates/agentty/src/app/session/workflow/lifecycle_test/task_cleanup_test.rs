//! Session deletion and cancellation tests for task history ownership and
//! graph runtime cleanup.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_forge::MockReviewRequestClient;
use ag_git::{GitError, MockGitClient};
use ag_session::{
    AgentKind, AgentModel, AgentSelection, CreateTaskRequest, DurableMessageVisibility,
    SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentInput, TaskAgentRole, TaskWorktreeId,
};

use super::support::{
    database_with_session_and_pool, session_manager_with_one_session, test_services_with_fs_client,
    test_session,
};
use crate::app::{AppServices, ProjectManager, SessionManager};
use crate::domain::session::Status;
use crate::infra::clock::RealClock;
use crate::infra::db::{self, AppRepositories};
use crate::infra::fs::{FsError, MockFsClient};
use crate::test_support::enable_tracing_log_fallback;

#[tokio::test]
async fn deletion_retains_task_history_ownership_before_cascade() {
    // Arrange
    for deferred in [false, true] {
        let session = test_session("", Status::Review, None, "");
        let (repositories, _pool) = database_with_session_and_pool(&session).await;
        seed_task(&repositories).await;
        let runtime_ids = SessionManager::load_task_runtime_ids(&repositories, &session.id)
            .await
            .expect("owned resources");
        let (services, removed) = cleanup_services(&repositories);
        let projects = ProjectManager::new(
            1,
            "/tmp/project".into(),
            None,
            None,
            Vec::new(),
            "/tmp/project".into(),
        );
        let mut manager = session_manager_with_one_session(session);
        manager.select_session_index(Some(0));

        // Act
        if deferred {
            manager
                .delete_selected_session_deferred_cleanup(&projects, &services)
                .await;
        } else {
            manager.delete_selected_session(&projects, &services).await;
        }
        // Assert: deferred cleanup can run after the rows have disappeared.
        assert!(manager.sessions().is_empty());
        assert_eq!(runtime_ids.len(), 1);
        assert_eq!(
            repositories
                .tasks()
                .load_agent_context("arbitrary/task")
                .await
                .expect("cascade"),
            None
        );
        services.wait_for_cleanup_tasks(None).await;
        assert!(
            removed
                .lock()
                .expect("removed paths")
                .iter()
                .any(|path| path.ends_with(&runtime_ids[0]))
        );
    }
}

#[tokio::test]
async fn deletion_continues_when_record_and_staged_draft_removal_fail() {
    // Arrange
    let session = test_session("", Status::Review, None, "");
    let (repositories, pool) = database_with_session_and_pool(&session).await;
    sqlx::query(
        "CREATE TRIGGER reject_session_delete BEFORE DELETE ON session BEGIN SELECT RAISE(FAIL, \
         'delete rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("failure trigger");
    let removal_attempts = Arc::new(Mutex::new(Vec::new()));
    let recorded_attempts = Arc::clone(&removal_attempts);
    let mut filesystem = MockFsClient::new();
    filesystem.expect_is_dir().return_const(true);
    filesystem.expect_remove_dir_all().returning(move |path| {
        recorded_attempts
            .lock()
            .expect("removal attempts")
            .push(path);
        Box::pin(async { Err(FsError::Io(std::io::Error::other("directory busy"))) })
    });
    let services = test_services_with_fs_client(
        &repositories,
        Arc::new(RealClock),
        Arc::new(filesystem),
        Arc::new(MockGitClient::new()),
        Arc::new(MockReviewRequestClient::new()),
    );
    let projects = ProjectManager::new(
        1,
        "/tmp/project".into(),
        None,
        None,
        Vec::new(),
        "/tmp/project".into(),
    );
    let mut manager = session_manager_with_one_session(session);
    manager.select_session_index(Some(0));
    enable_tracing_log_fallback();

    // Act
    manager.delete_selected_session(&projects, &services).await;

    // Assert: the in-memory session is gone, while the rejected record and
    // the staged draft root were both still attempted.
    assert!(manager.sessions().is_empty());
    assert!(
        repositories
            .sessions()
            .load_session("session-id")
            .await
            .expect("session lookup")
            .is_some()
    );
    assert!(
        removal_attempts
            .lock()
            .expect("removal attempts")
            .iter()
            .any(|path| path == &PathBuf::from("/tmp/agentty-tests/session-id"))
    );
}

#[tokio::test]
async fn graph_runtime_cleanup_uses_the_durable_host_uuid_before_cascade() {
    for context_id in ["aaaaaaaa-0000-0000-0000-000000000001", "invalid-context"] {
        // Arrange
        let session = test_session("", Status::Review, None, "");
        let (repositories, _pool) = database_with_session_and_pool(&session).await;
        let mut graph = crate::app::execution_graph::tests::graph();
        graph.session_id = session.id.clone();
        graph.nodes.truncate(1);
        graph.nodes[0].env_id = ag_session::EnvId::SessionWorktree(session.id.clone());
        repositories
            .graphs()
            .save_graph(&graph, None)
            .await
            .expect("graph");
        repositories
            .graph_dispatch()
            .claim(
                "graph",
                graph.revision,
                "first",
                context_id,
                std::path::Path::new("workspace"),
            )
            .await
            .expect("claim");

        // Act
        let result = SessionManager::load_task_runtime_ids(&repositories, &session.id).await;
        if context_id == "invalid-context" {
            // Assert: corrupt identities must fail before cleanup selects
            // paths.
            assert!(
                result
                    .expect_err("invalid runtime identity")
                    .to_string()
                    .contains("UUID")
            );
            assert!(
                repositories
                    .sessions()
                    .load_session(&session.id)
                    .await
                    .expect("retained session")
                    .is_some()
            );
            continue;
        }
        let ids = result.expect("runtime ownership");
        repositories
            .sessions()
            .delete_session(&session.id)
            .await
            .expect("cascade");

        // Assert
        assert_eq!(ids, ["task-aaaaaaaa-0000-0000-0000-000000000001"]);
        assert_eq!(
            repositories
                .graphs()
                .list_task_context_ids(&session.id)
                .await
                .expect("cleared")
                .len(),
            0
        );
    }
}

#[tokio::test]
async fn corrupted_task_ownership_blocks_deletion_without_removing_state() {
    // Arrange
    for corruption in ["uuid", "role", "database"] {
        let session = test_session("", Status::Review, None, "");
        let (repositories, pool) = database_with_session_and_pool(&session).await;
        seed_task(&repositories).await;
        match corruption {
            "uuid" => {
                sqlx::query("UPDATE task_agent_context SET id = '../unowned'")
                    .execute(&pool)
                    .await
                    .expect("corrupt identity");
            }
            "role" => {
                sqlx::raw_sql(
                    "PRAGMA ignore_check_constraints = ON; UPDATE task_agent_context SET role = \
                     'unknown';",
                )
                .execute(&pool)
                .await
                .expect("corrupt role");
            }
            _ => pool.close().await,
        }
        let (services, removed) = cleanup_services(&repositories);
        let projects = ProjectManager::new(
            1,
            "/tmp/project".into(),
            None,
            None,
            Vec::new(),
            "/tmp/project".into(),
        );
        let mut manager = session_manager_with_one_session(session);
        manager.select_session_index(Some(0));

        // Act
        manager.delete_selected_session(&projects, &services).await;

        // Assert
        assert_eq!(manager.sessions().len(), 1);
        assert!(removed.lock().expect("removed paths").is_empty());
    }
}

#[tokio::test]
async fn canceled_session_cleans_task_histories_and_reports_unreadable_ownership() {
    // Arrange
    for unreadable in [false, true] {
        let session = test_session("", Status::Canceled, None, "");
        let (repositories, pool) = database_with_session_and_pool(&session).await;
        seed_task(&repositories).await;
        let runtime_ids = SessionManager::load_task_runtime_ids(&repositories, &session.id)
            .await
            .expect("ownership");
        if unreadable {
            pool.close().await;
        }
        let (services, removed) = cleanup_services(&repositories);

        // Act
        SessionManager::spawn_canceled_session_cleanup(
            &services,
            session.folder,
            "branch".to_string(),
            false,
            session.id.to_string(),
        );
        services.wait_for_cleanup_tasks(None).await;

        // Assert
        assert_eq!(
            removed
                .lock()
                .expect("removed paths")
                .iter()
                .any(|path| path.ends_with(&runtime_ids[0])),
            !unreadable
        );
    }
}

#[tokio::test]
async fn deletion_removes_task_worktrees_read_before_cascade() {
    for deferred in [false, true] {
        // Arrange
        let session = test_session("", Status::Review, None, "");
        let (repositories, _pool) = database_with_session_and_pool(&session).await;
        reserve_task_worktree(&repositories).await;
        let cleanup = GitCleanup::new(&repositories, None);
        let projects = ProjectManager::new(
            1,
            "/tmp/project".into(),
            Some("main".to_string()),
            None,
            Vec::new(),
            "/tmp/project".into(),
        );
        let mut manager = session_manager_with_one_session(session);
        manager.select_session_index(Some(0));

        // Act
        if deferred {
            manager
                .delete_selected_session_deferred_cleanup(&projects, &cleanup.services)
                .await;
        } else {
            manager
                .delete_selected_session(&projects, &cleanup.services)
                .await;
        }
        cleanup.services.wait_for_cleanup_tasks(None).await;

        // Assert
        assert!(manager.sessions().is_empty());
        assert!(
            cleanup
                .removed()
                .contains(&PathBuf::from(TASK_WORKTREE_FOLDER))
        );
        assert_eq!(
            cleanup.deleted_branches(),
            ["wt/session-", TASK_WORKTREE_BRANCH]
        );
        assert_eq!(
            task_worktree_states(&repositories).await,
            Vec::<db::TaskWorktreeState>::new()
        );
    }
}

#[tokio::test]
async fn deletion_stops_when_task_worktrees_cannot_be_read() {
    // Arrange
    let session = test_session("", Status::Review, None, "");
    let (repositories, pool) = database_with_session_and_pool(&session).await;
    sqlx::query("DROP TABLE task_worktree")
        .execute(&pool)
        .await
        .expect("unreadable task worktrees");
    let cleanup = GitCleanup::new(&repositories, None);
    let projects = ProjectManager::new(
        1,
        "/tmp/project".into(),
        None,
        None,
        Vec::new(),
        "/tmp/project".into(),
    );
    let mut manager = session_manager_with_one_session(session);
    manager.select_session_index(Some(0));

    // Act
    manager
        .delete_selected_session(&projects, &cleanup.services)
        .await;

    // Assert
    assert_eq!(manager.sessions().len(), 1);
    assert_eq!(cleanup.removed(), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn canceled_session_removes_task_worktrees_from_the_session_repository() {
    for has_worktree in [false, true] {
        // Arrange
        let session = test_session("", Status::Canceled, None, "");
        let (repositories, _pool) = database_with_session_and_pool(&session).await;
        reserve_task_worktree(&repositories).await;
        let cleanup = GitCleanup::new(&repositories, None);

        // Act
        SessionManager::spawn_canceled_session_cleanup(
            &cleanup.services,
            session.folder,
            "wt/session-".to_string(),
            has_worktree,
            session.id.to_string(),
        );
        cleanup.services.wait_for_cleanup_tasks(None).await;

        // Assert: without a session worktree, the root comes from the
        // session's project.
        assert!(
            cleanup
                .removed()
                .contains(&PathBuf::from(TASK_WORKTREE_FOLDER))
        );
        let mut expected_branches = vec![TASK_WORKTREE_BRANCH];
        if has_worktree {
            expected_branches.push("wt/session-");
        }
        assert_eq!(cleanup.deleted_branches(), expected_branches);
        let expected_roots = if has_worktree {
            vec![PathBuf::from("/tmp/session")]
        } else {
            Vec::new()
        };
        assert_eq!(cleanup.resolved_roots(), expected_roots);
        assert_eq!(
            task_worktree_states(&repositories).await,
            Vec::<db::TaskWorktreeState>::new()
        );
    }
}

#[tokio::test]
async fn canceled_session_keeps_task_worktree_records_it_could_not_remove() {
    for failure in ["directory", "record update", "record deletion"] {
        // Arrange
        let session = test_session("", Status::Canceled, None, "");
        let (repositories, pool) = database_with_session_and_pool(&session).await;
        reserve_task_worktree(&repositories).await;
        let failed_folder = match failure {
            "directory" => Some(PathBuf::from(TASK_WORKTREE_FOLDER)),
            "record update" => {
                reject_task_worktree_writes(&pool, false).await;
                None
            }
            _ => {
                reject_task_worktree_writes(&pool, true).await;
                None
            }
        };
        let cleanup = GitCleanup::new(&repositories, failed_folder);
        enable_tracing_log_fallback();

        // Act
        SessionManager::spawn_canceled_session_cleanup(
            &cleanup.services,
            session.folder,
            "wt/session-".to_string(),
            false,
            session.id.to_string(),
        );
        cleanup.services.wait_for_cleanup_tasks(None).await;

        // Assert
        let expected_state = if failure == "record update" {
            db::TaskWorktreeState::Creating
        } else {
            db::TaskWorktreeState::Removing
        };
        assert_eq!(
            task_worktree_states(&repositories).await,
            [expected_state],
            "{failure}"
        );
        assert_eq!(
            cleanup
                .removed()
                .contains(&PathBuf::from(TASK_WORKTREE_FOLDER)),
            failure != "record update",
            "{failure}"
        );
    }
}

#[tokio::test]
async fn canceled_session_finishes_task_worktree_removal_when_resources_are_gone() {
    for case in [
        "already removed",
        "branch remains",
        "prune fails",
        "project repository",
        "repository unknown",
    ] {
        // Arrange: the directory is already gone and the branch deletion
        // fails; only a branch that still resolves, a Git worktree record
        // that cannot be pruned, or an unknown repository keeps the record.
        // The project names the repository when the task directory cannot.
        let session = test_session("", Status::Canceled, None, "");
        let (repositories, _pool) = database_with_session_and_pool(&session).await;
        reserve_task_worktree(&repositories).await;
        let mut filesystem = MockFsClient::new();
        filesystem.expect_is_dir().return_const(false);
        filesystem.expect_remove_dir_all().returning(|_| {
            Box::pin(async { Err(FsError::Io(std::io::ErrorKind::NotFound.into())) })
        });
        let mut git = MockGitClient::new();
        git.expect_find_git_repo_root().returning(move |_| {
            Box::pin(async move { (case == "project repository").then(|| PathBuf::from("/repo")) })
        });
        git.expect_main_repo_root().returning(move |_| {
            Box::pin(async move {
                if matches!(case, "project repository" | "repository unknown") {
                    return Err(GitError::OutputParse("not a repository".to_string()));
                }

                Ok(PathBuf::from("/repo"))
            })
        });
        git.expect_prune_worktrees().returning(move |_| {
            Box::pin(async move {
                if case == "prune fails" {
                    return Err(GitError::OutputParse("worktree locked".to_string()));
                }

                Ok(())
            })
        });
        git.expect_delete_branch().returning(|_, _| {
            Box::pin(async { Err(GitError::OutputParse("branch not found".to_string())) })
        });
        git.expect_ref_hash().returning(move |_, _| {
            Box::pin(async move {
                if case == "branch remains" {
                    return Ok("abc123".to_string());
                }

                Err(GitError::OutputParse("unknown revision".to_string()))
            })
        });
        let services = test_services_with_fs_client(
            &repositories,
            Arc::new(RealClock),
            Arc::new(filesystem),
            Arc::new(git),
            Arc::new(MockReviewRequestClient::new()),
        );
        enable_tracing_log_fallback();

        // Act
        SessionManager::spawn_canceled_session_cleanup(
            &services,
            session.folder,
            "wt/session-".to_string(),
            false,
            session.id.to_string(),
        );
        services.wait_for_cleanup_tasks(None).await;

        // Assert
        let expected_states = if matches!(case, "already removed" | "project repository") {
            Vec::new()
        } else {
            vec![db::TaskWorktreeState::Removing]
        };
        assert_eq!(
            task_worktree_states(&repositories).await,
            expected_states,
            "{case}"
        );
    }
}

/// Directory of the task worktree [`reserve_task_worktree`] records, under
/// the test services' worktree base.
const TASK_WORKTREE_FOLDER: &str = "/tmp/agentty-tests/session--writer-a";

/// Branch of the task worktree [`reserve_task_worktree`] records.
const TASK_WORKTREE_BRANCH: &str = "wt/session--writer-a";

/// Records one task worktree for the fixture session.
async fn reserve_task_worktree(repositories: &AppRepositories) {
    repositories
        .task_worktrees()
        .reserve_task_worktree(
            &TaskWorktreeId {
                session_id: "session-id".into(),
                worktree_id: "writer-a1".to_string(),
            },
            "base-commit",
        )
        .await
        .expect("task worktree");
}

/// Makes task worktree updates, or deletions when `deletion` is set, fail
/// the way a busy or corrupt database would.
async fn reject_task_worktree_writes(pool: &sqlx::SqlitePool, deletion: bool) {
    let trigger = if deletion {
        "CREATE TRIGGER reject_task_worktree_delete BEFORE DELETE ON task_worktree BEGIN SELECT \
         RAISE(FAIL, 'write rejected'); END"
    } else {
        "CREATE TRIGGER reject_task_worktree_update BEFORE UPDATE ON task_worktree BEGIN SELECT \
         RAISE(FAIL, 'write rejected'); END"
    };
    sqlx::query(trigger)
        .execute(pool)
        .await
        .expect("failure trigger");
}

async fn task_worktree_states(repositories: &AppRepositories) -> Vec<db::TaskWorktreeState> {
    repositories
        .task_worktrees()
        .list_task_worktrees("session-id")
        .await
        .expect("task worktrees")
        .into_iter()
        .map(|row| row.state)
        .collect()
}

/// Services whose Git boundary resolves every repository to `/repo` and
/// records what cleanup removes.
struct GitCleanup {
    deleted_branches: Arc<Mutex<Vec<String>>>,
    removed: Arc<Mutex<Vec<PathBuf>>>,
    resolved_roots: Arc<Mutex<Vec<PathBuf>>>,
    services: AppServices,
}

impl GitCleanup {
    /// Builds the services; removing `failed_folder` fails.
    fn new(repositories: &AppRepositories, failed_folder: Option<PathBuf>) -> Self {
        let deleted_branches = Arc::new(Mutex::new(Vec::new()));
        let removed = Arc::new(Mutex::new(Vec::new()));
        let resolved_roots = Arc::new(Mutex::new(Vec::new()));
        let mut filesystem = MockFsClient::new();
        filesystem.expect_is_dir().return_const(false);
        let recorded_removals = Arc::clone(&removed);
        filesystem.expect_remove_dir_all().returning(move |path| {
            let fails = failed_folder.as_ref() == Some(&path);
            recorded_removals.lock().expect("removed paths").push(path);
            Box::pin(async move {
                if fails {
                    return Err(FsError::Io(std::io::Error::other("directory busy")));
                }

                Ok(())
            })
        });
        let mut git = MockGitClient::new();
        git.expect_find_git_repo_root()
            .returning(|_| Box::pin(async { Some(PathBuf::from("/repo")) }));
        let recorded_roots = Arc::clone(&resolved_roots);
        git.expect_main_repo_root().returning(move |path| {
            recorded_roots.lock().expect("resolved roots").push(path);
            Box::pin(async { Ok(PathBuf::from("/repo")) })
        });
        git.expect_prune_worktrees()
            .returning(|_| Box::pin(async { Ok(()) }));
        let recorded_branches = Arc::clone(&deleted_branches);
        git.expect_delete_branch().returning(move |_, branch| {
            recorded_branches.lock().expect("branches").push(branch);
            Box::pin(async { Ok(()) })
        });
        let services = test_services_with_fs_client(
            repositories,
            Arc::new(RealClock),
            Arc::new(filesystem),
            Arc::new(git),
            Arc::new(MockReviewRequestClient::new()),
        );

        Self {
            deleted_branches,
            removed,
            resolved_roots,
            services,
        }
    }

    fn deleted_branches(&self) -> Vec<String> {
        self.deleted_branches.lock().expect("branches").clone()
    }

    fn removed(&self) -> Vec<PathBuf> {
        self.removed.lock().expect("removed paths").clone()
    }

    fn resolved_roots(&self) -> Vec<PathBuf> {
        self.resolved_roots.lock().expect("resolved roots").clone()
    }
}

async fn seed_task(repositories: &AppRepositories) {
    repositories
        .tasks()
        .create_task(
            "session-id",
            &CreateTaskRequest {
                objective: "ordinary".to_string(),
                task_id: "ordinary".to_string(),
            },
        )
        .await
        .expect("ordinary task");
    repositories
        .tasks()
        .claim_agent_attempt(
            "session-id",
            &SubmitAgentTaskRequest {
                assignment: TaskAgentAssignment {
                    agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
                    role: TaskAgentRole::Planner,
                    definition: ag_session::AgentDefinition::bundled("planner")
                        .expect("bundled agent"),
                },
                attempt_id: "plan".to_string(),
                input: TaskAgentInput {
                    attachments: Vec::new(),
                    accepted_plan: None,
                    clarification: None,
                    user_request: "request".to_string(),
                },
                objective: "plan".to_string(),
                task_id: "arbitrary/task".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
            "reply",
        )
        .await
        .expect("role task");
}

fn cleanup_services(repositories: &AppRepositories) -> (AppServices, Arc<Mutex<Vec<PathBuf>>>) {
    let removed = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&removed);
    let mut filesystem = MockFsClient::new();
    filesystem.expect_is_dir().return_const(false);
    filesystem.expect_remove_dir_all().returning(move |path| {
        recorded.lock().expect("removed paths").push(path);
        Box::pin(async { Ok(()) })
    });
    let services = test_services_with_fs_client(
        repositories,
        Arc::new(RealClock),
        Arc::new(filesystem),
        Arc::new(MockGitClient::new()),
        Arc::new(MockReviewRequestClient::new()),
    );

    (services, removed)
}
