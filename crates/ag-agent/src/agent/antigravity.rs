use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::{fs, io};

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::prompt::{self as shared_prompt, CliPromptAccessRootMode};

/// Wall-clock limit passed with `agy --print` for one Agentty turn.
///
/// Antigravity CLI defaults print mode to five minutes, which is too short
/// for repository edits.
const ANTIGRAVITY_PRINT_TIMEOUT: &str = "1h";
/// Git exclude pattern for Antigravity workspace project state.
const ANTIGRAVITY_PROJECT_STATE_PATTERN: &str = ".antigravitycli/";
/// Git exclude pattern for Antigravity's workspace project cache file.
const ANTIGRAVITY_PROJECT_CACHE_PATTERN: &str = "cache/projects.json";
/// Git exclude patterns for Antigravity workspace-local state files.
const ANTIGRAVITY_PROJECT_STATE_PATTERNS: &[&str] = &[
    ANTIGRAVITY_PROJECT_STATE_PATTERN,
    ANTIGRAVITY_PROJECT_CACHE_PATTERN,
];
/// Non-hidden temp directory used for Antigravity worktree aliases.
const ANTIGRAVITY_WORKTREE_ALIAS_DIR: &str = "agentty-antigravity-worktrees";

/// Backend implementation for the Antigravity CLI.
///
/// Antigravity does not currently expose an ACP/app-server flag in `agy
/// --help`, so Agentty runs it as a stateless CLI provider through
/// `agy --print`. Prompts are streamed through stdin to avoid argv length
/// limits for transcript replay, large diffs, and one-shot utility prompts.
pub(super) struct AntigravityBackend;

impl AgentBackend for AntigravityBackend {
    fn setup(&self, folder: &Path) -> Result<(), AgentBackendError> {
        ensure_antigravity_project_state_ignored(folder)?;
        ensure_antigravity_workspace_alias(folder)?;

        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        let BuildCommandRequest {
            attachments,
            folder,
            prompt: _prompt,
            request_kind: _request_kind,
            model,
            reasoning_level: _reasoning_level,
        } = request;
        let mut command = Command::new("agy");

        ensure_antigravity_project_state_ignored(folder)?;
        let workspace_folder = ensure_antigravity_workspace_alias(folder)?;
        shared_prompt::append_cli_prompt_access_directories(
            &mut command,
            &workspace_folder,
            attachments,
            CliPromptAccessRootMode::WorkspaceThenAttachments,
        );

        command
            .arg("--sandbox")
            .arg("--dangerously-skip-permissions")
            .arg("--print")
            .arg("--print-timeout")
            .arg(ANTIGRAVITY_PRINT_TIMEOUT)
            .arg("--model")
            .arg(model)
            .current_dir(workspace_folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
}

/// Returns a workspace path Antigravity can mount for one session folder.
///
/// Antigravity rejects workspace roots whose path contains hidden components
/// such as Agentty's default `.agentty` home. For those worktrees, Agentty
/// creates a stable non-hidden symlink under the system temp directory and
/// gives Antigravity that alias as both its cwd and primary `--add-dir` root.
/// Git metadata updates still use the real worktree path.
///
/// # Errors
/// Returns an error when the folder needs an alias but the alias cannot be
/// created, verified, or placed at a non-hidden path.
fn ensure_antigravity_workspace_alias(folder: &Path) -> Result<PathBuf, AgentBackendError> {
    if !has_hidden_path_component(folder) {
        return Ok(folder.to_path_buf());
    }

    let alias = antigravity_workspace_alias_path(folder);
    if has_hidden_path_component(&alias) {
        return Err(AgentBackendError::Setup(format!(
            "Antigravity refuses hidden workspace folders and Agentty could not create a \
             non-hidden alias for `{}` because the temp alias path `{}` also contains a hidden \
             path component.",
            folder.display(),
            alias.display()
        )));
    }

    ensure_directory_symlink(folder, &alias).map_err(|error| {
        AgentBackendError::Setup(format!(
            "Antigravity refuses hidden workspace folders and Agentty failed to create a \
             non-hidden alias from `{}` to `{}`: {error}",
            alias.display(),
            folder.display()
        ))
    })?;

    Ok(alias)
}

/// Removes the Antigravity temp symlink alias for one session worktree.
///
/// The cleanup only removes the alias when it is a symlink that still points at
/// `folder`, preventing unrelated files at the deterministic alias path from
/// being deleted. After removing the symlink, this prunes the shared alias
/// directory only when no sibling aliases remain.
///
/// # Errors
/// Returns an error when the alias symlink or its now-empty parent directory
/// cannot be removed.
pub(crate) fn cleanup_workspace_alias(folder: &Path) -> Result<(), AgentBackendError> {
    if !has_hidden_path_component(folder) {
        return Ok(());
    }

    let alias = antigravity_workspace_alias_path(folder);
    let alias_target = match fs::read_link(&alias) {
        Ok(alias_target) => alias_target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => return Ok(()),
        Err(error) => {
            return Err(AgentBackendError::Setup(format!(
                "Failed to inspect Antigravity workspace alias `{}`: {error}",
                alias.display()
            )));
        }
    };

    if alias_target != folder {
        return Ok(());
    }

    fs::remove_file(&alias).map_err(|error| {
        AgentBackendError::Setup(format!(
            "Failed to remove Antigravity workspace alias `{}`: {error}",
            alias.display()
        ))
    })?;

    if let Some(parent) = alias.parent()
        && let Err(error) = fs::remove_dir(parent)
        && error.kind() != io::ErrorKind::NotFound
        && error.kind() != io::ErrorKind::DirectoryNotEmpty
    {
        return Err(AgentBackendError::Setup(format!(
            "Failed to prune Antigravity workspace alias directory `{}`: {error}",
            parent.display()
        )));
    }

    Ok(())
}

/// Returns whether a path contains a dot-prefixed component.
fn has_hidden_path_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if name.to_string_lossy().starts_with('.'))
    })
}

