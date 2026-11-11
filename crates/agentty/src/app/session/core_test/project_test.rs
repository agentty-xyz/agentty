use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;

use super::support::{new_test_app, new_test_app_with_db, test_session_manager};
use crate::app::Tab;
use crate::app::session::resource::ResourceMonitor;
use crate::domain::resource::SessionResources;
use crate::infra::db::AppRepositories;
use crate::infra::resource::{MockResourceClient, ProcessSample};

#[tokio::test]
async fn resource_refresh_projects_process_tree_totals() {
    // Arrange
    let mut manager = test_session_manager("tracked", None);
    let mut client = MockResourceClient::new();
    let totals = SessionResources {
        process_count: 1,
        cpu_percent: 5.0,
        resident_memory_kib: 1024,
    };
    client.expect_sample().times(1).returning(move || {
        Some(vec![ProcessSample {
            is_alive: true,
            pid: 42,
            parent_pid: 1,
            resources: totals,
        }])
    });
    manager.resources = ResourceMonitor::new(Arc::new(client), 42);

    // Act
    manager.refresh_resources().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !manager.refresh_resources().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("sample completion");
    let render_parts = manager.render_parts();

    // Assert
    assert_eq!(render_parts.process_resources, Some(totals));
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
