//! Shared prompt-shaping helpers for agent-facing markdown prompts.

use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) use ag_protocol::ProtocolSchemaInstructionMode;
use ag_protocol::{
    ProtocolRequestProfile, prepend_protocol_instructions as protocol_prepend_instructions,
    prepend_protocol_refresh_reminder as protocol_prepend_refresh_reminder,
};
use askama::Template;

use super::backend::{AgentBackendError, BuildCommandRequest};
use super::instruction::InstructionDeliveryMode;
use crate::domain::turn_prompt::{
    TurnPromptAttachment, TurnPromptContentPart, split_turn_prompt_content,
};

/// Askama view model for rendering resume prompts with prior session output.
#[derive(Template)]
#[template(path = "resume_with_session_output_prompt.md", escape = "none")]
struct ResumeWithSessionOutputPromptTemplate<'a> {
    /// New prompt content appended after the replayed transcript.
    prompt: &'a str,
    /// Prior session output replayed into the follow-up prompt.
    session_output: &'a str,
}

/// Shared prompt preparation input for one transport turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptPreparationRequest<'a> {
    /// Delivery mode selected for the current provider attempt.
    pub instruction_delivery_mode: InstructionDeliveryMode,
    /// Base user prompt before replay wrapping and protocol instructions.
    pub prompt: &'a str,
    /// Protocol family that determines the rendered instruction envelope.
    pub protocol_profile: ProtocolRequestProfile,
    /// Prior session output available for transcript replay.
    pub replay_session_output: Option<&'a str>,
    /// Schema guidance mode selected from the provider's structured-output
    /// capability.
    pub schema_instruction_mode: ProtocolSchemaInstructionMode,
}

/// Controls which directories CLI prompt transports expose as filesystem access
/// roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliPromptAccessRootMode {
    /// Expose only attachment parent directories.
    AttachmentsOnly,
    /// Expose the workspace folder first, then attachment parent directories.
    WorkspaceThenAttachments,
}

/// Applies transcript replay and protocol instructions to one prompt.
///
/// # Errors
/// Returns an error when replay or instruction templates fail to render.
pub(crate) fn prepare_prompt_text(
    request: PromptPreparationRequest<'_>,
) -> Result<String, AgentBackendError> {
    match request.instruction_delivery_mode {
        InstructionDeliveryMode::BootstrapFull => Ok(protocol_prepend_instructions(
            request.prompt,
            request.protocol_profile,
            request.schema_instruction_mode,
        )),
        InstructionDeliveryMode::DeltaOnly => Ok(protocol_prepend_refresh_reminder(
            request.prompt,
            request.protocol_profile,
        )),
        InstructionDeliveryMode::BootstrapWithReplay => {
            let prompt = build_resume_prompt(request.prompt, request.replay_session_output)?;

            Ok(protocol_prepend_instructions(
                &prompt,
                request.protocol_profile,
                request.schema_instruction_mode,
            ))
        }
    }
}

/// Builds a resume prompt that optionally prepends previous session output.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
pub(crate) fn build_resume_prompt(
    prompt: &str,
    session_output: Option<&str>,
) -> Result<String, AgentBackendError> {
    let Some(session_output) = session_output
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(prompt.to_string());
    };

    let template = ResumeWithSessionOutputPromptTemplate {
        prompt,
        session_output,
    };

    render_template("resume_with_session_output_prompt.md", &template)
}

/// Builds a full prompt payload to stream over stdin for CLI providers.
///
/// This shared helper keeps attachment placeholder rendering and provider
/// protocol preparation in one place while preserving backend-specific error
/// labels.
///
/// # Errors
/// Returns an error when attachment path rendering, resume wrapping, or
/// protocol prompt rendering fails.
pub(crate) fn build_prompt_stdin_payload(
    request: BuildCommandRequest<'_>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    backend_display_name: &str,
) -> Result<Vec<u8>, AgentBackendError> {
    let prompt =
        render_prompt_with_local_images(request.prompt, request.attachments, backend_display_name)?;
    let prompt = prepare_prompt_text(PromptPreparationRequest {
        instruction_delivery_mode: if request.request_kind.is_resume() {
            InstructionDeliveryMode::BootstrapWithReplay
        } else {
            InstructionDeliveryMode::BootstrapFull
        },
        prompt: &prompt,
        protocol_profile: request.request_kind.protocol_profile(),
        replay_session_output: request.request_kind.session_output(),
        schema_instruction_mode,
    })?;

    Ok(prompt.into_bytes())
}