/// Builds the stable non-hidden alias path for one real worktree folder.
fn antigravity_workspace_alias_path(folder: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    folder.hash(&mut hasher);

    let folder_name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim_start_matches('.'))
        .filter(|name| !name.is_empty())
        .unwrap_or("worktree");
    let alias_name = format!("{folder_name}-{:016x}", hasher.finish());

    std::env::temp_dir()
        .join(ANTIGRAVITY_WORKTREE_ALIAS_DIR)
        .join(alias_name)
}

/// Ensures one directory symlink exists and points at the requested target.
///
/// # Errors
/// Returns an I/O error when the alias parent cannot be created, the alias path
/// is already occupied by another filesystem entry, or the symlink cannot be
/// created.
fn ensure_directory_symlink(target: &Path, link: &Path) -> Result<(), io::Error> {
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)?;
    }

    match fs::read_link(link) {
        Ok(existing_target) if existing_target == target => return Ok(()),
        Ok(existing_target) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "alias already points to `{}` instead of `{}`",
                    existing_target.display(),
                    target.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    create_directory_symlink(target, link)
}

#[cfg(unix)]
/// Creates a Unix directory symlink.
fn create_directory_symlink(target: &Path, link: &Path) -> Result<(), io::Error> {
    std::os::unix::fs::symlink(target, link)
}

/// Ensures Antigravity's workspace-local project state stays out of session
/// diffs.
///
/// `agy --print` creates `.antigravitycli/` and `cache/projects.json` as
/// project configuration state in the current workspace. Agentty stores
/// session output as git diffs and commits, so the backend adds
/// repository-local git exclude entries before the process can create those
/// paths. The exclude lives under git metadata, not in tracked project files.
///
/// # Errors
/// Returns an error when the session worktree's git exclude file cannot be
/// resolved or updated.
fn ensure_antigravity_project_state_ignored(folder: &Path) -> Result<(), AgentBackendError> {
    let Some(exclude_path) = git_info_exclude_path(folder)? else {
        return Ok(());
    };

    for pattern in ANTIGRAVITY_PROJECT_STATE_PATTERNS {
        append_git_exclude_pattern(&exclude_path, pattern)?;
    }

    Ok(())
}

