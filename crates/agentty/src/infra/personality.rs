//! Workspace-only `.agents` personality discovery.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tracing::warn;

use crate::domain::personality::{
    Personality, PersonalityParseError, PersonalitySummary, parse_agent_definition,
    parse_agent_summary,
};

/// Maximum bytes retained while validating a personality prompt body.
const SUMMARY_BODY_BUFFER_BYTES: usize = 8 * 1024;

/// Boxed async result used by [`PersonalityCatalogClient`] methods.
pub type PersonalityCatalogFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Workspace personality discovery boundary.
///
/// Implementations must inspect only `.agents/agents/*/agent.md` below the
/// supplied session worktree and must not consult a user-global catalog.
#[cfg_attr(test, mockall::automock)]
pub trait PersonalityCatalogClient: Send + Sync {
    /// Lists enabled personality summaries in deterministic display order.
    fn list_summaries(
        &self,
        workspace_root: PathBuf,
    ) -> PersonalityCatalogFuture<Vec<PersonalitySummary>>;

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
    fn list_summaries(
        &self,
        workspace_root: PathBuf,
    ) -> PersonalityCatalogFuture<Vec<PersonalitySummary>> {
        Box::pin(async move { list_workspace_personality_summaries(&workspace_root).await })
    }

    fn resolve(
        &self,
        workspace_root: PathBuf,
        id: String,
    ) -> PersonalityCatalogFuture<Option<Personality>> {
        Box::pin(async move { resolve_workspace_personality(&workspace_root, &id).await })
    }
}

/// Lists lightweight metadata for valid definitions contained by one worktree.
async fn list_workspace_personality_summaries(workspace_root: &Path) -> Vec<PersonalitySummary> {
    let Some((canonical_workspace, canonical_agents_directory)) =
        canonical_catalog_paths(workspace_root).await
    else {
        return Vec::new();
    };

    list_catalog_personality_summaries(
        tokio::fs::read_dir(&canonical_agents_directory).await,
        &canonical_workspace,
        &canonical_agents_directory,
    )
    .await
}

/// Resolves one personality without loading every catalog prompt body.
async fn resolve_workspace_personality(workspace_root: &Path, id: &str) -> Option<Personality> {
    let (canonical_workspace, canonical_agents_directory) =
        canonical_catalog_paths(workspace_root).await?;
    let direct_directory = direct_agent_directory(&canonical_agents_directory, id);
    let agent_directories = list_agent_directories(
        tokio::fs::read_dir(&canonical_agents_directory).await,
        &canonical_agents_directory,
    )
    .await;
    for agent_directory in agent_directories {
        if direct_directory.as_deref() == Some(agent_directory.as_path()) {
            let personality = read_personality(&canonical_workspace, &agent_directory).await;
            if personality
                .as_ref()
                .is_some_and(|personality| personality.id == id)
            {
                return personality;
            }

            continue;
        }
        let Some(summary) = read_personality_summary(&canonical_workspace, &agent_directory).await
        else {
            continue;
        };
        if summary.id != id {
            continue;
        }

        return read_personality(&canonical_workspace, &agent_directory).await;
    }

    None
}

/// Resolves and contains the workspace and its personality catalog.
async fn canonical_catalog_paths(workspace_root: &Path) -> Option<(PathBuf, PathBuf)> {
    let canonical_workspace = match tokio::fs::canonicalize(workspace_root).await {
        Ok(path) => path,
        Err(error) => {
            let workspace_display = workspace_root.display().to_string();
            warn!(
                workspace_root = %workspace_display,
                %error,
                "failed to resolve session worktree for personality discovery"
            );

            return None;
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

            return None;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            let catalog_display = catalog_path.display().to_string();
            warn!(
                agents_directory = %catalog_display,
                %error,
                "failed to resolve workspace personality catalog"
            );

            return None;
        }
    };

    Some((canonical_workspace, canonical_agents_directory))
}