/// Appends CLI prompt filesystem access roots as `--add-dir` arguments.
///
/// Claude only needs pasted-image parent directories because its process
/// working directory is already the session workspace. Antigravity derives its
/// editable workspace from ordered `--add-dir` roots, so it uses
/// [`CliPromptAccessRootMode::WorkspaceThenAttachments`] to keep the workspace
/// root first.
pub(crate) fn append_cli_prompt_access_directories(
    command: &mut Command,
    workspace_folder: &Path,
    attachments: &[TurnPromptAttachment],
    root_mode: CliPromptAccessRootMode,
) {
    for directory in cli_prompt_access_directories(workspace_folder, attachments, root_mode) {
        command.arg("--add-dir").arg(directory);
    }
}

/// Replaces inline image placeholders with provider-usable local image paths.
///
/// The function preserves attachment ordering through prompt content parsing
/// and appends any orphaned attachments that no longer have a placeholder in
/// the prompt text.
///
/// # Errors
/// Returns an error when any local image path is not valid UTF-8.
pub(crate) fn render_prompt_with_local_images(
    prompt: &str,
    attachments: &[TurnPromptAttachment],
    backend_display_name: &str,
) -> Result<String, AgentBackendError> {
    if attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let mut rendered_prompt = String::new();

    for content_part in split_turn_prompt_content(prompt, attachments) {
        match content_part {
            TurnPromptContentPart::Text(text) => rendered_prompt.push_str(text),
            TurnPromptContentPart::Attachment(attachment) => {
                let attachment_path = attachment_path_for_prompt(backend_display_name, attachment)?;
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

                rendered_prompt.push_str(&attachment_path_for_prompt(
                    backend_display_name,
                    attachment,
                )?);
                rendered_prompt.push('\n');
            }
        }
    }

    Ok(rendered_prompt)
}

/// Returns ordered filesystem access roots for CLI prompt image access.
///
/// Directory paths are deduplicated and sorted for deterministic subprocess
/// argument ordering. When `root_mode` requests the workspace, the session
/// folder appears before attachment directories and is never duplicated.
pub(crate) fn cli_prompt_access_directories(
    workspace_folder: &Path,
    attachments: &[TurnPromptAttachment],
    root_mode: CliPromptAccessRootMode,
) -> Vec<PathBuf> {
    let mut attachment_directories = attachments
        .iter()
        .filter_map(|attachment| attachment.local_image_path.parent())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    attachment_directories.sort();
    attachment_directories.dedup();

    if matches!(root_mode, CliPromptAccessRootMode::AttachmentsOnly) {
        return attachment_directories;
    }

    attachment_directories
        .retain(|attachment_directory| attachment_directory.as_path() != workspace_folder);

    let mut workspace_directories = Vec::with_capacity(attachment_directories.len() + 1);
    workspace_directories.push(workspace_folder.to_path_buf());
    workspace_directories.extend(attachment_directories);

    workspace_directories
}

/// Returns one attachment path for prompt injection as strict UTF-8 text.
///
/// # Errors
/// Returns an error when the attachment path cannot be represented as UTF-8.
fn attachment_path_for_prompt(
    backend_display_name: &str,
    attachment: &TurnPromptAttachment,
) -> Result<String, AgentBackendError> {
    attachment
        .local_image_path
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AgentBackendError::CommandBuild(format!(
                "{backend_display_name} prompt image path is not valid UTF-8"
            ))
        })
}

/// Builds a Markdown code-fence delimiter long enough to safely wrap an
/// arbitrary diff payload.
///
/// Returns a string of backticks whose length exceeds the longest run of
/// consecutive backticks found anywhere in `content`, with a minimum length
/// of three. This prevents a triple-backtick fence from being terminated
/// prematurely when the diff itself contains Markdown fences (for example,
/// when reviewing changes to Markdown or prompt-template files).
pub(crate) fn diff_fence(content: &str) -> String {
    let mut max_run = 0usize;
    let mut current_run = 0usize;
    for character in content.chars() {
        if character == '`' {
            current_run += 1;
            if current_run > max_run {
                max_run = current_run;
            }
        } else {
            current_run = 0;
        }
    }

    let fence_length = std::cmp::max(3, max_run + 1);

    "`".repeat(fence_length)
}