/// Returns the git metadata exclude file used by one worktree.
///
/// Supports both regular repositories with a `.git/` directory and linked
/// worktrees whose `.git` file points at a worktree-specific gitdir. Linked
/// worktrees share ignore rules through their common gitdir, so this follows
/// `commondir` when git records one.
///
/// # Errors
/// Returns an error when git metadata exists but cannot be read.
fn git_info_exclude_path(folder: &Path) -> Result<Option<PathBuf>, AgentBackendError> {
    let git_path = folder.join(".git");
    let metadata = match fs::metadata(&git_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AgentBackendError::Setup(format!(
                "Failed to inspect Antigravity git metadata path `{}`: {error}",
                git_path.display()
            )));
        }
    };

    let git_dir = if metadata.is_dir() {
        git_path
    } else {
        let git_file = fs::read_to_string(&git_path).map_err(|error| {
            AgentBackendError::Setup(format!(
                "Failed to read Antigravity gitdir file `{}`: {error}",
                git_path.display()
            ))
        })?;
        let Some(git_dir) = parse_gitdir_file(folder, &git_file) else {
            return Ok(None);
        };
        git_dir
    };

    Ok(Some(git_common_info_exclude_path(&git_dir)?))
}

/// Returns the `info/exclude` path below a gitdir's common metadata directory.
///
/// # Errors
/// Returns an error when git's optional `commondir` file cannot be read.
fn git_common_info_exclude_path(git_dir: &Path) -> Result<PathBuf, AgentBackendError> {
    let common_dir_file = git_dir.join("commondir");
    let common_dir = match fs::read_to_string(&common_dir_file) {
        Ok(common_dir) => parse_common_dir_file(git_dir, &common_dir),
        Err(error) if error.kind() == io::ErrorKind::NotFound => git_dir.to_path_buf(),
        Err(error) => {
            return Err(AgentBackendError::Setup(format!(
                "Failed to read Antigravity git common-dir file `{}`: {error}",
                common_dir_file.display()
            )));
        }
    };

    Ok(common_dir.join("info").join("exclude"))
}

/// Parses a gitdir `commondir` file and resolves its target.
fn parse_common_dir_file(git_dir: &Path, common_dir_file: &str) -> PathBuf {
    let Some(common_dir) = common_dir_file.lines().next().map(str::trim) else {
        return git_dir.to_path_buf();
    };
    if common_dir.is_empty() {
        return git_dir.to_path_buf();
    }

    let common_dir = PathBuf::from(common_dir);
    if common_dir.is_absolute() {
        return common_dir;
    }

    git_dir.join(common_dir)
}

/// Parses a `.git` file and resolves its `gitdir:` target.
fn parse_gitdir_file(folder: &Path, git_file: &str) -> Option<PathBuf> {
    let git_dir = git_file.strip_prefix("gitdir:")?.trim();
    if git_dir.is_empty() {
        return None;
    }

    let git_dir = PathBuf::from(git_dir);
    if git_dir.is_absolute() {
        return Some(git_dir);
    }

    Some(folder.join(git_dir))
}

