//! `ProjectManager` navigation, listed projects, and active and viewed
//! project context unit tests.

use std::path::{Path, PathBuf};

use super::{ProjectContext, ProjectManager};
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

#[test]
fn project_navigation_steps_back_starts_at_first_row_and_clears_empty_list() {
    // Arrange
    let next: fn(&mut ProjectManager) = ProjectManager::next_project;
    let previous: fn(&mut ProjectManager) = ProjectManager::previous_project;
    let cases = [
        (
            "previous from second row",
            previous,
            Some(1),
            false,
            Some(0),
        ),
        ("next without selection", next, None, false, Some(0)),
        ("previous without selection", previous, None, false, Some(0)),
        ("next in empty list", next, Some(0), true, None),
        ("previous in empty list", previous, Some(0), true, None),
    ];

    for (name, navigate, selected, is_empty, expected) in cases {
        let mut manager = project_manager_fixture();
        if is_empty {
            manager.project_items.clear();
        }
        manager.table_state.select(selected);

        // Act
        navigate(&mut manager);

        // Assert
        assert_eq!(manager.table_state.selected(), expected, "{name}");
    }
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
    assert_eq!(manager.render_parts().git_status, None);
}

#[test]
fn update_active_project_context_drops_previous_project_git_status() {
    // Arrange
    let mut manager = project_manager_fixture();
    manager.set_git_status(Some((2, 1)));
    let reported_status = manager.render_parts().git_status;

    // Act
    manager.update_active_project_context(
        2,
        "service".to_string(),
        Some("main".to_string()),
        None,
        PathBuf::from("/tmp/service"),
    );

    // Assert
    assert_eq!(reported_status, Some((2, 1)));
    assert_eq!(manager.render_parts().git_status, None);
    assert_eq!(manager.render_parts().active_project_id, 2);
    assert_eq!(manager.selected_project_id(), Some(2));
}

#[test]
fn listed_projects_are_the_active_one_and_the_switchers_rows() {
    // Arrange
    let mut manager = project_manager_fixture();
    manager.update_active_project_context(
        5,
        "hidden".to_string(),
        None,
        None,
        PathBuf::from("/tmp/hidden"),
    );

    // Act
    let listed = [1, 2, 5, 9].map(|project_id| manager.is_listed(project_id));
    let rows = [1, 5].map(|project_id| {
        manager
            .listed_project(project_id)
            .map(|project_item| project_item.project.id)
    });

    // Assert
    // An active project the switcher does not offer is listed too.
    assert_eq!(listed, [true, true, true, false]);
    assert_eq!(rows, [Some(1), None]);
}

#[test]
fn viewed_contexts_describe_projects_without_making_them_active() {
    // Arrange
    let mut manager = project_manager_fixture();
    manager.set_git_status(Some((3, 1)));
    let mut viewed_context = ProjectContext::new(
        2,
        "service".to_string(),
        Some("develop".to_string()),
        None,
        PathBuf::from("/tmp/service"),
    );
    viewed_context.git_status = Some((4, 4));

    // Act
    manager.set_viewed_context(viewed_context);
    let viewed = manager.context(2).map(|context| {
        (
            context.id(),
            context.name().to_string(),
            context.git_branch().map(str::to_string),
            context.git_status(),
            context.working_dir().to_path_buf(),
        )
    });
    let active = manager.context(1).map(ProjectContext::git_status);
    let previous = manager.update_active_project_context(
        2,
        "service".to_string(),
        Some("main".to_string()),
        None,
        PathBuf::from("/tmp/service"),
    );
    let promoted_branch = manager.context(2).and_then(ProjectContext::git_branch);

    // Assert
    // A viewed context reports no counts; only the active fetch does.
    assert_eq!(
        viewed,
        Some((
            2,
            "service".to_string(),
            Some("develop".to_string()),
            None,
            PathBuf::from("/tmp/service")
        ))
    );
    assert_eq!(active, Some(Some((3, 1))));
    assert_eq!(manager.active_project_id(), 2);
    assert_eq!(previous.id(), 1);
    assert_eq!(previous.git_status(), Some((3, 1)));
    assert_eq!(promoted_branch, Some("main"));
    assert!(manager.context(1).is_none());
    assert!(manager.context(9).is_none());
    // A demoted context keeps only what a viewed project reports.
    manager.set_viewed_context(previous);
    let demoted = manager.context(1).expect("demoted context");
    assert_eq!(
        (demoted.git_status(), demoted.git_upstream_ref.as_deref()),
        (None, None)
    );
    assert_eq!(demoted.git_branch(), Some("main"));
}

#[test]
fn retained_viewed_contexts_follow_the_loaded_pages() {
    // Arrange
    let mut manager = project_manager_fixture();
    for (project_id, name) in [(2, "service"), (3, "docs")] {
        manager.set_viewed_context(ProjectContext::new(
            project_id,
            name.to_string(),
            None,
            None,
            PathBuf::from(format!("/tmp/{name}")),
        ));
    }

    // Act
    manager.retain_viewed_contexts(&[3]);

    // Assert
    assert!(manager.context(2).is_none());
    assert_eq!(manager.context(3).map(ProjectContext::name), Some("docs"));
    assert_eq!(manager.active_context().id(), 1);
}
