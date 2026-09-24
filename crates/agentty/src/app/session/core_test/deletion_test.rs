use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{AgentRequestKind, OneShotRequest, PermissionMode, ReasoningLevel, SpeedMode};
use ag_worker::test_support::MockAppServerClient;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use super::support::{add_manual_session, wait_for_path_absent};
use crate::app::App;
use crate::domain::session::Status;
use crate::infra::db::Database;

#[tokio::test]
async fn deleting_or_canceling_sessions_waits_for_utilities_before_removing_resources() {
    for (deferred, cancel) in [(false, false), (true, false), (false, true)] {
        // Arrange
        let directory = tempdir().expect("workspace");
        let started = CancellationToken::new();
        let stopped = CancellationToken::new();
        let finish = CancellationToken::new();
        let provider = blocking_provider(&started, &stopped, &finish);
        let db = Database::open_in_memory().await.expect("database");
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(provider));
        let mut app = App::new_with_clients(
            directory.path().into(),
            directory.path().into(),
            None,
            db.clone(),
            clients,
        )
        .await
        .expect("app");
        db.sessions()
            .insert_session(
                "deletion",
                "gpt-5.6",
                "main",
                "Review",
                app.projects.active_project_id(),
            )
            .await
            .expect("session");
        add_manual_session(&mut app, directory.path(), "deletion", "Title");
        crate::test_support::set_session_status_for_test(&mut app, "deletion", Status::Review);
        let folder = app.sessions.sessions()[0].folder.clone();
        let client = app.services.session_run_client("deletion");
        let request = OneShotRequest {
            execution_policy: ag_contracts::ExecutionPolicy::default(),
            child_pid: None,
            folder: folder.clone(),
            harness: "codex".into(),
            model: "model".into(),
            permission_mode: PermissionMode::AutoEdit,
            prompt: "review".into(),
            provider_call_budget: None,
            reasoning_level: ReasoningLevel::default(),
            request_kind: AgentRequestKind::UtilityPrompt,
            speed_mode: SpeedMode::default(),
        };
        let late_request = request.clone();
        let late_client = client.clone();
        let run = tokio::spawn(async move { client.submit(request).await });
        started.cancelled().await;
        // Act
        let deletion = tokio::spawn(async move {
            if cancel {
                app.sessions
                    .cancel_session(&app.services, "deletion")
                    .await
                    .expect("cancel");
            } else if deferred {
                app.sessions
                    .delete_selected_session_deferred_cleanup(&app.projects, &app.services)
                    .await;
            } else {
                app.delete_selected_session().await;
            }
            app
        });
        stopped.cancelled().await;
        // Assert
        assert!(folder.exists());
        assert_eq!(
            db.sessions().load_sessions().await.expect("sessions").len(),
            1
        );
        let app = if cancel {
            let app = tokio::time::timeout(Duration::from_secs(2), deletion)
                .await
                .expect("foreground cancellation must not wait for the harness")
                .expect("cancel task");
            assert!(folder.exists(), "cleanup must still wait for the harness");
            finish.cancel();
            app
        } else {
            assert!(!deletion.is_finished());
            finish.cancel();
            deletion.await.expect("deletion")
        };
        run.await.expect("utility").expect_err("canceled");
        late_client
            .submit(late_request)
            .await
            .expect_err("deleted session cannot execute");
        app.services.wait_for_cleanup_tasks().await;
        wait_for_path_absent(&folder).await;
        assert_eq!(
            db.sessions().load_sessions().await.expect("sessions").len(),
            usize::from(cancel)
        );
        let status: String = sqlx::query_scalar("SELECT status FROM agent_run")
            .fetch_one(db.pool())
            .await
            .expect("terminal utility");
        assert_eq!(status, "canceled");
    }
}

fn blocking_provider(
    started: &CancellationToken,
    stopped: &CancellationToken,
    finish: &CancellationToken,
) -> MockAppServerClient {
    let mut provider = MockAppServerClient::new();
    provider.expect_run_isolated_turn().once().returning({
        let started = started.clone();
        let stopped = stopped.clone();
        let finish = finish.clone();
        move |_, _| {
            let started = started.clone();
            let stopped = stopped.clone();
            let finish = finish.clone();
            Box::pin(async move {
                started.cancel();
                stopped.cancelled().await;
                finish.cancelled().await;
                Err(ag_worker::test_support::AppServerError::Provider(
                    "stopped".into(),
                ))
            })
        }
    });
    provider.expect_shutdown_session().times(2).returning({
        let stopped = stopped.clone();
        move |_| {
            stopped.cancel();
            Box::pin(async {})
        }
    });

    provider
}
