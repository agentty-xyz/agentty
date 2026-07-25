//! Workspace-only `.agents` personality discovery.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tracing::warn;

use crate::domain::personality::{Personality, parse_agent_definition};

/// Boxed async result used by [`PersonalityCatalogClient`] methods.
pub type PersonalityCatalogFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Workspace personality discovery boundary.
///
/// Implementations must inspect only `.agents/agents/*/agent.md` below the
/// supplied session worktree and must not consult a user-global catalog.
#[cfg_attr(test, mockall::automock)]
pub trait PersonalityCatalogClient: Send + Sync {
    /// Lists enabled personality definitions in deterministic display order.
    fn list(&self, workspace_root: PathBuf) -> PersonalityCatalogFuture<Vec<Personality>>;

    /// Resolves one enabled personality by its declared or directory ID.
    fn resolve(
        &self,
        workspace_root: PathBuf,
        id: String,
    ) -> PersonalityCatalogFuture<Option<Personality>>;
}

/// Production workspace personality catalog.
pub struct RealPersonalityCatalogClient;

impl PersonalityCatalogClient for RealPersonalityCatalogClient {
    fn list(&self, workspace_root: PathBuf) -> PersonalityCatalogFuture<Vec<Personality>> {
        Box::pin(async move { list_workspace_personalities(&workspace_root).await })
    }

    fn resolve(
        &self,
        workspace_root: PathBuf,
        id: String,
    ) -> PersonalityCatalogFuture<Option<Personality>> {
        Box::pin(async move {
            list_workspace_personalities(&workspace_root)
                .await
                .into_iter()
                .find(|personality| personality.id == id)
        })
    }
}

/// Lists valid direct-child agent definitions contained by one worktree.
async fn list_workspace_personalities(workspace_root: &Path) -> Vec<Personality> {
    let canonical_workspace = match tokio::fs::canonicalize(workspace_root).await {
        Ok(path) => path,
        Err(error) => {
            let workspace_display = workspace_root.display().to_string();
            warn!(
                workspace_root = %workspace_display,
                %error,
                "failed to resolve session worktree for personality discovery"
            );

            return Vec::new();
        }
    };
    let catalog_path = workspace_root.join(".agents").join("agents");
    let canonical_agents_directory = match tokio::fs::canonicalize(&catalog_path).await {
        Ok(path) if path.starts_with(&canonical_workspace) => path,
        Ok(path) => {
            let catalog_display = path.display().to_string();
            let workspace_display = canonical_workspace.display().to_string();
            warn!(
                agents_directory = %catalog_display,
                workspace_root = %workspace_display,
                "ignored personality catalog outside the session worktree"
            );

            return Vec::new();
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            let catalog_display = catalog_path.display().to_string();
            warn!(
                agents_directory = %catalog_display,
                %error,
                "failed to resolve workspace personality catalog"
            );

            return Vec::new();
        }
    };
    list_catalog_personalities(
        tokio::fs::read_dir(&canonical_agents_directory).await,
        &canonical_workspace,
        &canonical_agents_directory,
    )
    .await
}

/// Converts one catalog open result into sorted workspace personalities.
async fn list_catalog_personalities(
    result: io::Result<tokio::fs::ReadDir>,
    canonical_workspace: &Path,
    agents_directory: &Path,
) -> Vec<Personality> {
    let mut entries = match result {
        Ok(entries) => entries,
        Err(error) => {
            let catalog_display = agents_directory.display().to_string();
            warn!(
                agents_directory = %catalog_display,
                %error,
                "failed to read workspace personality catalog"
            );

            return Vec::new();
        }
    };
    let mut agent_directories = Vec::new();
    loop {
        let entry = entries
            .next_entry()
            .await
            .map(|entry| entry.map(|entry| entry.path()));
        let Some(profile_directory) = next_agent_directory(entry, agents_directory) else {
            break;
        };
        agent_directories.push(profile_directory);
    }
    agent_directories.sort();

    let mut personalities_by_id = BTreeMap::new();
    for agent_directory in agent_directories {
        let Some(personality) = read_personality(canonical_workspace, &agent_directory).await
        else {
            continue;
        };
        if personalities_by_id.contains_key(&personality.id) {
            let definition_directory_display = agent_directory.display().to_string();
            warn!(
                personality_id = personality.id,
                path = %definition_directory_display,
                "ignored duplicate workspace personality id"
            );
            continue;
        }
        personalities_by_id.insert(personality.id.clone(), personality);
    }

    let mut personalities = personalities_by_id.into_values().collect::<Vec<_>>();
    personalities.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    personalities
}

