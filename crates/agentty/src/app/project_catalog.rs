//! User-authorized project access and cancellable catalog discovery.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::app::{App, AppError, AppEvent, Tab};
use crate::domain::input::InputState;
use crate::domain::setting::SettingName;
use crate::infra::fs::FsError;
use crate::infra::project_discovery::ProjectDiscoveryClient;
use crate::presentation::app_mode::AppMode;
use crate::presentation::project_catalog::ProjectCatalogDialog;

/// Result returned to the foreground reducer for one authorized scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectDiscoveryCompletion {
    pub(crate) request_id: u64,
    pub(crate) result: Result<Vec<PathBuf>, String>,
}

/// Injected discovery capability and ownership of the current scan.
pub(crate) struct ProjectCatalog {
    pub(crate) home: Option<PathBuf>,
    client: Arc<dyn ProjectDiscoveryClient>,
    next_request_id: u64,
    scan: Option<(u64, Arc<AtomicBool>)>,
}

impl ProjectCatalog {
    pub(crate) fn new(client: Arc<dyn ProjectDiscoveryClient>, home: Option<PathBuf>) -> Self {
        Self {
            home,
            client,
            next_request_id: 1,
            scan: None,
        }
    }

    /// Cooperatively stops directory traversal and invalidates late results.
    pub(crate) fn cancel(&mut self) {
        if let Some((_, scan)) = self.scan.take() {
            scan.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for ProjectCatalog {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl App {
    /// Opens the one-time home scan invitation without accessing that folder.
    pub(crate) async fn suggest_project_discovery(&mut self) {
        let Some(root) = self.project_catalog.home.clone() else {
            return;
        };
        let seen = self
            .services
            .db()
            .settings()
            .get_setting(SettingName::ProjectDiscoveryPromptSeen)
            .await;
        if matches!(seen, Ok(Some(value)) if value == "true") {
            return;
        }
        self.mode = AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::SuggestHome { root, selected: 1 },
        };
    }

    /// Records acknowledgement; accepting permits this scan only, not future
    /// scans.
    pub(crate) async fn dismiss_project_discovery_suggestion(&mut self, discover: bool) {
        if let Err(error) = self
            .services
            .db()
            .settings()
            .upsert_setting(SettingName::ProjectDiscoveryPromptSeen, "true")
            .await
        {
            self.project_catalog_message(error.to_string());
            return;
        }
        self.mode = AppMode::List;
        if discover && let Some(root) = self.project_catalog.home.clone() {
            self.start_project_discovery(root).await;
        }
    }

    /// Opens a path input. No filesystem access happens until submission.
    pub(crate) fn open_project_path_input(&mut self, discover: bool) {
        self.mode = AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::PathInput {
                discover,
                error: None,
                input: InputState::default(),
            },
        };
    }

    /// Resolves a submitted path and validates only the requested location.
    pub(crate) async fn submit_project_path(&mut self) {
        let AppMode::ProjectCatalog {
            dialog:
                ProjectCatalogDialog::PathInput {
                    discover, input, ..
                },
        } = &self.mode
        else {
            return;
        };
        let discover = *discover;
        let text = input.text().trim();
        if text.is_empty() {
            self.project_catalog_error("Enter a repository or folder path".to_string());
            return;
        }
        let path = if text == "~" || text.starts_with("~/") {
            let Some(home) = &self.project_catalog.home else {
                self.project_catalog_error("Home directory is unavailable".to_string());
                return;
            };
            home.join(text.strip_prefix("~/").unwrap_or(""))
        } else {
            let path = PathBuf::from(text);
            if path.is_absolute() {
                path
            } else {
                self.working_dir().join(path)
            }
        };
        if discover {
            self.start_project_discovery(path).await;
        } else {
            match self.register_project(&path).await {
                Ok(()) => {
                    self.reload_projects().await;
                    self.mode = AppMode::List;
                }
                Err(error) => self.project_catalog_error(error.to_string()),
            }
        }
    }

    /// Starts a scan on a background task after explicit user submission.
    pub(crate) async fn start_project_discovery(&mut self, root: PathBuf) {
        let fs_client = self.services.fs_client();
        let root = match fs_client.canonicalize(root).await {
            Ok(root) if fs_client.is_dir(root.clone()) => root,
            Ok(_) => {
                self.project_catalog_message(
                    "Choose a directory to discover repositories".to_string(),
                );
                return;
            }
            Err(error) => {
                self.project_catalog_message(error.to_string());
                return;
            }
        };
        let session_worktree_root = match self.canonical_session_worktree_root().await {
            Ok(root) => root,
            Err(error) => {
                self.project_catalog_message(error.to_string());
                return;
            }
        };
        self.project_catalog.cancel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let request_id = self.project_catalog.next_request_id;
        self.project_catalog.next_request_id += 1;
        self.project_catalog.scan = Some((request_id, Arc::clone(&cancelled)));
        self.mode = AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Scanning { root: root.clone() },
        };
        let client = Arc::clone(&self.project_catalog.client);
        let event_tx = self.services.event_sender();
        tokio::spawn(async move {
            let result = client
                .discover_project_paths(root, session_worktree_root, Arc::clone(&cancelled))
                .await
                .map_err(|error| error.to_string());
            let _ = event_tx.send(AppEvent::ProjectDiscoveryCompleted {
                completion: ProjectDiscoveryCompletion { request_id, result },
            });
        });
    }

    /// Applies a scan completion only while its original dialog is still
    /// active.
    pub(crate) fn complete_project_discovery(&mut self, completion: ProjectDiscoveryCompletion) {
        if self
            .project_catalog
            .scan
            .as_ref()
            .is_none_or(|(request_id, _)| *request_id != completion.request_id)
        {
            return;
        }
        self.project_catalog.scan = None;
        match completion.result {
            Ok(paths) if paths.is_empty() => self.project_catalog_message(
                "No repositories found. Hidden and inaccessible folders may be skipped."
                    .to_string(),
            ),
            Ok(paths) => {
                self.tabs.set(Tab::Projects);
                self.mode = AppMode::ProjectCatalog {
                    dialog: ProjectCatalogDialog::Results {
                        error: None,
                        paths: paths.into_iter().map(|path| (path, false)).collect(),
                        selected: 0,
                    },
                };
            }
            Err(error) => self.project_catalog_message(error),
        }
    }

    /// Saves only selected results, leaving history and unselected paths
    /// intact.
    pub(crate) async fn save_discovered_projects(&mut self) {
        let AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Results { paths, .. },
        } = &self.mode
        else {
            return;
        };
        let selected = paths
            .iter()
            .filter(|(_, selected)| *selected)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        for path in selected {
            if let Err(error) = self.register_project(&path).await {
                self.project_catalog_error(error.to_string());
                self.reload_projects().await;
                return;
            }
        }
        self.reload_projects().await;
        self.mode = AppMode::List;
    }

