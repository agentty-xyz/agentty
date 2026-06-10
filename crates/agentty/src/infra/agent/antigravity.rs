use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{fs, io};

use super::backend::{AgentBackend, AgentBackendError, BuildCommandRequest};
use super::prompt::{PromptPreparationRequest, ProtocolSchemaInstructionMode, prepare_prompt_text};
use crate::domain::turn_prompt::{
    TurnPromptAttachment, TurnPromptContentPart, split_turn_prompt_content,
};

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
        ensure_antigravity_project_state_ignored(folder)
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
        append_workspace_access_directories(&mut command, folder, attachments);

        command
            .arg("--sandbox")
            .arg("--dangerously-skip-permissions")
            .arg("--print")
            .arg("--print-timeout")
            .arg(ANTIGRAVITY_PRINT_TIMEOUT)
            .arg("--model")
            .arg(model)
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

/// Renders the full Antigravity prompt text that Agentty streams through
/// stdin.
///
/// # Errors
/// Returns an error when image placeholder rendering or protocol prompt
/// rendering fails.
pub(super) fn build_prompt_stdin_payload(
    request: BuildCommandRequest<'_>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
) -> Result<Vec<u8>, AgentBackendError> {
    let prompt = render_prompt_with_local_images(request.prompt, request.attachments)?;
    let prompt = prepare_prompt_text(PromptPreparationRequest {
        instruction_delivery_mode: if request.request_kind.is_resume() {
            super::instruction::InstructionDeliveryMode::BootstrapWithReplay
        } else {
            super::instruction::InstructionDeliveryMode::BootstrapFull
        },
        prompt: &prompt,
        protocol_profile: request.request_kind.protocol_profile(),
        replay_session_output: request.request_kind.session_output(),
        schema_instruction_mode,
    })?;

    Ok(prompt.into_bytes())
}

/// Adds Antigravity workspace roots for the session folder and prompt
/// attachments.
///
/// `agy --print` uses `--add-dir` rather than the process working directory to
/// decide which folders tools can read and write. Agentty keeps the session
/// worktree as the first workspace root, then adds pasted-image parent
/// directories that live under Agentty's temp directory so Antigravity can
/// inspect those local files without replacing the active editable workspace.
fn append_workspace_access_directories(
    command: &mut Command,
    folder: &Path,
    attachments: &[TurnPromptAttachment],
) {
    for workspace_directory in workspace_access_directories(folder, attachments) {
        command.arg("--add-dir").arg(workspace_directory);
    }
}

/// Returns the Antigravity workspace roots required by one turn.
///
/// The session worktree is always first because Antigravity derives editable
/// workspace behavior from the ordered `--add-dir` roots. Attachment
/// directories are sorted after the session root for deterministic arguments.
fn workspace_access_directories(
    folder: &Path,
    attachments: &[TurnPromptAttachment],
) -> Vec<PathBuf> {
    let folder = folder.to_path_buf();
    let mut attachment_directories = attachments
        .iter()
        .filter_map(|attachment| attachment.local_image_path.parent())
        .map(std::path::Path::to_path_buf)
        .filter(|attachment_directory| attachment_directory != &folder)
        .collect::<Vec<_>>();
    attachment_directories.sort();
    attachment_directories.dedup();

    let mut workspace_directories = Vec::with_capacity(attachment_directories.len() + 1);
    workspace_directories.push(folder);
    workspace_directories.extend(attachment_directories);

    workspace_directories
}

/// Replaces inline prompt-image placeholders with Antigravity-readable local
/// image paths while preserving attachment order.
///
/// # Errors
/// Returns an error when any attachment path is not valid UTF-8, because the
/// prompt protocol can only carry UTF-8 text and lossy conversion could point
/// Antigravity at the wrong file.
fn render_prompt_with_local_images(
    prompt: &str,
    attachments: &[TurnPromptAttachment],
) -> Result<String, AgentBackendError> {
    if attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let mut rendered_prompt = String::new();

    for content_part in split_turn_prompt_content(prompt, attachments) {
        match content_part {
            TurnPromptContentPart::Text(text) => rendered_prompt.push_str(text),
            TurnPromptContentPart::Attachment(attachment) => {
                let attachment_path = attachment_path_for_prompt(attachment)?;
                rendered_prompt.push_str(&attachment_path);
            }
            TurnPromptContentPart::OrphanAttachment(attachment) => {
                if !rendered_prompt.is_empty()
                    && rendered_prompt
                        .chars()
                        .last()
                        .is_some_and(|character| !character.is_whitespace())
                {
                    rendered_prompt.push('\n');
                }

                rendered_prompt.push_str(&attachment_path_for_prompt(attachment)?);
                rendered_prompt.push('\n');
            }
        }
    }

    Ok(rendered_prompt)
}

/// Returns one prompt attachment path as strict UTF-8 for Antigravity stdin
/// rendering.
fn attachment_path_for_prompt(
    attachment: &TurnPromptAttachment,
) -> Result<String, AgentBackendError> {
    attachment
        .local_image_path
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AgentBackendError::CommandBuild(
                "Antigravity prompt image path is not valid UTF-8".to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;
    use crate::domain::agent::{AgentModel, ReasoningLevel};
    use crate::infra::channel::AgentRequestKind;

    fn session_resume_request_kind(session_output: Option<&str>) -> AgentRequestKind {
        AgentRequestKind::SessionResume {
            session_output: session_output.map(ToString::to_string),
        }
    }

    fn session_start_request_kind() -> AgentRequestKind {
        AgentRequestKind::SessionStart
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
        let temp_directory = tempdir().expect("failed to create temp dir");
        create_standard_git_directory(temp_directory.path());
        let backend = AntigravityBackend;
        let requested_model = AgentModel::AntigravityGemini31ProPreview.provider_model_str();

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
    /// Verifies Antigravity setup excludes workspace-local CLI state for
    /// standard repositories.
    fn test_antigravity_setup_ignores_project_state_for_standard_git_directory() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
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
    /// Verifies Antigravity setup follows linked-worktree `.git` files and
    /// `commondir` metadata to the repository-local exclude file used by git.
    fn test_antigravity_setup_ignores_project_state_for_linked_worktree_gitdir() {
        // Arrange
        let temp_directory = tempdir().expect("failed to create temp dir");
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
        let temp_directory = tempdir().expect("failed to create temp dir");
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
        let temp_directory = tempdir().expect("failed to create temp dir");
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
        let workspace_directories = workspace_access_directories(&session_folder, &[attachment]);

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
                model: AgentModel::AntigravityGemini31ProPreview.provider_model_str(),
                reasoning_level: ReasoningLevel::default(),
            },
            ProtocolSchemaInstructionMode::PromptSchema,
        )
        .expect("stdin payload should build");
        let prompt = String::from_utf8(payload).expect("prompt should be utf8");

        // Assert
        assert!(prompt.contains("Structured response protocol:"));
        assert!(prompt.contains("previous answer"));
        assert!(prompt.contains("continue work"));
    }

    #[test]
    /// Verifies image placeholders are replaced before the prompt is sent to
    /// Antigravity.
    fn test_render_prompt_with_local_images_replaces_placeholders_in_order() {
        // Arrange
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/one.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/two.png"),
            },
        ];

        // Act
        let rendered_prompt =
            render_prompt_with_local_images("Compare [Image #2] with [Image #1]", &attachments)
                .expect("prompt should render");

        // Assert
        assert_eq!(rendered_prompt, "Compare /tmp/two.png with /tmp/one.png");
    }
}
