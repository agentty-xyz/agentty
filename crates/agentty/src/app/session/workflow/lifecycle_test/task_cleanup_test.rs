use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_forge::MockReviewRequestClient;
use ag_git::MockGitClient;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, CreateTaskRequest,
    DurableMessageVisibility, SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentInput,
};

use super::support::{
    database_with_session_and_pool, session_manager_with_one_session, test_services_with_fs_client,
    test_session,
};
use crate::app::{AppServices, ProjectManager, SessionManager};
use crate::domain::session::Status;
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::fs::MockFsClient;

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
                    definition: BuiltinAgentDefinition::Planner,
                },
                attempt_id: "plan".to_string(),
                input: TaskAgentInput {
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