/// Returns one enumerated agent directory or stops after completion/failure.
fn next_agent_directory(
    result: io::Result<Option<PathBuf>>,
    agents_directory: &Path,
) -> Option<PathBuf> {
    match result {
        Ok(entry) => entry,
        Err(error) => {
            let catalog_display = agents_directory.display().to_string();
            warn!(
                agents_directory = %catalog_display,
                %error,
                "failed while enumerating workspace personality catalog"
            );

            None
        }
    }
}

/// Reads and parses one direct-child agent definition.
async fn read_personality(
    canonical_workspace: &Path,
    agent_directory: &Path,
) -> Option<Personality> {
    if !is_personality_directory(
        tokio::fs::symlink_metadata(agent_directory).await,
        agent_directory,
    ) {
        return None;
    }

    let definition_path = agent_directory.join("agent.md");
    if !is_personality_definition(
        tokio::fs::symlink_metadata(&definition_path).await,
        &definition_path,
    ) {
        return None;
    }

    let directory_id = agent_directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    parse_personality_definition_file(
        tokio::fs::canonicalize(&definition_path).await,
        canonical_workspace,
        &definition_path,
        directory_id,
    )
    .await
}

/// Returns whether one entry is a direct, non-symlinked agent directory.
fn is_personality_directory(result: io::Result<std::fs::Metadata>, path: &Path) -> bool {
    match result {
        Ok(metadata) => metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        Err(error) => {
            let directory_display = path.display().to_string();
            warn!(
                path = %directory_display,
                %error,
                "failed to inspect workspace personality directory"
            );

            false
        }
    }
}

/// Returns whether one definition is a direct, non-symlinked regular file.
fn is_personality_definition(result: io::Result<std::fs::Metadata>, path: &Path) -> bool {
    match result {
        Ok(metadata) => metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            let definition_display = path.display().to_string();
            warn!(
                path = %definition_display,
                %error,
                "failed to inspect workspace personality definition"
            );

            false
        }
    }
}