/// Renders one Askama markdown template and trims the trailing newline added
/// by file-based templates.
fn render_template(
    template_name: &str,
    template: &impl Template,
) -> Result<String, AgentBackendError> {
    let rendered = template.render().map_err(|error| {
        AgentBackendError::CommandBuild(format!("Failed to render `{template_name}`: {error}"))
    })?;

    Ok(rendered.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use super::*;

    #[test]
    /// Ensures the diff fence falls back to three backticks when the content
    /// contains no backtick runs.
    fn test_diff_fence_returns_minimum_three_backticks_for_plain_diff() {
        // Arrange
        let diff = "diff --git a/a.rs b/a.rs\n+fn main() {}\n";

        // Act
        let fence = diff_fence(diff);

        // Assert
        assert_eq!(fence, "```");
    }

    #[test]
    /// Ensures the diff fence grows to exceed the longest backtick run in the
    /// diff so a Markdown triple-backtick fence inside the diff cannot
    /// terminate the outer wrapper fence.
    fn test_diff_fence_exceeds_longest_backtick_run_in_diff() {
        // Arrange
        let diff = "+```\nsample\n+```\n";

        // Act
        let fence = diff_fence(diff);

        // Assert
        assert_eq!(fence, "````");
    }

    #[test]
    /// Ensures longer backtick runs keep producing a strictly longer fence so
    /// nested or unusually long code fences in the diff stay contained.
    fn test_diff_fence_handles_long_backtick_runs() {
        // Arrange
        let diff = "prefix `````diff\ncontent\n`````\n";

        // Act
        let fence = diff_fence(diff);

        // Assert
        assert_eq!(fence, "``````");
    }

    #[test]
    /// Ensures resume prompt rendering includes trimmed session output and
    /// the new user prompt.
    fn test_build_resume_prompt_includes_session_output_and_prompt() {
        // Arrange
        let prompt = "Continue and update tests";
        let session_output = Some("  previous output line  \n");

        // Act
        let resume_prompt =
            build_resume_prompt(prompt, session_output).expect("resume prompt should render");

        // Assert
        let normalized_resume_prompt = resume_prompt.split_whitespace().collect::<Vec<_>>();
        let normalized_resume_prompt = normalized_resume_prompt.join(" ");
        assert!(resume_prompt.contains("previous output line"));
        assert!(normalized_resume_prompt.contains("Treat the user's new prompt as a follow-up"));
        assert!(normalized_resume_prompt.contains("changes made during this Agentty session"));
        assert!(normalized_resume_prompt.contains("preserve unrelated pre-existing work"));
        assert!(resume_prompt.contains("Continue and update tests"));
    }

    #[test]
    /// Ensures whitespace-only session output does not trigger transcript
    /// wrapping and returns the original prompt.
    fn test_build_resume_prompt_returns_original_prompt_when_output_is_blank() {
        // Arrange
        let prompt = "Follow-up request";
        let session_output = Some("   ");

        // Act
        let resume_prompt =
            build_resume_prompt(prompt, session_output).expect("resume prompt should render");

        // Assert
        assert_eq!(resume_prompt, prompt);
    }

    #[test]
    /// Ensures absent session output keeps resume prompt formatting unchanged.
    fn test_build_resume_prompt_returns_original_prompt_without_output() {
        // Arrange
        let prompt = "Retry merge";

        // Act
        let resume_prompt = build_resume_prompt(prompt, None).expect("resume prompt should render");

        // Assert
        assert_eq!(resume_prompt, prompt);
    }

    #[test]
    /// Ensures session prompts include the critical protocol contract markers.
    fn test_prepend_protocol_instructions_adds_session_protocol_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt = protocol_prepend_instructions(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::PromptSchema,
        );

        // Assert
        assert!(rendered_prompt.contains("File path output requirements:"));
        assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
        assert!(rendered_prompt.contains("Paths must be relative to the repository root."));
        assert!(rendered_prompt.contains("If you run git commands, use read-only commands only"));
        assert!(rendered_prompt.contains("Do not run mutating git commands"));
        assert!(rendered_prompt.contains("Quality check requirements:"));
        assert!(rendered_prompt.contains("repository-defined quality checks"));
        let normalized_rendered_prompt = rendered_prompt.split_whitespace().collect::<Vec<_>>();
        let normalized_rendered_prompt = normalized_rendered_prompt.join(" ");
        assert!(normalized_rendered_prompt.contains("affected dependencies and dependents"));
        assert!(rendered_prompt.contains("full repository test/check suite"));
        assert!(rendered_prompt.contains("Remove any temporary scripts or files"));
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(rendered_prompt.contains("Return a single JSON object"));
        assert!(rendered_prompt.contains("Do not wrap the JSON in markdown code fences."));
        assert!(rendered_prompt.contains("Follow this JSON Schema exactly."));
        assert!(rendered_prompt.contains("Treat the JSON Schema titles and descriptions"));
        assert!(rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(rendered_prompt.contains("---"));
        assert!(rendered_prompt.contains("For this session turn"));
        assert!(normalized_rendered_prompt.contains("Do not create commits"));
        assert!(normalized_rendered_prompt.contains("suggest creating commits"));
        assert!(rendered_prompt.contains("summary"));
        assert!(rendered_prompt.contains("turn"));
        assert!(rendered_prompt.contains("session"));
        assert!(rendered_prompt.contains("\"answer\""));
        assert!(rendered_prompt.contains("\"questions\""));
        assert!(rendered_prompt.contains("\"title\""));
        assert!(rendered_prompt.contains("\"description\""));
        assert!(rendered_prompt.contains("summary"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures schema-enforcing transports get protocol policy without the
    /// large prompt-side JSON Schema body.
    fn test_prepend_protocol_instructions_omits_schema_for_transport_schema_mode() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt = protocol_prepend_instructions(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::TransportSchema,
        );

        // Assert
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(rendered_prompt.contains("provider enforces Agentty's response JSON schema"));
        assert!(rendered_prompt.contains("Return a single JSON object"));
        assert!(!rendered_prompt.contains("Follow this JSON Schema exactly."));
        assert!(!rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures protocol instructions are not duplicated when already present.
    fn test_prepend_protocol_instructions_is_idempotent() {
        // Arrange
        let prompt = protocol_prepend_instructions(
            "Implement feature",
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::PromptSchema,
        );

        // Act
        let rendered_prompt = protocol_prepend_instructions(
            &prompt,
            ProtocolRequestProfile::UtilityPrompt,
            ProtocolSchemaInstructionMode::TransportSchema,
        );

        // Assert
        assert_eq!(rendered_prompt, prompt);
    }

    #[test]
    /// Ensures one-shot prompts reuse the shared full-schema protocol
    /// instructions.
    fn test_prepend_protocol_instructions_reuses_same_contract_for_one_shot() {
        // Arrange
        let prompt = "Generate title";

        // Act
        let rendered_prompt = protocol_prepend_instructions(
            prompt,
            ProtocolRequestProfile::UtilityPrompt,
            ProtocolSchemaInstructionMode::PromptSchema,
        );

        // Assert
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(rendered_prompt.contains("---"));
        assert!(rendered_prompt.contains("For this one-shot utility prompt"));
        assert!(rendered_prompt.contains(r#"{"answer":"...","questions":[],"summary":null}"#));
        assert!(rendered_prompt.contains("\"summary\""));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures shared prompt preparation applies replay wrapping before
    /// protocol instructions.
    fn test_prepare_prompt_text_applies_replay_and_protocol_instructions() {
        // Arrange
        let request = PromptPreparationRequest {
            instruction_delivery_mode: InstructionDeliveryMode::BootstrapWithReplay,
            prompt: "Continue edits",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_session_output: Some("previous output"),
            schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
        };

        // Act
        let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");

        // Assert
        assert!(prepared_prompt.contains("Structured response protocol:"));
        assert!(prepared_prompt.contains("previous output"));
        assert!(prepared_prompt.ends_with("Continue edits"));
    }

    #[test]
    /// Ensures compact refresh reminders omit the full schema while keeping
    /// the contract reminder and task body.
    fn test_prepend_protocol_refresh_reminder_adds_compact_contract_notice() {
        // Arrange
        let prompt = "Continue the implementation";

        // Act
        let rendered_prompt =
            protocol_prepend_refresh_reminder(prompt, ProtocolRequestProfile::SessionTurn);

        // Assert
        assert!(rendered_prompt.contains("Protocol refresh reminder:"));
        assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
        assert!(rendered_prompt.contains("read-only git commands"));
        assert!(!rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures prompt preparation can emit the compact app-server reminder
    /// instead of the full bootstrap wrapper.
    fn test_prepare_prompt_text_uses_delta_only_refresh_mode() {
        // Arrange
        let request = PromptPreparationRequest {
            instruction_delivery_mode: InstructionDeliveryMode::DeltaOnly,
            prompt: "Continue edits",
            protocol_profile: ProtocolRequestProfile::SessionTurn,
            replay_session_output: Some("previous output"),
            schema_instruction_mode: ProtocolSchemaInstructionMode::PromptSchema,
        };

        // Act
        let prepared_prompt = prepare_prompt_text(request).expect("prompt should render");

        // Assert
        assert!(prepared_prompt.contains("Protocol refresh reminder:"));
        assert!(!prepared_prompt.contains("Authoritative JSON Schema:"));
        assert!(!prepared_prompt.contains("previous output"));
        assert!(prepared_prompt.ends_with("Continue edits"));
    }

    #[test]
    /// Ensures CLI prompt rendering replaces image placeholders with local
    /// file paths in placeholder order.
    fn test_render_prompt_with_local_images_replaces_placeholders_in_order() {
        // Arrange
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/first-image.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/second-image.png"),
            },
        ];

        // Act
        let rendered_prompt = render_prompt_with_local_images(
            "Compare [Image #2] with [Image #1]",
            &attachments,
            "TestBackend",
        )
        .expect("prompt rendering should succeed");

        // Assert
        assert_eq!(
            rendered_prompt,
            "Compare /tmp/second-image.png with /tmp/first-image.png"
        );
    }

    #[test]
    /// Ensures CLI prompt rendering appends local image paths when attachment
    /// metadata survives without a placeholder match.
    fn test_render_prompt_with_local_images_appends_missing_paths() {
        // Arrange
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/first-image.png"),
        }];

        // Act
        let rendered_prompt =
            render_prompt_with_local_images("Review this change", &attachments, "TestBackend")
                .expect("prompt rendering should succeed");

        // Assert
        assert_eq!(
            rendered_prompt,
            "Review this change\n/tmp/first-image.png\n"
        );
    }

    #[cfg(unix)]
    #[test]
    /// Ensures CLI prompt rendering fails fast with the provider label when an
    /// attachment path is not valid UTF-8.
    fn test_render_prompt_with_local_images_rejects_non_utf8_paths() {
        // Arrange
        let attachments = vec![TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from(OsString::from_vec(vec![0x66, 0x80, 0x6f])),
        }];

        // Act
        let error = render_prompt_with_local_images("Review [Image #1]", &attachments, "Claude")
            .expect_err("prompt rendering should fail");

        // Assert
        assert_eq!(
            error,
            AgentBackendError::CommandBuild(
                "Claude prompt image path is not valid UTF-8".to_string()
            )
        );
    }

    #[test]
    /// Ensures CLI prompt access roots deduplicate sorted attachment
    /// directories when the provider only needs attachment parents.
    fn test_cli_prompt_access_directories_deduplicates_attachment_directories() {
        // Arrange
        let workspace_folder = PathBuf::from("/tmp/session");
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/images-b/two.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/images-a/one.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #3]".to_string(),
                local_image_path: PathBuf::from("/tmp/images-a/three.png"),
            },
        ];

        // Act
        let directories = cli_prompt_access_directories(
            &workspace_folder,
            &attachments,
            CliPromptAccessRootMode::AttachmentsOnly,
        );

        // Assert
        assert_eq!(
            directories,
            vec![
                PathBuf::from("/tmp/images-a"),
                PathBuf::from("/tmp/images-b")
            ]
        );
    }

    #[test]
    /// Ensures Antigravity-style access roots keep the workspace first and do
    /// not duplicate it when an attachment also lives under that directory.
    fn test_cli_prompt_access_directories_keeps_workspace_first() {
        // Arrange
        let workspace_folder = PathBuf::from("/tmp/z-session");
        let attachments = vec![
            TurnPromptAttachment {
                placeholder: "[Image #1]".to_string(),
                local_image_path: PathBuf::from("/tmp/z-session/one.png"),
            },
            TurnPromptAttachment {
                placeholder: "[Image #2]".to_string(),
                local_image_path: PathBuf::from("/tmp/a-images/two.png"),
            },
        ];

        // Act
        let directories = cli_prompt_access_directories(
            &workspace_folder,
            &attachments,
            CliPromptAccessRootMode::WorkspaceThenAttachments,
        );

        // Assert
        assert_eq!(
            directories,
            vec![workspace_folder, PathBuf::from("/tmp/a-images")]
        );
    }
}
