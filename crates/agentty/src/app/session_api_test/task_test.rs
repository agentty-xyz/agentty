use ag_session::{CreateTaskRequest, SessionError, SessionId, Task, TaskAttempt};

use crate::test_support::new_test_app;

#[tokio::test]
async fn task_planning_uses_persisted_sessions_without_starting_work() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let service = app.session_service();
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");

    for status in ["Draft", "InProgress", "Done"] {
        let session_id = SessionId::from(status);
        app.services
            .db()
            .sessions()
            .insert_session(&session_id, "gpt-5.6-sol", "main", status, project_id)
            .await
            .expect("persist session without loading a worker");
        let request = CreateTaskRequest {
            objective: "Plan without execution".to_string(),
            task_id: format!("task-{status}"),
        };
        let before = app
            .drive_session_request(service.get_session(&session_id))
            .await
            .expect("session");

        // Act
        app.drive_session_request(service.create_task(&session_id, request.clone()))
            .await
            .expect("plan");
        app.drive_session_request(service.create_task(&session_id, request.clone()))
            .await
            .expect("repeat");
        let tasks = app
            .drive_session_request(service.list_tasks(&session_id))
            .await
            .expect("list");
        let after = app
            .drive_session_request(service.get_session(&session_id))
            .await
            .expect("session");

        // Assert
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, request.task_id);
        assert_eq!(tasks[0].session_id, session_id);
        assert_eq!(tasks[0].objective, request.objective);
        assert_eq!(tasks[0].attempts, Vec::<TaskAttempt>::new());
        assert_eq!(tasks[0].completed_at, None);
        assert_eq!(before, after);
        assert!(app.sessions.session_for_id(&session_id).is_none());
    }

    // Act / Assert: persistence failures return through admission replies.
    let missing = SessionId::from("missing");
    let error = app
        .drive_session_request(service.create_task(
            &missing,
            CreateTaskRequest {
                objective: "Missing owner".to_string(),
                task_id: "missing-task".to_string(),
            },
        ))
        .await
        .expect_err("missing session");
    assert!(matches!(error, SessionError::Operation(_)));
    assert_eq!(
        app.drive_session_request(service.list_tasks(&missing))
            .await
            .expect("empty"),
        Vec::<Task>::new()
    );
}