/// Resolves, reads, and parses one definition contained by the worktree.
async fn parse_personality_definition_file(
    result: io::Result<PathBuf>,
    canonical_workspace: &Path,
    definition_path: &Path,
    directory_id: &str,
) -> Option<Personality> {
    let canonical_definition = match result {
        Ok(path) if path.starts_with(canonical_workspace) => path,
        Ok(path) => {
            let definition_display = path.display().to_string();
            let workspace_display = canonical_workspace.display().to_string();
            warn!(
                path = %definition_display,
                workspace_root = %workspace_display,
                "ignored personality definition outside the session worktree"
            );

            return None;
        }
        Err(error) => {
            let definition_display = definition_path.display().to_string();
            warn!(
                path = %definition_display,
                %error,
                "failed to resolve workspace personality definition"
            );

            return None;
        }
    };
    let contents = match tokio::fs::read_to_string(&canonical_definition).await {
        Ok(contents) => contents,
        Err(error) => {
            let definition_display = canonical_definition.display().to_string();
            warn!(
                path = %definition_display,
                %error,
                "failed to read workspace personality definition"
            );

            return None;
        }
    };

    match parse_agent_definition(directory_id, &contents) {
        Ok(personality) => personality,
        Err(error) => {
            let definition_display = canonical_definition.display().to_string();
            warn!(
                path = %definition_display,
                %error,
                "ignored malformed workspace personality definition"
            );

            None
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing::instrument::WithSubscriber;

    use super::*;

    /// Writes one agent definition below the test workspace.
    async fn write_definition(workspace: &Path, directory: &str, definition: &str) {
        let agent_directory = workspace.join(".agents").join("agents").join(directory);
        tokio::fs::create_dir_all(&agent_directory)
            .await
            .expect("create agent directory");
        tokio::fs::write(agent_directory.join("agent.md"), definition)
            .await
            .expect("write agent definition");
    }

    #[tokio::test]
    async fn test_real_catalog_lists_enabled_workspace_personalities_in_name_order() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        write_definition(
            workspace.path(),
            "reviewer",
            "---\nid: reviewer\nname: Reviewer\ndescription: Reviews code\n---\nReview.",
        )
        .await;
        write_definition(
            workspace.path(),
            "architect",
            "---\nid: architect\nname: Architect\ndescription: Designs systems\n---\nDesign.",
        )
        .await;
        write_definition(
            workspace.path(),
            "disabled",
            "---\nname: Disabled\ndescription: Hidden\nenabled: false\n---\nHide.",
        )
        .await;
        write_definition(
            workspace.path(),
            "duplicate",
            "---\nid: reviewer\nname: Duplicate\ndescription: Duplicate id\n---\nDuplicate.",
        )
        .await;
        let catalog = RealPersonalityCatalogClient;

        // Act
        let personalities = catalog
            .list(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert_eq!(
            personalities
                .iter()
                .map(|personality| personality.id.as_str())
                .collect::<Vec<_>>(),
            ["architect", "reviewer"]
        );
    }

    #[tokio::test]
    async fn test_real_catalog_resolves_declared_id_and_directory_fallback() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        write_definition(
            workspace.path(),
            "reviewer-folder",
            "---\nid: reviewer\nname: Reviewer\ndescription: Reviews code\n---\nReview.",
        )
        .await;
        write_definition(
            workspace.path(),
            "planner",
            "---\nname: Planner\ndescription: Plans work\n---\nPlan.",
        )
        .await;
        let catalog = RealPersonalityCatalogClient;

        // Act
        let declared = catalog
            .resolve(workspace.path().to_path_buf(), "reviewer".to_string())
            .await;
        let fallback = catalog
            .resolve(workspace.path().to_path_buf(), "planner".to_string())
            .await;

        // Assert
        assert_eq!(
            declared.map(|personality| personality.name),
            Some("Reviewer".to_string())
        );
        assert_eq!(
            fallback.map(|personality| personality.name),
            Some("Planner".to_string())
        );
    }

    #[tokio::test]
    async fn test_real_catalog_returns_empty_for_missing_or_malformed_catalog() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        let catalog = RealPersonalityCatalogClient;

        // Act
        let missing = catalog.list(workspace.path().to_path_buf()).await;
        write_definition(
            workspace.path(),
            "malformed",
            "---\nname malformed\n---\nPrompt.",
        )
        .await;
        tokio::fs::create_dir(
            workspace
                .path()
                .join(".agents")
                .join("agents")
                .join("missing-definition"),
        )
        .await
        .expect("create agent directory without a definition");
        let malformed = catalog
            .list(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert!(missing.is_empty());
        assert!(malformed.is_empty());
    }

    #[tokio::test]
    async fn test_real_catalog_handles_missing_workspace_and_unreadable_definition() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        let missing_workspace = workspace.path().join("missing");
        let invalid_directory = workspace
            .path()
            .join(".agents")
            .join("agents")
            .join("invalid");
        tokio::fs::create_dir_all(&invalid_directory)
            .await
            .expect("create invalid definition directory");
        tokio::fs::write(invalid_directory.join("agent.md"), [0xff])
            .await
            .expect("write invalid UTF-8 definition");
        let catalog = RealPersonalityCatalogClient;

        // Act
        let missing = catalog
            .list(missing_workspace)
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let unreadable = catalog
            .list(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert!(missing.is_empty());
        assert!(unreadable.is_empty());
    }

    #[tokio::test]
    async fn test_catalog_helpers_fail_closed_for_io_and_containment_errors() {
        // Arrange
        let workspace = Path::new("/workspace");
        let agents_directory = workspace.join(".agents").join("agents");
        let definition_path = agents_directory.join("reviewer").join("agent.md");
        let outside_definition = PathBuf::from("/outside/reviewer/agent.md");

        // Act
        let entries = list_catalog_personalities(
            Err(io::Error::other("open failed")),
            workspace,
            &agents_directory,
        )
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
        let (next_entry, directory, missing_definition, definition) =
            tracing::subscriber::with_default(crate::test_support::TestSubscriber, || {
                (
                    next_agent_directory(
                        io::Result::Err(io::Error::other("enumeration failed")),
                        &agents_directory,
                    ),
                    is_personality_directory(
                        io::Result::Err(io::Error::other("metadata failed")),
                        &agents_directory,
                    ),
                    is_personality_definition(
                        io::Result::Err(io::Error::from(io::ErrorKind::NotFound)),
                        &definition_path,
                    ),
                    is_personality_definition(
                        io::Result::Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "metadata failed",
                        )),
                        &definition_path,
                    ),
                )
            });
        let outside = parse_personality_definition_file(
            Ok(outside_definition),
            workspace,
            &definition_path,
            "reviewer",
        )
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
        let unresolved = parse_personality_definition_file(
            Err(io::Error::other("canonicalize failed")),
            workspace,
            &definition_path,
            "reviewer",
        )
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

        // Assert
        assert!(entries.is_empty());
        assert!(next_entry.is_none());
        assert!(!directory);
        assert!(!missing_definition);
        assert!(!definition);
        assert!(outside.is_none());
        assert!(unresolved.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_real_catalog_rejects_catalog_links_outside_workspace() {
        use std::os::unix::fs::symlink;

        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        let outside = tempfile::tempdir().expect("create outside directory");
        let agents_parent = workspace.path().join(".agents");
        tokio::fs::create_dir_all(&agents_parent)
            .await
            .expect("create agents parent");
        symlink(outside.path(), agents_parent.join("agents")).expect("create outside catalog link");
        let loop_workspace = tempfile::tempdir().expect("create loop workspace");
        let loop_agents_parent = loop_workspace.path().join(".agents");
        tokio::fs::create_dir_all(&loop_agents_parent)
            .await
            .expect("create loop agents parent");
        symlink("agents", loop_agents_parent.join("agents")).expect("create catalog link loop");
        let catalog = RealPersonalityCatalogClient;

        // Act
        let outside_personalities = catalog
            .list(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let loop_personalities = catalog
            .list(loop_workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert!(outside_personalities.is_empty());
        assert!(loop_personalities.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_real_catalog_rejects_symlinked_definitions_outside_workspace() {
        use std::os::unix::fs::symlink;

        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        let outside = tempfile::tempdir().expect("create outside directory");
        let agents_directory = workspace.path().join(".agents").join("agents");
        tokio::fs::create_dir_all(&agents_directory)
            .await
            .expect("create catalog");
        write_definition(
            outside.path(),
            "external",
            "---\nname: External\ndescription: External prompt\n---\nExternal.",
        )
        .await;
        symlink(
            outside
                .path()
                .join(".agents")
                .join("agents")
                .join("external"),
            agents_directory.join("external"),
        )
        .expect("create symlink");
        let catalog = RealPersonalityCatalogClient;

        // Act
        let personalities = catalog.list(workspace.path().to_path_buf()).await;

        // Assert
        assert!(personalities.is_empty());
    }
}
