use std::cmp::Reverse;
use std::path::{Path, PathBuf};

/// Persisted project metadata used for multi-project management.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    /// Creation timestamp in Unix seconds.
    pub created_at: i64,
    /// Optional user-defined display name.
    pub display_name: Option<String>,
    /// Last observed branch for the project checkout.
    pub git_branch: Option<String>,
    /// Stable database identifier.
    pub id: i64,
    /// Whether the project is pinned ahead of other projects.
    pub is_favorite: bool,
    /// Most recent project-open timestamp in Unix seconds.
    pub last_opened_at: Option<i64>,
    /// Absolute path to the project checkout.
    pub path: PathBuf,
    /// Last metadata update timestamp in Unix seconds.
    pub updated_at: i64,
}

impl Project {
    /// Returns the preferred project label for UI rendering.
    #[must_use]
    pub fn display_label(&self) -> String {
        if let Some(display_name) = self.display_name.as_deref()
            && !display_name.trim().is_empty()
        {
            return display_name.to_string();
        }

        project_name_from_path(self.path.as_path())
    }
}

/// Aggregated project snapshot for list and switcher views.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectListItem {
    /// Number of sessions still in an active lifecycle state.
    pub active_session_count: u32,
    /// Total input tokens accumulated by sessions in this project.
    pub input_tokens: u64,
    /// Most recent session update timestamp for this project.
    pub last_session_updated_at: Option<i64>,
    /// Total output tokens accumulated by sessions in this project.
    pub output_tokens: u64,
    /// Persisted project metadata for display and selection.
    pub project: Project,
    /// Total number of sessions belonging to this project.
    pub session_count: u32,
}

/// Returns indices into `project_items` ordered most-recently-opened first.
///
/// Projects that were never opened sort after all opened projects; ties are
/// broken by display label so the switcher order stays deterministic. Callers
/// cache the returned order alongside the rows it was derived from and rebuild
/// it only when the rows change, keeping the sort off the render path.
#[must_use]
pub fn mru_project_order(project_items: &[ProjectListItem]) -> Vec<usize> {
    let mut ordered_indices: Vec<usize> = (0..project_items.len()).collect();
    ordered_indices.sort_by_cached_key(|&index| {
        let project = &project_items[index].project;

        (Reverse(project.last_opened_at), project.display_label())
    });

    ordered_indices
}

/// Returns the project rows named by `project_order`, skipping stale indices.
#[must_use]
pub fn ordered_project_items<'a>(
    project_items: &'a [ProjectListItem],
    project_order: &[usize],
) -> Vec<&'a ProjectListItem> {
    project_order
        .iter()
        .filter_map(|&index| project_items.get(index))
        .collect()
}

/// Derives a readable project name from its filesystem path.
#[must_use]
pub fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_label_prefers_display_name() {
        // Arrange
        let project = Project {
            created_at: 0,
            display_name: Some("Agentty".to_string()),
            git_branch: Some("main".to_string()),
            id: 1,
            is_favorite: false,
            last_opened_at: None,
            path: PathBuf::from("/tmp/agentty"),
            updated_at: 0,
        };

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

    /// Maps an MRU order into the project identifiers it selects.
    fn ordered_project_ids(project_items: &[ProjectListItem], project_order: &[usize]) -> Vec<i64> {
        ordered_project_items(project_items, project_order)
            .iter()
            .map(|project_item| project_item.project.id)
            .collect()
    }

    /// Builds one project list row with the provided identity and MRU stamp.
    fn project_list_item_fixture(
        id: i64,
        name: &str,
        last_opened_at: Option<i64>,
    ) -> ProjectListItem {
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

    #[test]
    fn test_display_label_falls_back_to_path_folder_name() {
        // Arrange
        let project = Project {
            created_at: 0,
            display_name: None,
            git_branch: Some("main".to_string()),
            id: 1,
            is_favorite: false,
            last_opened_at: None,
            path: PathBuf::from("/tmp/agentty"),
            updated_at: 0,
        };

        // Act
        let label = project.display_label();

        // Assert
        assert_eq!(label, "agentty");
    }
}
