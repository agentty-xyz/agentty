//! Protocol-owned prompt envelopes for agent-facing instruction text.

use std::path::Path;

use askama::Template;

use crate::{ProtocolRequestProfile, agent_response_json_schema_json};

const PROTOCOL_INSTRUCTIONS_MARKER: &str = "Structured response protocol:";
const PROTOCOL_REFRESH_REMINDER_MARKER: &str = "Protocol refresh reminder:";
const REPAIR_RESPONSE_PREVIEW_MAX_CHARS: usize = 500;

/// Askama view model for full protocol instructions with prompt-side schema.
#[derive(Template)]
#[template(path = "protocol_instruction_prompt.md", escape = "none")]
struct ProtocolInstructionPromptTemplate<'a> {
    prompt: &'a str,
    protocol_usage_instructions: &'a str,
    response_json_schema: &'a str,
    workspace_root: &'a str,
}

/// Askama view model for protocol instructions when the transport enforces
/// the response schema.
#[derive(Template)]
#[template(path = "protocol_instruction_policy_prompt.md", escape = "none")]
struct ProtocolInstructionPolicyPromptTemplate<'a> {
    prompt: &'a str,
    protocol_usage_instructions: &'a str,
    workspace_root: &'a str,
}

/// Askama view model for session-turn protocol usage instructions.
#[derive(Template)]
#[template(path = "protocol_instruction_session_turn_usage.md", escape = "none")]
struct ProtocolInstructionSessionTurnUsageTemplate;

/// Askama view model for one-shot protocol usage instructions.
#[derive(Template)]
#[template(path = "protocol_instruction_utility_prompt_usage.md", escape = "none")]
struct ProtocolInstructionUtilityPromptUsageTemplate;

/// Askama view model for compact refresh reminders.
#[derive(Template)]
#[template(path = "protocol_refresh_prompt.md", escape = "none")]
struct ProtocolRefreshPromptTemplate<'a> {
    prompt: &'a str,
    protocol_refresh_instructions: &'a str,
    workspace_root: &'a str,
}

/// Askama view model for session-turn refresh instructions.
#[derive(Template)]
#[template(path = "protocol_refresh_session_turn_instruction.md", escape = "none")]
struct ProtocolRefreshSessionTurnInstructionTemplate;

/// Askama view model for one-shot refresh instructions.
#[derive(Template)]
#[template(
    path = "protocol_refresh_utility_prompt_instruction.md",
    escape = "none"
)]
struct ProtocolRefreshUtilityPromptInstructionTemplate;

/// Askama view model for repair prompts after protocol parse failures.
#[derive(Template)]
#[template(path = "protocol_repair_prompt.md", escape = "none")]
struct ProtocolRepairPromptTemplate<'a> {
    parse_error: &'a str,
    response_json_schema: &'a str,
    response_preview: &'a str,
}

/// Controls whether bootstrap prompt instructions include the full protocol
/// JSON Schema text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolSchemaInstructionMode {
    /// Include the full self-descriptive JSON Schema in the prompt because
    /// the provider does not enforce Agentty's response schema natively.
    PromptSchema,
    /// Omit the full schema text because the provider enforces the same
    /// response schema through its transport-level structured output API.
    TransportSchema,
}

impl ProtocolSchemaInstructionMode {
    /// Returns whether bootstrap instructions should embed the full JSON
    /// Schema text in the prompt body.
    fn includes_response_json_schema(self) -> bool {
        matches!(self, Self::PromptSchema)
    }
}

