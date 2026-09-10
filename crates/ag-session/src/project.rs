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
#[path = "project_test.rs"]
mod tests;