/// Appends one pattern to a git exclude file when it is not already present.
///
/// # Errors
/// Returns an error when the exclude directory cannot be created or the file
/// cannot be read or appended.
fn append_git_exclude_pattern(exclude_path: &Path, pattern: &str) -> Result<(), AgentBackendError> {
    let existing = match fs::read_to_string(exclude_path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(AgentBackendError::Setup(format!(
                "Failed to read Antigravity git exclude `{}`: {error}",
                exclude_path.display()
            )));
        }
    };

    if existing.lines().any(|line| line.trim() == pattern) {
        return Ok(());
    }

    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            AgentBackendError::Setup(format!(
                "Failed to create Antigravity git exclude directory `{}`: {error}",
                parent.display()
            ))
        })?;
    }

    let mut exclude_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(exclude_path)
        .map_err(|error| {
            AgentBackendError::Setup(format!(
                "Failed to open Antigravity git exclude `{}`: {error}",
                exclude_path.display()
            ))
        })?;

    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(exclude_file).map_err(|error| {
            AgentBackendError::Setup(format!(
                "Failed to update Antigravity git exclude `{}`: {error}",
                exclude_path.display()
            ))
        })?;
    }

    writeln!(
        exclude_file,
        "# Agentty: ignore Antigravity CLI workspace project state\n{pattern}"
    )
    .map_err(|error| {
        AgentBackendError::Setup(format!(
            "Failed to update Antigravity git exclude `{}`: {error}",
            exclude_path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::{TempDir, tempdir};

    use super::shared_prompt::{
        CliPromptAccessRootMode, ProtocolSchemaInstructionMode, build_prompt_stdin_payload,
        cli_prompt_access_directories,
    };
    use super::*;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::{AgentModel, ReasoningLevel};
    use crate::model::turn_prompt::TurnPromptAttachment;

    struct AliasCleanup {
        link: PathBuf,
    }

    impl AliasCleanup {
        /// Tracks one Antigravity alias path so tests remove global temp
        /// symlinks they create.
        fn new(folder: &Path) -> Self {
            Self {
                link: antigravity_workspace_alias_path(folder),
            }
        }
    }

    impl Drop for AliasCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.link);
        }
    }

    fn session_resume_request_kind(session_output: Option<&str>) -> AgentRequestKind {
        AgentRequestKind::SessionResume {
            session_output: session_output.map(ToString::to_string),
        }
    }

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    /// Creates a temp directory whose own basename is visible so no-alias
    /// command assertions are stable on platforms where `tempdir()` uses dot
    /// prefixes.
    fn visible_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("agentty-antigravity-test-")
            .tempdir()
            .expect("failed to create visible temp dir")
    }

    /// Creates a minimal standard git metadata directory for backend setup
    /// tests.
    fn create_standard_git_directory(folder: &Path) {
        fs::create_dir_all(folder.join(".git")).expect("failed to create git metadata directory");
    }

    /// Reads the repository-local git exclude file created by Antigravity
    /// setup.
    fn read_standard_git_exclude(folder: &Path) -> String {
        fs::read_to_string(folder.join(".git").join("info").join("exclude"))
            .expect("failed to read git exclude")
    }

    /// Verifies all Antigravity workspace-local state patterns are present in
    /// one git exclude file.
    fn assert_antigravity_project_state_patterns_ignored(exclude: &str) {
        for pattern in ANTIGRAVITY_PROJECT_STATE_PATTERNS {
            assert!(
                exclude.lines().any(|line| line.trim() == *pattern),
                "exclude should contain pattern `{pattern}`"
            );
        }
    }

    #[test]
    /// Verifies Antigravity starts in unattended print mode with sandbox
    /// restrictions enabled.
    fn test_antigravity_build_command_uses_print_mode_with_sandbox() {
        // Arrange
        let temp_directory = visible_tempdir();
        create_standard_git_directory(temp_directory.path());
        let backend = AntigravityBackend;
        let requested_model = AgentModel::Gemini31ProPreview.provider_model_str();

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "Write tests",
                request_kind: &session_start_request_kind(),
                model: requested_model,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let session_folder = temp_directory.path().to_string_lossy().into_owned();

        // Assert
        assert_eq!(
            args,
            vec![
                "--add-dir".to_string(),
                session_folder,
                "--sandbox".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "--print".to_string(),
                "--print-timeout".to_string(),
                ANTIGRAVITY_PRINT_TIMEOUT.to_string(),
                "--model".to_string(),
                requested_model.to_string(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
        assert_antigravity_project_state_patterns_ignored(&read_standard_git_exclude(
            temp_directory.path(),
        ));
    }

    #[test]
    /// Verifies Antigravity receives a non-hidden symlink alias when the real
    /// session worktree path contains a hidden component.
    fn test_antigravity_build_command_aliases_hidden_session_folder() {
        // Arrange
        let temp_directory = visible_tempdir();
        let session_folder = temp_directory
            .path()
            .join(".agentty")
            .join("wt")
            .join("00cbfefe");
        fs::create_dir_all(&session_folder).expect("failed to create hidden session folder");
        create_standard_git_directory(&session_folder);
        let _alias_cleanup = AliasCleanup::new(&session_folder);
        let backend = AntigravityBackend;
        let requested_model = AgentModel::Gemini31ProPreview.provider_model_str();

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: &session_folder,
                prompt: "Write tests",
                request_kind: &session_start_request_kind(),
                model: requested_model,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let alias = antigravity_workspace_alias_path(&session_folder);

        // Assert
        assert!(!has_hidden_path_component(&alias));
        assert_eq!(
            fs::read_link(&alias).expect("alias should exist"),
            session_folder
        );
        assert_eq!(command.get_current_dir(), Some(alias.as_path()));
        assert_eq!(args[0], "--add-dir");
        assert_eq!(args[1], alias.to_string_lossy());
        assert_antigravity_project_state_patterns_ignored(&read_standard_git_exclude(
            &session_folder,
        ));
    }

    #[test]
    /// Verifies Antigravity setup excludes workspace-local CLI state for
    /// standard repositories.
    fn test_antigravity_setup_ignores_project_state_for_standard_git_directory() {
        // Arrange
        let temp_directory = visible_tempdir();
        create_standard_git_directory(temp_directory.path());
        let backend = AntigravityBackend;

        // Act
        AgentBackend::setup(&backend, temp_directory.path()).expect("setup should succeed");
        let exclude = read_standard_git_exclude(temp_directory.path());

        // Assert
        assert!(exclude.contains("# Agentty: ignore Antigravity CLI workspace project state"));
        assert_antigravity_project_state_patterns_ignored(&exclude);
    }

    #[test]
    /// Verifies Antigravity setup prepares a non-hidden alias before the first
    /// command is built for a hidden session worktree path.
    fn test_antigravity_setup_aliases_hidden_session_folder() {
        // Arrange
        let temp_directory = visible_tempdir();
        let session_folder = temp_directory
            .path()
            .join(".agentty")
            .join("wt")
            .join("00cbfefe");
        fs::create_dir_all(&session_folder).expect("failed to create hidden session folder");
        create_standard_git_directory(&session_folder);
        let _alias_cleanup = AliasCleanup::new(&session_folder);
        let backend = AntigravityBackend;

        // Act
        AgentBackend::setup(&backend, &session_folder).expect("setup should succeed");
        let alias = antigravity_workspace_alias_path(&session_folder);

        // Assert
        assert!(!has_hidden_path_component(&alias));
        assert_eq!(
            fs::read_link(alias).expect("alias should exist"),
            session_folder
        );
    }

    #[test]
    /// Verifies Antigravity cleanup removes the non-hidden alias created for a
    /// hidden session worktree path.
    fn test_cleanup_workspace_alias_removes_hidden_session_alias() {
        // Arrange
        let temp_directory = visible_tempdir();
        let session_folder = temp_directory
            .path()
            .join(".agentty")
            .join("wt")
            .join("00cbfefe");
        fs::create_dir_all(&session_folder).expect("failed to create hidden session folder");
        create_standard_git_directory(&session_folder);
        let backend = AntigravityBackend;
        AgentBackend::setup(&backend, &session_folder).expect("setup should succeed");
        let alias = antigravity_workspace_alias_path(&session_folder);

        // Act
        cleanup_workspace_alias(&session_folder).expect("cleanup should succeed");

        // Assert
        let error = fs::read_link(alias).expect_err("alias should be removed");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    /// Verifies Antigravity cleanup leaves an alias path alone when it points
    /// at a different target.
    fn test_cleanup_workspace_alias_preserves_unrelated_symlink() {
        // Arrange
        let temp_directory = visible_tempdir();
        let session_folder = temp_directory
            .path()
            .join(".agentty")
            .join("wt")
            .join("00cbfefe");
        let other_folder = temp_directory.path().join("other-worktree");
        fs::create_dir_all(&session_folder).expect("failed to create hidden session folder");
        fs::create_dir_all(&other_folder).expect("failed to create other folder");
        let alias = antigravity_workspace_alias_path(&session_folder);
        fs::create_dir_all(alias.parent().expect("alias should have parent"))
            .expect("failed to create alias parent");
        create_directory_symlink(&other_folder, &alias).expect("failed to create unrelated alias");
        let _alias_cleanup = AliasCleanup::new(&session_folder);

        // Act
        cleanup_workspace_alias(&session_folder).expect("cleanup should succeed");

        // Assert
        assert_eq!(
            fs::read_link(alias).expect("alias should remain"),
            other_folder
        );
    }

    #[test]
    /// Verifies Antigravity setup follows linked-worktree `.git` files and
    /// `commondir` metadata to the repository-local exclude file used by git.
    fn test_antigravity_setup_ignores_project_state_for_linked_worktree_gitdir() {
        // Arrange
        let temp_directory = visible_tempdir();
        let common_git_dir = temp_directory.path().join("main").join(".git");
        let worktree = temp_directory.path().join("worktree");
        let worktree_git_dir = common_git_dir.join("worktrees").join("feature");
        fs::create_dir_all(&worktree).expect("failed to create worktree directory");
        fs::create_dir_all(&worktree_git_dir).expect("failed to create gitdir directory");
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git_dir.display()),
        )
        .expect("failed to write linked worktree gitdir file");
        fs::write(worktree_git_dir.join("commondir"), "../..\n")
            .expect("failed to write linked worktree commondir file");
        let backend = AntigravityBackend;

        // Act
        AgentBackend::setup(&backend, &worktree).expect("setup should succeed");
        let exclude = fs::read_to_string(common_git_dir.join("info").join("exclude"))
            .expect("failed to read linked worktree git exclude");

        // Assert
        assert_antigravity_project_state_patterns_ignored(&exclude);
    }

    #[test]
    /// Verifies repeated Antigravity setup keeps one copy of each exclude
    /// pattern.
    fn test_antigravity_setup_ignores_project_state_idempotently() {
        // Arrange
        let temp_directory = visible_tempdir();
        create_standard_git_directory(temp_directory.path());
        let backend = AntigravityBackend;

        // Act
        AgentBackend::setup(&backend, temp_directory.path()).expect("first setup should succeed");
        AgentBackend::setup(&backend, temp_directory.path()).expect("second setup should succeed");
        let exclude = read_standard_git_exclude(temp_directory.path());

        // Assert
        for pattern in ANTIGRAVITY_PROJECT_STATE_PATTERNS {
            let pattern_count = exclude
                .lines()
                .filter(|line| line.trim() == *pattern)
                .count();
            assert_eq!(pattern_count, 1, "pattern `{pattern}` should appear once");
        }
    }

    #[test]
    /// Verifies Antigravity grants workspace roots for the session folder and
    /// external prompt image attachments.
    fn test_antigravity_build_command_adds_workspace_directories() {
        // Arrange
        let temp_directory = visible_tempdir();
        let attachment_directory = temp_directory.path().join("images");
        let attachment = TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: attachment_directory.join("one.png"),
        };
        let backend = AntigravityBackend;
        let requested_model = "gemini-3.5-flash";

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[attachment],
                folder: temp_directory.path(),
                prompt: "Review [Image #1]",
                request_kind: &session_start_request_kind(),
                model: requested_model,
                reasoning_level: ReasoningLevel::default(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let expected_workspace_args = vec![
            "--add-dir".to_string(),
            temp_directory.path().to_string_lossy().into_owned(),
            "--add-dir".to_string(),
            attachment_directory.to_string_lossy().into_owned(),
        ];
        let expected_model_args = ["--model".to_string(), requested_model.to_string()];

        // Assert
        assert_eq!(args[..4], expected_workspace_args);
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == expected_model_args[0] && pair[1] == expected_model_args[1]),
            "command should include requested model"
        );
    }

    #[test]
    /// Verifies Antigravity keeps the session worktree as the primary
    /// workspace even when an attachment directory sorts before it.
    fn test_workspace_access_directories_keep_session_folder_first() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let session_folder = temp_directory.path().join("z-session");
        let attachment_directory = temp_directory.path().join("a-images");
        let attachment = TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: attachment_directory.join("one.png"),
        };

        // Act
        let workspace_directories = cli_prompt_access_directories(
            &session_folder,
            &[attachment],
            CliPromptAccessRootMode::WorkspaceThenAttachments,
        );

        // Assert
        assert_eq!(
            workspace_directories,
            vec![session_folder, attachment_directory]
        );
    }

    #[test]
    /// Verifies Antigravity stdin prompts include protocol instructions and
    /// replayed transcript output for resume turns.
    fn test_antigravity_stdin_payload_replays_session_output_on_resume() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let request_kind = session_resume_request_kind(Some("previous answer"));

        // Act
        let payload = build_prompt_stdin_payload(
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                prompt: "continue work",
                request_kind: &request_kind,
                model: AgentModel::Gemini31ProPreview.provider_model_str(),
                reasoning_level: ReasoningLevel::default(),
            },
            ProtocolSchemaInstructionMode::PromptSchema,
            "Antigravity",
        )
        .expect("stdin payload should build");
        let prompt = String::from_utf8(payload).expect("prompt should be utf8");

        // Assert
        assert!(prompt.contains("Structured response protocol:"));
        assert!(prompt.contains("previous answer"));
        assert!(prompt.contains("continue work"));
    }
}