/// Prepends structured response protocol instructions to a prompt.
///
/// Tells agents to emit one top-level JSON object that matches Agentty's
/// structured protocol while selecting the cheapest safe schema guidance for
/// the current provider. Providers without native structured output receive
/// the full JSON Schema in the prompt; providers with native enforcement get
/// policy and field-routing instructions only. `workspace_root` names the
/// only writable directory for the turn. If the prompt already contains the
/// protocol marker, this function returns the prompt unchanged to avoid
/// duplicated guidance.
#[must_use]
pub fn prepend_protocol_instructions(
    prompt: &str,
    profile: ProtocolRequestProfile,
    schema_instruction_mode: ProtocolSchemaInstructionMode,
    workspace_root: &Path,
) -> String {
    if prompt.contains(PROTOCOL_INSTRUCTIONS_MARKER) {
        return prompt.to_string();
    }

    let protocol_usage_instructions = render_protocol_usage_instructions(profile);
    let workspace_root = workspace_root.display().to_string();
    if !schema_instruction_mode.includes_response_json_schema() {
        let template = ProtocolInstructionPolicyPromptTemplate {
            prompt,
            protocol_usage_instructions: &protocol_usage_instructions,
            workspace_root: &workspace_root,
        };

        return render_template("protocol_instruction_policy_prompt.md", &template);
    }

    let response_json_schema = agent_response_json_schema_json();
    let template = ProtocolInstructionPromptTemplate {
        prompt,
        protocol_usage_instructions: &protocol_usage_instructions,
        response_json_schema: &response_json_schema,
        workspace_root: &workspace_root,
    };

    render_template("protocol_instruction_prompt.md", &template)
}

/// Prepends a compact refresh reminder for providers that already received
/// the full instruction contract in the active context.
///
/// The reminder repeats the workspace-isolation boundary for
/// `workspace_root` so long-lived provider contexts keep the rule even after
/// provider-side context compaction.
#[must_use]
pub fn prepend_protocol_refresh_reminder(
    prompt: &str,
    profile: ProtocolRequestProfile,
    workspace_root: &Path,
) -> String {
    if prompt.contains(PROTOCOL_INSTRUCTIONS_MARKER)
        || prompt.contains(PROTOCOL_REFRESH_REMINDER_MARKER)
    {
        return prompt.to_string();
    }

    let protocol_refresh_instructions = render_protocol_refresh_instructions(profile);
    let workspace_root = workspace_root.display().to_string();
    let template = ProtocolRefreshPromptTemplate {
        prompt,
        protocol_refresh_instructions: &protocol_refresh_instructions,
        workspace_root: &workspace_root,
    };

    render_template("protocol_refresh_prompt.md", &template)
}

/// Builds the protocol repair prompt text for one failed parse attempt.
///
/// The returned prompt is self-contained: it includes the full JSON schema
/// and the `Structured response protocol:` marker so it can be submitted
/// through the standard prompt pipeline without being double-wrapped.
#[must_use]
pub fn build_protocol_repair_prompt(parse_error: &str, malformed_response: &str) -> String {
    let response_json_schema = agent_response_json_schema_json();
    let response_preview = truncate_preview(malformed_response, REPAIR_RESPONSE_PREVIEW_MAX_CHARS);
    let template = ProtocolRepairPromptTemplate {
        parse_error,
        response_json_schema: &response_json_schema,
        response_preview: &response_preview,
    };

    render_template("protocol_repair_prompt.md", &template)
}

fn render_protocol_usage_instructions(profile: ProtocolRequestProfile) -> String {
    if matches!(profile, ProtocolRequestProfile::SessionTurn) {
        return render_template(
            "protocol_instruction_session_turn_usage.md",
            &ProtocolInstructionSessionTurnUsageTemplate,
        );
    }

    render_template(
        "protocol_instruction_utility_prompt_usage.md",
        &ProtocolInstructionUtilityPromptUsageTemplate,
    )
}

fn render_protocol_refresh_instructions(profile: ProtocolRequestProfile) -> String {
    if matches!(profile, ProtocolRequestProfile::SessionTurn) {
        return render_template(
            "protocol_refresh_session_turn_instruction.md",
            &ProtocolRefreshSessionTurnInstructionTemplate,
        );
    }

    render_template(
        "protocol_refresh_utility_prompt_instruction.md",
        &ProtocolRefreshUtilityPromptInstructionTemplate,
    )
}

fn render_template(template_name: &str, template: &impl Template) -> String {
    let rendered = match template.render() {
        Ok(rendered) => rendered,
        Err(error) => format!("Failed to render `{template_name}`: {error}"),
    };

    trim_template(&rendered)
}

fn trim_template(template: &str) -> String {
    template.trim_end().to_string()
}