    /// Cancels the current scan or closes a project dialog without saving.
    pub(crate) fn close_project_catalog(&mut self) {
        self.project_catalog.cancel();
        self.mode = AppMode::List;
    }

    /// Displays project access failures without hiding saved history.
    pub(crate) fn project_catalog_message(&mut self, message: String) {
        self.mode = AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Message { message },
        };
    }

    async fn register_project(&self, path: &Path) -> Result<(), AppError> {
        let fs_client = self.services.fs_client();
        let path = fs_client
            .canonicalize(path.to_path_buf())
            .await
            .map_err(|error| AppError::Workflow(error.to_string()))?;
        if !fs_client.is_dir(path.clone()) || !fs_client.exists(path.join(".git")) {
            return Err(AppError::Workflow(
                "Choose a Git repository root".to_string(),
            ));
        }
        if path.starts_with(self.canonical_session_worktree_root().await?) {
            return Err(AppError::Workflow(
                "Agentty session worktrees cannot be added as projects".to_string(),
            ));
        }
        let branch = self
            .services
            .git_client()
            .detect_git_info(path.clone())
            .await;
        let project_id = self
            .services
            .db()
            .projects()
            .upsert_project(&path.to_string_lossy(), branch)
            .await?;
        self.services
            .db()
            .settings()
            .upsert_project_setting(project_id, SettingName::ProjectCatalogVisible, "true")
            .await?;

        Ok(())
    }

    /// Resolves the trusted worktree root, including a not-yet-created leaf.
    async fn canonical_session_worktree_root(&self) -> Result<PathBuf, AppError> {
        let root = self.services.base_path();
        let fs_client = self.services.fs_client();
        match fs_client.canonicalize(root.to_path_buf()).await {
            Ok(root) => Ok(root),
            Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = root
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let parent = fs_client
                    .canonicalize(parent.to_path_buf())
                    .await
                    .map_err(|error| AppError::Workflow(error.to_string()))?;

                Ok(parent.join(root.file_name().unwrap_or_default()))
            }
            Err(error) => Err(AppError::Workflow(error.to_string())),
        }
    }

    fn project_catalog_error(&mut self, message: String) {
        if let AppMode::ProjectCatalog {
            dialog:
                ProjectCatalogDialog::PathInput { error, .. }
                | ProjectCatalogDialog::Results { error, .. },
        } = &mut self.mode
        {
            *error = Some(message);
        }
    }
}

#[cfg(test)]
#[path = "project_catalog_test.rs"]
mod tests;
