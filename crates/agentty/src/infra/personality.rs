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
        .and_then(std::ffi::OsStr::to_str)
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
#[path = "personality_test.rs"]
mod tests;