/// Returns the direct-child directory matching one safe personality ID.
fn direct_agent_directory(agents_directory: &Path, id: &str) -> Option<PathBuf> {
    let mut components = Path::new(id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(directory_name)), None) => {
            Some(agents_directory.join(directory_name))
        }
        _ => None,
    }
}

/// Converts one catalog open result into sorted personality summaries.
async fn list_catalog_personality_summaries(
    result: io::Result<tokio::fs::ReadDir>,
    canonical_workspace: &Path,
    agents_directory: &Path,
) -> Vec<PersonalitySummary> {
    let agent_directories = list_agent_directories(result, agents_directory).await;
    let mut personalities_by_id = BTreeMap::new();
    for agent_directory in agent_directories {
        let Some(personality) =
            read_personality_summary(canonical_workspace, &agent_directory).await
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

/// Enumerates direct catalog children in deterministic path order.
async fn list_agent_directories(
    result: io::Result<tokio::fs::ReadDir>,
    agents_directory: &Path,
) -> Vec<PathBuf> {
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

    agent_directories
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
    let (canonical_definition, directory_id) =
        resolve_personality_definition(canonical_workspace, agent_directory).await?;
    let source = tokio::fs::read_to_string(&canonical_definition).await;
    let contents = read_personality_source(source, &canonical_definition)?;

    parse_personality_value(
        parse_agent_definition(&directory_id, &contents),
        &canonical_definition,
    )
}

/// Reads and parses summary metadata without retaining the prompt body.
async fn read_personality_summary(
    canonical_workspace: &Path,
    agent_directory: &Path,
) -> Option<PersonalitySummary> {
    let (canonical_definition, directory_id) =
        resolve_personality_definition(canonical_workspace, agent_directory).await?;
    let contents = read_personality_source(
        read_personality_summary_source(&canonical_definition).await,
        &canonical_definition,
    )?;

    parse_personality_value(
        parse_agent_summary(&directory_id, &contents),
        &canonical_definition,
    )
}

/// Validates and resolves one direct-child personality definition.
async fn resolve_personality_definition(
    canonical_workspace: &Path,
    agent_directory: &Path,
) -> Option<(PathBuf, String)> {
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

    let resolved_path = tokio::fs::canonicalize(&definition_path).await;
    let canonical_definition =
        contain_personality_definition(resolved_path, canonical_workspace, &definition_path)?;
    let directory_id = agent_directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();

    Some((canonical_definition, directory_id))
}

/// Returns whether one entry is a direct, non-symlinked agent directory.
fn is_personality_directory(result: io::Result<std::fs::Metadata>, path: &Path) -> bool {
    match result {
        Ok(metadata) => metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
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

/// Contains one resolved definition path within the session worktree.
fn contain_personality_definition(
    result: io::Result<PathBuf>,
    canonical_workspace: &Path,
    definition_path: &Path,
) -> Option<PathBuf> {
    match result {
        Ok(path) if path.starts_with(canonical_workspace) => Some(path),
        Ok(path) => {
            let definition_display = path.display().to_string();
            let workspace_display = canonical_workspace.display().to_string();
            warn!(
                path = %definition_display,
                workspace_root = %workspace_display,
                "ignored personality definition outside the session worktree"
            );

            None
        }
        Err(error) => {
            let definition_display = definition_path.display().to_string();
            warn!(
                path = %definition_display,
                %error,
                "failed to resolve workspace personality definition"
            );

            None
        }
    }
}

/// Retains definition frontmatter while validating and discarding its body.
async fn read_personality_summary_source(definition_path: &Path) -> io::Result<String> {
    let file = tokio::fs::File::open(definition_path).await?;
    let mut reader = BufReader::new(file);
    let mut contents = String::new();
    if reader.read_line(&mut contents).await? == 0 {
        return Ok(String::new());
    }
    if contents.trim() != "---" {
        return Ok(contents);
    }

    let mut line = String::new();
    let mut found_frontmatter_end = false;
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let is_delimiter = line.trim() == "---";
        contents.push_str(&line);
        if is_delimiter {
            found_frontmatter_end = true;
            break;
        }
    }
    if !found_frontmatter_end {
        return Ok(contents);
    }

    if body_contains_non_whitespace(&mut reader).await? {
        contents.push_str("prompt\n");
    }

    Ok(contents)
}

/// Validates UTF-8 and detects non-whitespace body content in bounded chunks.
async fn body_contains_non_whitespace(
    reader: &mut (dyn AsyncRead + Send + Unpin),
) -> io::Result<bool> {
    let mut buffer = [0_u8; SUMMARY_BODY_BUFFER_BYTES];
    let mut pending_utf8 = Vec::with_capacity(SUMMARY_BODY_BUFFER_BYTES.saturating_add(3));
    let mut has_content = false;

    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        pending_utf8.extend_from_slice(&buffer[..bytes_read]);

        match std::str::from_utf8(&pending_utf8) {
            Ok(text) => {
                has_content |= text.chars().any(|character| !character.is_whitespace());
                pending_utf8.clear();
            }
            Err(error) if error.error_len().is_some() => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, error));
            }
            Err(error) => {
                let valid_bytes = error.valid_up_to();
                let text = String::from_utf8_lossy(&pending_utf8[..valid_bytes]);
                has_content |= text.chars().any(|character| !character.is_whitespace());

                let pending_bytes = pending_utf8.len().saturating_sub(valid_bytes);
                pending_utf8.copy_within(valid_bytes.., 0);
                pending_utf8.truncate(pending_bytes);
            }
        }
    }
    if !pending_utf8.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "personality definition ends with incomplete UTF-8",
        ));
    }

    Ok(has_content)
}

