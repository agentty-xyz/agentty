use std::io::Write as _;
use std::path::{Path, PathBuf};
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

        Ok(())
    }

    fn build_command<'request>(
        &'request self,
        request: BuildCommandRequest<'request>,
    ) -> Result<Command, AgentBackendError> {
        let BuildCommandRequest {
            attachments,
            folder,
            main_checkout_root: _main_checkout_root,
            model,
            prompt: _prompt,
            request_kind: _request_kind,
            replay_transcript: _replay_transcript,
            reasoning_level,
            ..
        } = request;
        let mut command = Command::new("agy");

        ensure_antigravity_project_state_ignored(folder)?;
        shared_prompt::append_cli_prompt_access_directories(
            &mut command,
            folder,
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
            .arg("--effort")
            .arg(reasoning_level.antigravity())
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(command)
    }
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
    use ag_protocol::{ProtocolSchemaInstructionMode, TurnPromptAttachment};
    use tempfile::{TempDir, tempdir};

    use super::shared_prompt::{
        CliPromptAccessRootMode, build_prompt_stdin_payload, cli_prompt_access_directories,
    };
    use super::*;
    use crate::channel::AgentRequestKind;
    use crate::model::agent::{AgentModel, ReasoningLevel};

    fn session_resume_request_kind(_replay_transcript: Option<&str>) -> AgentRequestKind {
        AgentRequestKind::SessionResume
    }

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
    }

    /// Creates a temp directory whose own basename is visible so command
    /// assertions are stable on platforms where `tempdir()` uses dot prefixes.
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
                main_checkout_root: None,
                replay_transcript: None,
                model: requested_model,
                personality_prompt: None,
                prompt: "Write tests",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
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
                "--effort".to_string(),
                ReasoningLevel::default().antigravity().to_string(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(temp_directory.path()));
        assert_antigravity_project_state_patterns_ignored(&read_standard_git_exclude(
            temp_directory.path(),
        ));
    }

    #[test]
    /// Verifies Antigravity receives every supported effort and caps higher
    /// Agentty reasoning levels at the CLI's highest accepted value.
    fn test_antigravity_build_command_passes_supported_effort() {
        // Arrange
        let temp_directory = visible_tempdir();
        let backend = AntigravityBackend;
        let cases = ReasoningLevel::ALL;

        for reasoning_level in cases {
            // Act
            let command = AgentBackend::build_command(
                &backend,
                BuildCommandRequest {
                    attachments: &[],
                    folder: temp_directory.path(),
                    main_checkout_root: None,
                    replay_transcript: None,
                    model: AgentModel::Gemini31ProPreview.provider_model_str(),
                    personality_prompt: None,
                    prompt: "Write tests",
                    reasoning_level,
                    request_kind: &session_start_request_kind(),
                },
            )
            .expect("command should build");
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let effort_position = args
                .iter()
                .position(|arg| arg == "--effort")
                .expect("--effort flag should be present");

            // Assert
            assert_eq!(args[effort_position + 1], reasoning_level.antigravity());
        }
    }

    #[test]
    /// Verifies Antigravity receives the real session worktree even when its
    /// path contains a hidden component.
    fn test_antigravity_build_command_uses_hidden_session_folder_directly() {
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
        let requested_model = AgentModel::Gemini31ProPreview.provider_model_str();

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[],
                folder: &session_folder,
                main_checkout_root: None,
                replay_transcript: None,
                model: requested_model,
                personality_prompt: None,
                prompt: "Write tests",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
            },
        )
        .expect("command should build");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let session_folder_display = session_folder.to_string_lossy().into_owned();

        // Assert
        assert_eq!(command.get_current_dir(), Some(session_folder.as_path()));
        assert_eq!(args[0], "--add-dir");
        assert_eq!(args[1], session_folder_display);
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
    /// Verifies Antigravity setup accepts a hidden session worktree path.
    fn test_antigravity_setup_accepts_hidden_session_folder() {
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

        // Act
        AgentBackend::setup(&backend, &session_folder).expect("setup should succeed");
        let exclude = read_standard_git_exclude(&session_folder);

        // Assert
        assert_antigravity_project_state_patterns_ignored(&exclude);
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
        let requested_model = "gemini-3.6-flash";

        // Act
        let command = AgentBackend::build_command(
            &backend,
            BuildCommandRequest {
                attachments: &[attachment],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: None,
                model: requested_model,
                personality_prompt: None,
                prompt: "Review [Image #1]",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &session_start_request_kind(),
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
    fn test_antigravity_stdin_payload_replays_transcript_on_resume() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
        let request_kind = session_resume_request_kind(Some("previous answer"));

        // Act
        let payload = build_prompt_stdin_payload(
            BuildCommandRequest {
                attachments: &[],
                folder: temp_directory.path(),
                main_checkout_root: None,
                replay_transcript: Some("previous answer"),
                model: AgentModel::Gemini31ProPreview.provider_model_str(),
                personality_prompt: None,
                prompt: "continue work",
                reasoning_level: ReasoningLevel::default(),
                request_kind: &request_kind,
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
