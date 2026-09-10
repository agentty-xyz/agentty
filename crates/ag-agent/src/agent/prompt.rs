//! Shared prompt-shaping helpers for agent-facing markdown prompts.

use std::path::{Path, PathBuf};
use std::process::Command;

use ag_protocol::{
    ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt, TurnPromptAttachment,
    TurnPromptContentPart, prepend_protocol_instructions as protocol_prepend_instructions,
    prepend_protocol_refresh_reminder as protocol_prepend_refresh_reminder,
    split_turn_prompt_content,
};
use askama::Template;

use super::backend::{AgentBackendError, BuildCommandRequest};
use super::instruction::InstructionDeliveryMode;
use crate::channel::PersonalityPromptUpdate;
use crate::model::session::ResponseStyle;

/// Askama view model for rendering resume prompts with prior transcript text.
#[derive(Template)]
#[template(path = "resume_with_transcript_prompt.md", escape = "none")]
struct ResumeWithTranscriptPromptTemplate<'a> {
    /// New prompt content appended after the replayed transcript.
    prompt: &'a str,
    /// Prior transcript text replayed into the follow-up prompt.
    transcript: &'a str,
}

/// Askama view model for placing personality instructions before a turn.
#[derive(Template)]
#[template(path = "personality_prompt.md", escape = "none")]
struct PersonalityPromptTemplate<'a> {
    /// Markdown heading describing a bootstrap or delta update.
    heading: &'a str,
    /// Personality instructions or clearing guidance.
    personality: &'a str,
    /// Remaining turn prompt content.
    prompt: &'a str,
}

/// Askama view model for placing response-style guidance before a turn.
#[derive(Template)]
#[template(path = "response_style_prompt.md", escape = "none")]
struct ResponseStylePromptTemplate<'a> {
    /// Guidance corresponding to the selected response style.
    instruction: &'a str,
    /// Remaining turn prompt content.
    prompt: &'a str,
}

/// Shared prompt preparation input for one transport turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptPreparationRequest<'a> {
    /// Delivery mode selected for the current provider attempt.
    pub instruction_delivery_mode: InstructionDeliveryMode,
    /// Current personality body used for full instruction bootstraps.
    pub personality_prompt: Option<&'a str>,
    /// Personality change used only for delta delivery.
    pub personality_update: &'a PersonalityPromptUpdate,
    /// Base user prompt before replay wrapping and protocol instructions.
    pub prompt: &'a str,
    /// Protocol family that determines the rendered instruction envelope.
    pub protocol_profile: ProtocolRequestProfile,
    /// Prior transcript text available for replay.
    pub replay_transcript: Option<&'a str>,
    /// Schema guidance mode selected from the provider's structured-output
    /// capability.
    pub schema_instruction_mode: ProtocolSchemaInstructionMode,
    /// Workspace folder rendered into the isolation contract as the only
    /// writable root for the turn.
    pub workspace_root: &'a Path,
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
        InstructionDeliveryMode::BootstrapFull => {
            let prompt = prepend_personality_prompt(request.prompt, request.personality_prompt)?;

            Ok(protocol_prepend_instructions(
                &prompt,
                request.protocol_profile,
                request.schema_instruction_mode,
                request.workspace_root,
            ))
        }
        InstructionDeliveryMode::DeltaOnly => {
            let prompt = prepend_personality_update(request.prompt, request.personality_update)?;

            Ok(protocol_prepend_refresh_reminder(
                &prompt,
                request.protocol_profile,
                request.workspace_root,
            ))
        }
        InstructionDeliveryMode::BootstrapWithReplay => {
            let prompt = build_resume_prompt(request.prompt, request.replay_transcript)?;
            let prompt = prepend_personality_prompt(&prompt, request.personality_prompt)?;

            Ok(protocol_prepend_instructions(
                &prompt,
                request.protocol_profile,
                request.schema_instruction_mode,
                request.workspace_root,
            ))
        }
    }
}

/// Builds a resume prompt that optionally prepends previous transcript text.
///
/// # Errors
/// Returns an error if Askama template rendering fails.
pub(crate) fn build_resume_prompt(
    prompt: &str,
    replay_transcript: Option<&str>,
) -> Result<String, AgentBackendError> {
    let Some(transcript) = replay_transcript
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(prompt.to_string());
    };

    let template = ResumeWithTranscriptPromptTemplate { prompt, transcript };

    render_template("resume_with_transcript_prompt.md", &template)
}

