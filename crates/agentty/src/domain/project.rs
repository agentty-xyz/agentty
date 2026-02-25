use std::path::{Path, PathBuf};

/// Persisted project metadata used for multi-project management.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    pub created_at: i64,
    pub display_name: Option<String>,
    pub git_branch: Option<String>,
    pub id: i64,
    pub is_favorite: bool,
    pub last_opened_at: Option<i64>,
    pub path: PathBuf,
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
    pub last_session_updated_at: Option<i64>,
    pub project: Project,
    pub session_count: u32,
}

/// Derives a readable project name from its filesystem path.
#[must_use]
pub fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
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
