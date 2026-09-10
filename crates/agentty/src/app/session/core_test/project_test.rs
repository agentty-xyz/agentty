use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;

use super::support::{new_test_app, new_test_app_with_db, test_session_manager};
use crate::app::Tab;
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn resource_refresh_projects_only_current_worker_pids() {
    // Arrange
    let mut manager = test_session_manager("tracked", None);
    let mut client = crate::infra::resource::MockResourceClient::new();
    client.expect_sample().times(1).returning(|_| {
        Some(vec![crate::infra::resource::ProcessSample {
            host_cpu_temperature_celsius: None,
            is_alive: true,
            identity: Some(crate::infra::process_identity::ProcessIdentity(1_000_001)),
            pid: 42,
            parent_pid: 1,
            resources: crate::domain::resource::SessionResources {
                process_count: 1,
                cpu_percent: 5.0,
                resident_memory_kib: 1024,
            },
        }])
    });
    manager.resources = crate::app::session::resource::ResourceMonitor::new(Arc::new(client));
    let pid = Arc::clone(&manager.state.handle("tracked").expect("handles").child_pid);
    *pid.lock().expect("pid") = Some(42);

    // Act
    manager.refresh_resources().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !manager.refresh_resources().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("sample completion");

    // Assert
    assert_eq!(
        manager.render_parts().session_resources["tracked"].process_count,
        1
    );
    *pid.lock().expect("pid") = None;
    assert!(manager.refresh_resources().await);
    assert!(manager.render_parts().session_resources.is_empty());
}

#[tokio::test]
async fn test_active_project_id_getter() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let app = new_test_app(dir.path().to_path_buf()).await;

    // Act & Assert
    assert!(app.active_project_id() > 0);
}

#[tokio::test]
async fn test_next_tab_includes_tasks_when_active_project_has_roadmap() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let database = AppRepositories::in_memory().await.expect("db should open");
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        None,
        database,
    )
    .await;

    // Act & Assert
    assert_eq!(app.tabs.current(), Tab::Projects);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Sessions);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Settings);
    app.next_tab();
    assert_eq!(app.tabs.current(), Tab::Projects);
}