/// Builds the full prompt text for a CLI provider.
///
/// This shared helper keeps attachment placeholder rendering and provider
/// protocol preparation in one place for both argv and stdin transports while
/// preserving backend-specific error labels.
///
/// # Errors
/// Returns an error when attachment path rendering, resume wrapping, or
/// protocol prompt rendering fails.
pub(crate) fn build_cli_prompt_text(
    request: BuildCommandRequest<'_>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    backend_display_name: &str,
) -> Result<String, AgentBackendError> {
    let prompt =
        render_prompt_with_local_images(request.prompt, request.attachments, backend_display_name)?;

    prepare_prompt_text(PromptPreparationRequest {
        instruction_delivery_mode: if request.request_kind.is_resume() {
            InstructionDeliveryMode::BootstrapWithReplay
        } else {
            InstructionDeliveryMode::BootstrapFull
        },
        personality_prompt: request.personality_prompt,
        personality_update: &PersonalityPromptUpdate::Unchanged,
        prompt: &prompt,
        protocol_profile: request.request_kind.protocol_profile(),
        replay_transcript: request.replay_transcript,
        schema_instruction_mode,
        workspace_root: request.folder,
    })
}

/// Prepends response-style guidance to interactive session turns.
pub(crate) fn apply_response_style_prompt(
    mut prompt: TurnPrompt,
    protocol_profile: ProtocolRequestProfile,
    response_style: ResponseStyle,
) -> Result<TurnPrompt, AgentBackendError> {
    if protocol_profile == ProtocolRequestProfile::SessionTurn {
        let prompt_text = prompt.agent_text();
        let template = ResponseStylePromptTemplate {
            instruction: response_style.prompt_instruction(),
            prompt: &prompt_text,
        };
        prompt.text = render_template("response_style_prompt.md", &template)?;
    }

    Ok(prompt)
}

/// Builds a full prompt payload to stream over stdin for CLI providers.
///
/// # Errors
/// Returns an error when the shared CLI prompt text cannot be rendered.
pub(crate) fn build_prompt_stdin_payload(
    request: BuildCommandRequest<'_>,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    backend_display_name: &str,
) -> Result<Vec<u8>, AgentBackendError> {
    build_cli_prompt_text(request, schema_instruction_mode, backend_display_name)
        .map(String::into_bytes)
}

/// Prepends current personality instructions to one full bootstrap prompt.
fn prepend_personality_prompt(
    prompt: &str,
    personality_prompt: Option<&str>,
) -> Result<String, AgentBackendError> {
    let Some(personality) = personality_prompt
        .map(str::trim)
        .filter(|personality| !personality.is_empty())
    else {
        return Ok(prompt.to_string());
    };
    let template = PersonalityPromptTemplate {
        heading: "# Personality",
        personality,
        prompt,
    };

    render_template("personality_prompt.md", &template)
}

/// Prepends one changed or cleared personality instruction for delta mode.
fn prepend_personality_update(
    prompt: &str,
    personality_update: &PersonalityPromptUpdate,
) -> Result<String, AgentBackendError> {
    let personality = match personality_update {
        PersonalityPromptUpdate::Clear => {
            "The session personality has been cleared. Continue without the previous personality \
             instructions."
        }
        PersonalityPromptUpdate::Set(personality) => personality.trim(),
        PersonalityPromptUpdate::Unchanged => return Ok(prompt.to_string()),
    };
    let template = PersonalityPromptTemplate {
        heading: "# Personality Update",
        personality,
        prompt,
    };

    render_template("personality_prompt.md", &template)
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
/// arbitrary prompt payload.
///
/// Returns a string of backticks whose length exceeds the longest run of
/// consecutive backticks found anywhere in `content`, with a minimum length
/// of three. This prevents a triple-backtick fence from being terminated
/// prematurely when the payload itself contains Markdown fences (for example,
/// when reviewing changes to Markdown or prompt-template files).
pub fn diff_fence(content: &str) -> String {
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
#[path = "prompt_test.rs"]
mod tests;
