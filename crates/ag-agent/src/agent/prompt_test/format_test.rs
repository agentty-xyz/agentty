use super::*;

#[test]
fn response_style_prompt_wraps_session_turns_and_preserves_attachments() {
    // Arrange
    let attachment = TurnPromptAttachment {
        local_image_path: PathBuf::from("/tmp/example.png"),
        placeholder: "[Image #1]".to_string(),
    };

    // Act
    let prompts = ResponseStyle::ALL.map(|response_style| {
        apply_response_style_prompt(
            TurnPrompt {
                attachments: vec![attachment.clone()],
                text: "Explain [Image #1]".to_string(),
                text_source: ag_protocol::TurnPromptTextSource::UserPrompt,
            },
            ProtocolRequestProfile::SessionTurn,
            response_style,
        )
        .expect("response style prompt should render")
    });

    // Assert
    for (prompt, response_style) in prompts.iter().zip(ResponseStyle::ALL) {
        assert!(prompt.text.starts_with("# Response Style\n\n"));
        assert!(prompt.text.contains(response_style.prompt_instruction()));
        assert!(prompt.text.ends_with("Explain [Image #1]"));
        assert_eq!(prompt.attachments, vec![attachment.clone()]);
        assert_eq!(
            prompt.text_source,
            ag_protocol::TurnPromptTextSource::UserPrompt
        );
    }
}

#[test]
fn response_style_prompt_leaves_utility_prompts_unchanged() {
    // Arrange
    let prompt = TurnPrompt::from_agent_data("Generate a title".to_string());

    // Act
    let styled_prompt = apply_response_style_prompt(
        prompt.clone(),
        ProtocolRequestProfile::UtilityPrompt,
        ResponseStyle::Detailed,
    )
    .expect("utility prompt should remain valid");

    // Assert
    assert_eq!(styled_prompt, prompt);
}

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
/// Ensures resume prompt rendering includes trimmed transcript text and
/// the new user prompt.
fn test_build_resume_prompt_includes_replay_transcript_and_prompt() {
    // Arrange
    let prompt = "Continue tests; keep {{ transcript }} literal";
    let replay_transcript = Some("  previous {{ prompt }} line  \n");

    // Act
    let resume_prompt =
        build_resume_prompt(prompt, replay_transcript).expect("resume prompt should render");

    let normalized_resume_prompt = normalize_prompt(&resume_prompt);
    let transcript_position = resume_prompt
        .find(r"\<session_transcript> previous {{ prompt }} line")
        .expect("transcript boundary should be present");
    let prompt_position = resume_prompt
        .find(r"\<user_prompt> Continue tests; keep {{ transcript }} literal")
        .expect("user prompt boundary should be present");

    // Assert
    assert!(transcript_position < prompt_position);
    assert!(normalized_resume_prompt.contains("new user prompt as a follow-up"));
    assert!(normalized_resume_prompt.contains("changes made during this session"));
    assert!(normalized_resume_prompt.contains("preserve unrelated pre-existing work"));
    assert!(normalized_resume_prompt.contains("resume unfinished work"));
    assert!(resume_prompt.ends_with(r"\</user_prompt>"));
}

#[test]
/// Ensures whitespace-only transcript text does not trigger transcript
/// wrapping and returns the original prompt.
fn test_build_resume_prompt_returns_original_prompt_when_output_is_blank() {
    // Arrange
    let prompt = "Follow-up request";
    let replay_transcript = Some("   ");

    // Act
    let resume_prompt =
        build_resume_prompt(prompt, replay_transcript).expect("resume prompt should render");

    // Assert
    assert_eq!(resume_prompt, prompt);
}

#[test]
/// Ensures absent transcript text keeps resume prompt formatting unchanged.
fn test_build_resume_prompt_returns_original_prompt_without_output() {
    // Arrange
    let prompt = "Retry merge";

    // Act
    let resume_prompt = build_resume_prompt(prompt, None).expect("resume prompt should render");

    // Assert
    assert_eq!(resume_prompt, prompt);
}
