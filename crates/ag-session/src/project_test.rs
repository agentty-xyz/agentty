use std::path::PathBuf;

use crate::project::{Project, ProjectListItem, mru_project_order, ordered_project_items};

#[test]
fn test_display_label_prefers_display_name() {
    // Arrange
    let project = project_list_item_fixture(1, "Agentty", None).project;

    // Act
    let label = project.display_label();

    // Assert
    assert_eq!(label, "Agentty");
}

#[test]
fn test_mru_project_order_orders_by_last_opened_descending() {
    // Arrange
    let project_items = vec![
        project_list_item_fixture(1, "alpha", Some(10)),
        project_list_item_fixture(2, "beta", Some(30)),
        project_list_item_fixture(3, "gamma", Some(20)),
    ];

    // Act
    let project_order = mru_project_order(&project_items);

    // Assert
    let ordered_ids = ordered_project_ids(&project_items, &project_order);
    assert_eq!(ordered_ids, vec![2, 3, 1]);
}

#[test]
fn test_mru_project_order_sorts_never_opened_projects_last_by_label() {
    // Arrange
    let project_items = vec![
        project_list_item_fixture(1, "zeta", None),
        project_list_item_fixture(2, "alpha", None),
        project_list_item_fixture(3, "beta", Some(5)),
    ];

    // Act
    let project_order = mru_project_order(&project_items);

    // Assert
    let ordered_ids = ordered_project_ids(&project_items, &project_order);
    assert_eq!(ordered_ids, vec![3, 2, 1]);
}

#[test]
fn test_ordered_project_items_skips_stale_indices() {
    // Arrange
    let project_items = vec![
        project_list_item_fixture(1, "alpha", Some(10)),
        project_list_item_fixture(2, "beta", Some(30)),
    ];

    // Act
    let ordered_items = ordered_project_items(&project_items, &[1, 7, 0]);

    // Assert
    let ordered_ids: Vec<i64> = ordered_items.iter().map(|item| item.project.id).collect();
    assert_eq!(ordered_ids, vec![2, 1]);
}

#[test]
fn test_display_label_falls_back_to_path_folder_name() {
    // Arrange
    let mut project = project_list_item_fixture(1, "agentty", None).project;
    project.display_name = None;

    // Act
    let label = project.display_label();

    // Assert
    assert_eq!(label, "agentty");
}

/// Builds one project list row with the provided identity and MRU stamp.
fn project_list_item_fixture(id: i64, name: &str, last_opened_at: Option<i64>) -> ProjectListItem {
    ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
        project: Project {
            created_at: 0,
            display_name: Some(name.to_string()),
            git_branch: Some("main".to_string()),
            id,
            is_favorite: false,
            last_opened_at,
            path: PathBuf::from(format!("/tmp/{name}")),
            updated_at: 0,
        },
        session_count: 0,
    }
}

/// Maps an MRU order into the project identifiers it selects.
fn ordered_project_ids(project_items: &[ProjectListItem], project_order: &[usize]) -> Vec<i64> {
    ordered_project_items(project_items, project_order)
        .iter()
        .map(|project_item| project_item.project.id)
        .collect()
}