/// Logs one definition read failure and returns successful contents.
fn read_personality_source(result: io::Result<String>, definition_path: &Path) -> Option<String> {
    match result {
        Ok(contents) => Some(contents),
        Err(error) => {
            let definition_display = definition_path.display().to_string();
            warn!(
                path = %definition_display,
                %error,
                "failed to read workspace personality definition"
            );

            None
        }
    }
}

/// Logs one parse failure and returns an enabled personality value.
fn parse_personality_value<T>(
    result: Result<Option<T>, PersonalityParseError>,
    definition_path: &Path,
) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(error) => {
            let definition_display = definition_path.display().to_string();
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
        write_definition(
            workspace.path(),
            "alpha",
            "---\nid: alpha\nname: Same\ndescription: Same name\n---\nAlpha.",
        )
        .await;
        write_definition(
            workspace.path(),
            "beta",
            "---\nid: beta\nname: same\ndescription: Same lowercase name\n---\nBeta.",
        )
        .await;
        let catalog = RealPersonalityCatalogClient;

        // Act
        let personalities = catalog
            .list_summaries(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert_eq!(
            personalities
                .iter()
                .map(|personality| personality.id.as_str())
                .collect::<Vec<_>>(),
            ["architect", "reviewer", "alpha", "beta"]
        );
    }

    #[test]
    fn test_direct_agent_directory_accepts_only_one_normal_component() {
        // Arrange
        let agents_directory = Path::new("/workspace/.agents/agents");

        // Act
        let direct = direct_agent_directory(agents_directory, "reviewer");
        let parent = direct_agent_directory(agents_directory, "../reviewer");
        let nested = direct_agent_directory(agents_directory, "team/reviewer");

        // Assert
        assert_eq!(direct, Some(agents_directory.join("reviewer")));
        assert_eq!(parent, None);
        assert_eq!(nested, None);
    }

    #[tokio::test]
    async fn test_summary_source_retains_frontmatter_and_prompt_presence_only() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        let large_definition = format!(
            "---\nname: Reviewer\ndescription: Reviews code\n---\n{}",
            "x".repeat(SUMMARY_BODY_BUFFER_BYTES.saturating_mul(16))
        );
        write_definition(workspace.path(), "large", &large_definition).await;
        write_definition(workspace.path(), "empty", "").await;
        write_definition(
            workspace.path(),
            "invalid-start",
            "name: Reviewer\nignored body",
        )
        .await;
        write_definition(
            workspace.path(),
            "missing-end",
            "---\nname: Reviewer\ndescription: Reviews code",
        )
        .await;
        write_definition(
            workspace.path(),
            "missing-prompt",
            "---\nname: Reviewer\ndescription: Reviews code\n---\n \n",
        )
        .await;
        let definitions = workspace.path().join(".agents").join("agents");

        // Act
        let large = read_personality_summary_source(&definitions.join("large").join("agent.md"))
            .await
            .expect("large summary source should load");
        let empty = read_personality_summary_source(&definitions.join("empty").join("agent.md"))
            .await
            .expect("empty summary source should load");
        let invalid_start =
            read_personality_summary_source(&definitions.join("invalid-start").join("agent.md"))
                .await
                .expect("invalid summary source should load");
        let missing_end =
            read_personality_summary_source(&definitions.join("missing-end").join("agent.md"))
                .await
                .expect("unterminated summary source should load");
        let missing_prompt =
            read_personality_summary_source(&definitions.join("missing-prompt").join("agent.md"))
                .await
                .expect("promptless summary source should load");

        // Assert
        assert_eq!(
            large,
            "---\nname: Reviewer\ndescription: Reviews code\n---\nprompt\n"
        );
        assert_eq!(empty, "");
        assert_eq!(invalid_start, "name: Reviewer\n");
        assert_eq!(
            missing_end,
            "---\nname: Reviewer\ndescription: Reviews code"
        );
        assert_eq!(
            missing_prompt,
            "---\nname: Reviewer\ndescription: Reviews code\n---\n"
        );
    }

    #[tokio::test]
    async fn test_summary_body_scan_validates_utf8_across_bounded_chunks() {
        // Arrange
        let mut split_body = vec![b' '; SUMMARY_BODY_BUFFER_BYTES.saturating_sub(1)];
        split_body.extend_from_slice("é".as_bytes());
        let mut split_reader = split_body.as_slice();
        let invalid_body = [b'x', 0xff];
        let mut invalid_reader = invalid_body.as_slice();
        let incomplete_body = [b'x', 0xc3];
        let mut incomplete_reader = incomplete_body.as_slice();

        // Act
        let has_content = body_contains_non_whitespace(&mut split_reader)
            .await
            .expect("split UTF-8 should remain valid");
        let invalid_error = body_contains_non_whitespace(&mut invalid_reader)
            .await
            .expect_err("invalid UTF-8 should fail");
        let incomplete_error = body_contains_non_whitespace(&mut incomplete_reader)
            .await
            .expect_err("incomplete UTF-8 should fail");

        // Assert
        assert!(has_content);
        assert_eq!(invalid_error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(incomplete_error.kind(), io::ErrorKind::InvalidData);
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
    async fn test_real_catalog_skips_unavailable_entries_and_returns_none() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        let agents_directory = workspace.path().join(".agents").join("agents");
        tokio::fs::create_dir_all(agents_directory.join("a-missing"))
            .await
            .expect("create directory without definition");
        write_definition(
            workspace.path(),
            "requested",
            "---\nid: other\nname: Other\ndescription: Different id\n---\nOther prompt.",
        )
        .await;
        let invalid_directory = agents_directory.join("invalid");
        tokio::fs::create_dir_all(&invalid_directory)
            .await
            .expect("create invalid definition directory");
        tokio::fs::write(invalid_directory.join("agent.md"), [0xff])
            .await
            .expect("write invalid UTF-8 definition");
        let catalog = RealPersonalityCatalogClient;

        // Act
        let mismatched = catalog
            .resolve(workspace.path().to_path_buf(), "requested".to_string())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let unreadable = catalog
            .resolve(workspace.path().to_path_buf(), "invalid".to_string())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let missing = catalog
            .resolve(workspace.path().to_path_buf(), "absent".to_string())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert!(mismatched.is_none());
        assert!(unreadable.is_none());
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_duplicate_id_resolution_matches_picker_directory_winner() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        write_definition(
            workspace.path(),
            "a",
            "---\nid: reviewer\nname: First Reviewer\ndescription: First duplicate\n---\nFirst \
             prompt.",
        )
        .await;
        write_definition(
            workspace.path(),
            "reviewer",
            "---\nid: reviewer\nname: Named Reviewer\ndescription: Named directory\n---\nNamed \
             prompt.",
        )
        .await;
        let catalog = RealPersonalityCatalogClient;

        // Act
        let summaries = catalog
            .list_summaries(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let resolved = catalog
            .resolve(workspace.path().to_path_buf(), "reviewer".to_string())
            .await
            .expect("picker personality should resolve");

        // Assert
        assert_eq!(
            summaries,
            vec![PersonalitySummary {
                description: "First duplicate".to_string(),
                id: "reviewer".to_string(),
                name: "First Reviewer".to_string(),
            }]
        );
        assert_eq!(resolved.name, "First Reviewer");
        assert_eq!(resolved.prompt, "First prompt.");
    }

    #[tokio::test]
    async fn test_real_catalog_returns_empty_for_missing_or_malformed_catalog() {
        // Arrange
        let workspace = tempfile::tempdir().expect("create workspace");
        let catalog = RealPersonalityCatalogClient;

        // Act
        let missing = catalog.list_summaries(workspace.path().to_path_buf()).await;
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
            .list_summaries(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert_eq!(
            missing,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
        assert_eq!(
            malformed,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
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
            .list_summaries(missing_workspace)
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let unreadable = catalog
            .list_summaries(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert_eq!(
            missing,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
        assert_eq!(
            unreadable,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
    }

    #[tokio::test]
    async fn test_catalog_helpers_fail_closed_for_io_and_containment_errors() {
        // Arrange
        let workspace = Path::new("/workspace");
        let agents_directory = workspace.join(".agents").join("agents");
        let definition_path = agents_directory.join("reviewer").join("agent.md");
        let outside_definition = PathBuf::from("/outside/reviewer/agent.md");

        // Act
        let entries = list_catalog_personality_summaries(
            Err(io::Error::other("open failed")),
            workspace,
            &agents_directory,
        )
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
        let (
            next_entry,
            directory,
            missing_directory,
            missing_definition,
            definition,
            outside,
            unresolved,
        ) = tracing::subscriber::with_default(crate::test_support::TestSubscriber, || {
            (
                next_agent_directory(
                    io::Result::Err(io::Error::other("enumeration failed")),
                    &agents_directory,
                ),
                is_personality_directory(
                    io::Result::Err(io::Error::other("metadata failed")),
                    &agents_directory,
                ),
                is_personality_directory(
                    io::Result::Err(io::Error::from(io::ErrorKind::NotFound)),
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
                contain_personality_definition(Ok(outside_definition), workspace, &definition_path),
                contain_personality_definition(
                    Err(io::Error::other("canonicalize failed")),
                    workspace,
                    &definition_path,
                ),
            )
        });

        // Assert
        assert_eq!(
            entries,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
        assert!(next_entry.is_none());
        assert!(!directory);
        assert!(!missing_directory);
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
            .list_summaries(workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;
        let loop_personalities = catalog
            .list_summaries(loop_workspace.path().to_path_buf())
            .with_subscriber(crate::test_support::TestSubscriber)
            .await;

        // Assert
        assert_eq!(
            outside_personalities,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
        assert_eq!(
            loop_personalities,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
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
        let personalities = catalog.list_summaries(workspace.path().to_path_buf()).await;

        // Assert
        assert_eq!(
            personalities,
            [] as [crate::domain::personality::PersonalitySummary; 0]
        );
    }
}
