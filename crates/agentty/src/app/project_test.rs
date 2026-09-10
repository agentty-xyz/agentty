use std::path::{Path, PathBuf};

use super::ProjectManager;
use crate::domain::project::{Project, ProjectListItem};

#[test]
fn test_next_project_wraps_to_first_row() {
    // Arrange
    let mut manager = project_manager_fixture();
    manager.table_state.select(Some(1));

    // Act
    manager.next_project();

    // Assert
    assert_eq!(manager.table_state.selected(), Some(0));
}

#[test]
fn test_previous_project_wraps_to_last_row() {
    // Arrange
    let mut manager = project_manager_fixture();
    manager.table_state.select(Some(0));

    // Act
    manager.previous_project();

    // Assert
    assert_eq!(manager.table_state.selected(), Some(1));
}

fn project_manager_fixture() -> ProjectManager {
    let project_items = vec![
        ProjectListItem {
            active_session_count: 0,
            input_tokens: 0,
            last_session_updated_at: Some(10),
            output_tokens: 0,
            project: Project {
                created_at: 1,
                display_name: Some("agentty".to_string()),
                git_branch: Some("main".to_string()),
                id: 1,
                is_favorite: false,
                last_opened_at: Some(20),
                path: PathBuf::from("/tmp/agentty"),
                updated_at: 2,
            },
            session_count: 3,
        },
        ProjectListItem {
            active_session_count: 0,
            input_tokens: 0,
            last_session_updated_at: Some(11),
            output_tokens: 0,
            project: Project {
                created_at: 1,
                display_name: Some("service".to_string()),
                git_branch: Some("main".to_string()),
                id: 2,
                is_favorite: true,
                last_opened_at: Some(21),
                path: PathBuf::from("/tmp/service"),
                updated_at: 2,
            },
            session_count: 2,
        },
    ];

    ProjectManager::new(
        1,
        "agentty".to_string(),
        Some("main".to_string()),
        Some("origin/main".to_string()),
        project_items,
        PathBuf::from("/tmp/agentty"),
    )
}

#[test]
fn test_update_active_project_context_replaces_upstream_reference() {
    // Arrange
    let mut manager = project_manager_fixture();

    // Act
    manager.update_active_project_context(
        2,
        "service".to_string(),
        Some("feature/footer".to_string()),
        Some("origin/feature/footer".to_string()),
        PathBuf::from("/tmp/service"),
    );

    // Assert
    assert_eq!(manager.active_project_id(), 2);
    assert_eq!(manager.project_name(), "service");
    assert_eq!(manager.git_branch(), Some("feature/footer"));
    assert_eq!(manager.git_upstream_ref(), Some("origin/feature/footer"));
    assert_eq!(manager.working_dir(), Path::new("/tmp/service"));
    assert_eq!(manager.git_status(), None);
}