fn truncate_preview(raw: &str, max_chars: usize) -> String {
    let preview: String = raw.chars().take(max_chars).collect();
    let total_chars = raw.chars().count();

    if total_chars <= max_chars {
        return preview;
    }

    format!("{preview}\n... [{} more chars]", total_chars - max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the workspace root used by envelope rendering tests.
    fn test_workspace_root() -> &'static Path {
        Path::new("/tmp/agentty-wt/session-1")
    }

    #[test]
    /// Ensures session prompts include the critical protocol contract markers.
    fn test_prepend_protocol_instructions_adds_session_protocol_instructions() {
        // Arrange
        let prompt = "Implement feature";

        // Act
        let rendered_prompt = prepend_protocol_instructions(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::PromptSchema,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.contains("File path output requirements:"));
        assert!(rendered_prompt.contains("Workspace isolation requirements:"));
        assert!(rendered_prompt.contains("Your workspace root is `/tmp/agentty-wt/session-1`."));
        assert!(rendered_prompt.contains("Anything outside that"));
        assert!(rendered_prompt.contains("root is read-only."));
        assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
        assert!(
            rendered_prompt.contains("Allowed forms: `path`, `path:line`, `path:line:column`.")
        );
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
        assert!(
            rendered_prompt
                .contains("______________________________________________________________________")
        );
        assert!(!rendered_prompt.contains("{# task separator #}"));
        assert!(rendered_prompt.contains("For this session turn"));
        assert!(rendered_prompt.contains("```mermaid"));
        assert!(rendered_prompt.contains("put the diagram only in `answer`"));
        assert!(normalized_rendered_prompt.contains("start the opening fence at column 1"));
        assert!(rendered_prompt.contains("Do not emit Mermaid as plain text"));
        assert!(rendered_prompt.contains("Supported mermaid syntax"));
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
        let rendered_prompt = prepend_protocol_instructions(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::TransportSchema,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(rendered_prompt.contains("Workspace isolation requirements:"));
        assert!(rendered_prompt.contains("Your workspace root is `/tmp/agentty-wt/session-1`."));
        assert!(rendered_prompt.contains("Anything outside that"));
        assert!(rendered_prompt.contains("root is read-only."));
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
        let prompt = prepend_protocol_instructions(
            "Implement feature",
            ProtocolRequestProfile::SessionTurn,
            ProtocolSchemaInstructionMode::PromptSchema,
            test_workspace_root(),
        );

        // Act
        let rendered_prompt = prepend_protocol_instructions(
            &prompt,
            ProtocolRequestProfile::UtilityPrompt,
            ProtocolSchemaInstructionMode::TransportSchema,
            test_workspace_root(),
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
        let rendered_prompt = prepend_protocol_instructions(
            prompt,
            ProtocolRequestProfile::UtilityPrompt,
            ProtocolSchemaInstructionMode::PromptSchema,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.contains("Structured response protocol:"));
        assert!(
            rendered_prompt
                .contains("______________________________________________________________________")
        );
        assert!(rendered_prompt.contains("For this one-shot utility prompt"));
        assert!(!rendered_prompt.contains("mermaid"));
        assert!(rendered_prompt.contains(r#"{"answer":"...","questions":[],"summary":null}"#));
        assert!(rendered_prompt.contains("\"summary\""));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures user prompt text is inserted after generated protocol
    /// placeholders so prompt content cannot trigger recursive expansion.
    fn test_prepend_protocol_instructions_preserves_prompt_placeholders() {
        // Arrange
        let prompt = "Keep these literal: {{ response_json_schema }} {{ \
                      protocol_usage_instructions }} {{ workspace_root }}";

        // Act
        let rendered_prompt = prepend_protocol_instructions(
            prompt,
            ProtocolRequestProfile::UtilityPrompt,
            ProtocolSchemaInstructionMode::PromptSchema,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures compact refresh reminders omit the full schema while keeping
    /// the contract reminder and task body.
    fn test_prepend_protocol_refresh_reminder_adds_compact_contract_notice() {
        // Arrange
        let prompt = "Continue the implementation";

        // Act
        let rendered_prompt = prepend_protocol_refresh_reminder(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.contains("Protocol refresh reminder:"));
        assert!(rendered_prompt.contains("repository-root-relative POSIX paths"));
        assert!(rendered_prompt.contains("If you run git commands, use read-only commands only."));
        assert!(rendered_prompt.contains("Do not run mutating git commands."));
        assert!(rendered_prompt.contains("inside the workspace root `/tmp/agentty-wt/session-1`"));
        assert!(rendered_prompt.contains("anything outside that root is read-only"));
        assert!(rendered_prompt.contains("Mermaid diagrams must remain in `answer`"));
        assert!(rendered_prompt.contains("fences without the `mermaid` info string"));
        assert!(
            rendered_prompt
                .contains("______________________________________________________________________")
        );
        assert!(!rendered_prompt.contains("Authoritative JSON Schema:"));
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Ensures refresh prompt text is inserted after generated reminder
    /// placeholders so prompt content cannot trigger recursive expansion.
    fn test_prepend_protocol_refresh_reminder_preserves_prompt_placeholders() {
        // Arrange
        let prompt = "Keep this literal: {{ protocol_refresh_instructions }} {{ workspace_root }}";

        // Act
        let rendered_prompt = prepend_protocol_refresh_reminder(
            prompt,
            ProtocolRequestProfile::SessionTurn,
            test_workspace_root(),
        );

        // Assert
        assert!(rendered_prompt.ends_with(prompt));
    }

    #[test]
    /// Repair prompt renders with the parse error and a response preview.
    fn test_build_protocol_repair_prompt_includes_error_and_preview() {
        // Arrange
        let parse_error = "response is not valid protocol JSON: invalid JSON";
        let malformed_response = "plain text response";

        // Act
        let repair_prompt = build_protocol_repair_prompt(parse_error, malformed_response);

        // Assert
        assert!(repair_prompt.contains(parse_error));
        assert!(repair_prompt.contains("plain text response"));
        assert!(repair_prompt.contains("Structured response protocol:"));
        assert!(repair_prompt.contains("Authoritative JSON Schema:"));
        assert!(repair_prompt.contains("\"answer\""));
    }

    #[test]
    /// Ensures malformed response previews are inserted after generated
    /// schema placeholders so agent output cannot trigger recursive expansion.
    fn test_build_protocol_repair_prompt_preserves_response_preview_placeholders() {
        // Arrange
        let malformed_response = "Keep this literal: {{ response_json_schema }}";

        // Act
        let repair_prompt =
            build_protocol_repair_prompt("schema validation failed", malformed_response);

        // Assert
        assert!(repair_prompt.contains(malformed_response));
    }

    #[test]
    /// Repair prompt truncates long malformed responses to the preview limit.
    fn test_build_protocol_repair_prompt_truncates_long_response() {
        // Arrange
        let parse_error = "schema validation failed";
        let malformed_response = "x".repeat(1000);

        // Act
        let repair_prompt = build_protocol_repair_prompt(parse_error, &malformed_response);

        // Assert
        assert!(repair_prompt.contains("500 more chars"));
        assert!(!repair_prompt.contains(&malformed_response));
    }

    #[test]
    /// Repair prompt includes the protocol marker to prevent double-wrapping.
    fn test_build_protocol_repair_prompt_contains_protocol_marker() {
        // Arrange / Act
        let repair_prompt = build_protocol_repair_prompt("error", "response");

        // Assert
        assert!(repair_prompt.contains("Structured response protocol:"));
    }

    #[test]
    /// Short responses are not truncated.
    fn test_truncate_preview_keeps_short_responses_intact() {
        // Arrange / Act
        let preview = truncate_preview("short", 500);

        // Assert
        assert_eq!(preview, "short");
    }

    #[test]
    /// Long responses are truncated with a character count suffix.
    fn test_truncate_preview_truncates_long_responses() {
        // Arrange
        let long_response = "a".repeat(600);

        // Act
        let preview = truncate_preview(&long_response, 500);

        // Assert
        assert!(preview.starts_with(&"a".repeat(500)));
        assert!(preview.contains("100 more chars"));
    }
}
