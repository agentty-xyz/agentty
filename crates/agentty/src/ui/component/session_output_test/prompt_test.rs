use ratatui::layout::Rect;

use super::super::SessionOutputLineContext;
use super::support::{line_context, output_lines, session_fixture, set_conversation_transcript};
use crate::domain::session::Status;
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::ui::prompt_block::USER_PROMPT_PREFIX;
use crate::ui::{prompt_block, session_output_assembly, style};

#[test]
fn test_append_user_prompt_markdown_lines_keeps_right_gutter() {
    // Arrange
    let mut lines = Vec::new();

    // Act
    session_output_assembly::tests::append_user_prompt_markdown_lines(
        &mut lines, "one two", 10, None,
    );
    let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    let continuation_prefix = prompt_block::user_prompt_continuation_prefix();
    let prompt_lines = lines
        .iter()
        .filter(|line| {
            let text = line.to_string();

            !text.trim().is_empty()
                && (text.starts_with(USER_PROMPT_PREFIX)
                    || text.starts_with(continuation_prefix.as_str()))
        })
        .collect::<Vec<_>>();

    // Assert
    assert!(
        !rendered_lines
            .iter()
            .any(|line| line.trim_end() == " › one two")
    );
    assert_eq!(prompt_lines.len(), 2);
    for line in prompt_lines {
        let rendered_text = line.to_string();
        let trimmed_width = rendered_text.trim_end().chars().count();

        assert_eq!(line.width(), 10);
        assert!(trimmed_width < line.width());
        assert!(
            line.spans
                .last()
                .is_some_and(|span| span.content.chars().all(char::is_whitespace)
                    && span.style.bg == Some(style::palette::surface_prompt()))
        );
    }
}

#[test]
fn test_append_user_prompt_markdown_lines_wraps_code_blocks_on_word_boundaries() {
    // Arrange
    let mut lines = Vec::new();
    let prompt = "```text\nformatted blocks in user messages without words breaking\n```";

    // Act
    session_output_assembly::tests::append_user_prompt_markdown_lines(&mut lines, prompt, 36, None);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(
        rendered_lines
            .iter()
            .any(|line| line == " › formatted blocks in user")
    );
    assert!(
        rendered_lines
            .iter()
            .any(|line| line == "   messages without words breaking")
    );
    assert!(!rendered_lines.iter().any(|line| line.ends_with("message")));
    assert!(!rendered_lines.iter().any(|line| line.starts_with("   s ")));
}

/// Verifies active-turn splitting uses typed user-prompt rows instead of
/// assistant text that happens to contain the prompt marker.
#[test]
fn test_typed_transcript_sections_ignore_assistant_prompt_markers() {
    // Arrange
    let transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "previous prompt"),
        SessionMessage::conversation(
            1,
            SessionMessageKind::AssistantAnswer,
            "previous answer\n › quoted assistant marker",
        ),
        SessionMessage::new(
            2,
            SessionMessageKind::WorkflowNotice,
            "\n[Commit] No changes to commit.\n",
        ),
        SessionMessage::conversation(3, SessionMessageKind::UserPrompt, "actual prompt"),
        SessionMessage::conversation(
            4,
            SessionMessageKind::AssistantAnswer,
            "streaming answer\n › quoted active output",
        ),
    ]);

    // Act
    let (completed_turn, active_turn, trailing_notice) =
        session_output_assembly::tests::transcript_section_texts(Status::InProgress, &transcript);

    // Assert
    assert!(completed_turn.contains(" › quoted assistant marker"));
    assert!(trailing_notice.contains("[Commit] No changes to commit."));
    assert!(active_turn.starts_with(" › actual prompt"));
}

/// Verifies prompt edge trimming keeps internal blank rows while
/// retaining one separator before the following transcript message.
#[test]
fn test_output_lines_trims_only_outer_user_prompt_empty_lines() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    set_conversation_transcript(
        &mut session,
        vec![
            (
                SessionMessageKind::UserPrompt,
                "\nfollow up\n\nwith context\n",
            ),
            (SessionMessageKind::AssistantAnswer, "Completed response."),
        ],
    );

    // Act
    let rendered_lines = output_lines(&session, Rect::new(0, 0, 120, 16), line_context(), None);
    let prompt_line_index = rendered_lines
        .iter()
        .position(|line| line.to_string().contains("follow up"))
        .expect("prompt should be rendered");
    let context_line_index = rendered_lines
        .iter()
        .position(|line| line.to_string().contains("with context"))
        .expect("prompt context should be rendered");
    let response_line_index = rendered_lines
        .iter()
        .position(|line| line.to_string() == "Completed response.")
        .expect("response should be rendered");

    // Assert
    assert_eq!(prompt_line_index, 1);
    assert_eq!(context_line_index, prompt_line_index + 2);
    assert!(rendered_lines[prompt_line_index + 1].width() > 0);
    assert_eq!(rendered_lines[prompt_line_index + 1].to_string().trim(), "");
    assert_eq!(response_line_index, context_line_index + 3);
    assert!(rendered_lines[context_line_index + 1].width() > 0);
    assert_eq!(rendered_lines[context_line_index + 2].width(), 0);
}

/// Verifies a reply prompt follows earlier workflow notices.
#[test]
fn test_output_lines_in_progress_session_places_active_prompt_last() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::UserPrompt, "hi"),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            ),
            (SessionMessageKind::UserPrompt, "add hello world"),
        ],
    );
    session.status = Status::InProgress;

    // Act
    let lines = output_lines(
        &session,
        Rect::new(0, 0, 80, 8),
        SessionOutputLineContext {
            active_prompt_output: Some("\n › add hello world\n\n"),
            ..line_context()
        },
        None,
    );
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let commit_index = text
        .find("[Commit] No changes to commit.")
        .expect("commit footer should be rendered");
    let prompt_index = text
        .find(" › add hello world")
        .expect("active prompt should be rendered");

    // Assert
    assert!(!text.contains("Change Summary"));
    assert!(commit_index < prompt_index);
}

/// Verifies an active prompt at the start of the transcript retains its
/// assistant answer.
#[test]
fn test_output_lines_in_progress_single_prompt_keeps_answer() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::UserPrompt, "add hello world"),
            (
                SessionMessageKind::AssistantAnswer,
                "I added the README change.",
            ),
        ],
    );
    session.status = Status::InProgress;

    // Act
    let lines = output_lines(
        &session,
        Rect::new(0, 0, 80, 8),
        SessionOutputLineContext {
            active_prompt_output: Some(" › add hello world\n\n"),
            ..line_context()
        },
        None,
    );
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let prompt_index = text
        .find(" › add hello world")
        .expect("prompt should be rendered");
    let answer_index = text
        .find("I added the README change.")
        .expect("answer should be rendered");

    // Assert
    assert!(!text.contains("Change Summary"));
    assert!(prompt_index < answer_index);
}

/// Verifies the latest user prompt is detected when the exact active
/// prompt capture is unavailable.
#[test]
fn test_output_lines_in_progress_without_active_capture_finds_last_prompt() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::UserPrompt, "hi"),
            (SessionMessageKind::AssistantAnswer, "Hello!"),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            ),
            (SessionMessageKind::UserPrompt, "review project"),
        ],
    );
    session.status = Status::InProgress;

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let commit_index = text
        .find("[Commit] No changes to commit.")
        .expect("commit footer should be rendered");
    let prompt_index = text
        .find(" › review project")
        .expect("latest prompt should be rendered");

    // Assert
    assert!(!text.contains("Change Summary"));
    assert!(commit_index < prompt_index);
}

/// Verifies active-turn splitting uses the captured prompt block so
/// assistant output that resembles a prompt remains in the active block.
#[test]
fn test_output_lines_in_progress_ignores_assistant_lines_that_look_like_prompts() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (SessionMessageKind::UserPrompt, "hi"),
            (SessionMessageKind::AssistantAnswer, "previous answer"),
            (SessionMessageKind::UserPrompt, "actual prompt"),
            (
                SessionMessageKind::AssistantAnswer,
                "streaming answer\n › quoted output",
            ),
        ],
    );
    session.status = Status::InProgress;

    // Act
    let lines = output_lines(
        &session,
        Rect::new(0, 0, 80, 8),
        SessionOutputLineContext {
            active_prompt_output: Some("\n › actual prompt\n\n"),
            ..line_context()
        },
        None,
    );
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let prompt_index = text
        .find(" › actual prompt")
        .expect("active prompt should be rendered");
    let quoted_output_index = text
        .find(" › quoted output")
        .expect("assistant output that looks like a prompt should be rendered");

    // Assert
    assert!(!text.contains("Change Summary"));
    assert!(prompt_index < quoted_output_index);
}

#[test]
fn test_output_lines_render_user_prompt_markdown() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (
                SessionMessageKind::UserPrompt,
                concat!(
                    "Use **bold** and `code`.\n\n",
                    "| Input | Meaning |\n",
                    "| --- | --- |\n",
                    "| User prompt | Markdown |",
                ),
            ),
            (SessionMessageKind::AssistantAnswer, "assistant response"),
        ],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let inline_line = lines
        .iter()
        .find(|line| line.to_string().contains("Use bold and code."))
        .expect("inline markdown line should render");
    let table_header_line = lines
        .iter()
        .find(|line| line.to_string().contains("Input"))
        .expect("table header line should render");

    // Assert
    assert!(text.contains(" › Use bold and code."));
    assert!(text.contains("┌"));
    assert!(text.contains("User prompt"));
    assert!(!text.contains("**bold**"));
    assert!(!text.contains("`code`"));
    assert!(!text.contains("| --- | --- |"));
    assert!(inline_line.spans.iter().any(|span| {
        span.content.as_ref() == "bold"
            && span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
    }));
    assert!(table_header_line.spans.iter().any(|span| {
        span.content.as_ref().contains("Input")
            && span.style.bg == Some(style::palette::surface_elevated())
    }));
}

#[test]
fn test_output_lines_preserve_user_prompt_indentation() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![(
            SessionMessageKind::UserPrompt,
            "    if ready {\n        run();\n    }",
        )],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(
        rendered_lines
            .iter()
            .any(|line| line == " ›     if ready {"),
        "rendered lines: {rendered_lines:#?}"
    );
    assert!(
        rendered_lines
            .iter()
            .any(|line| line == "           run();")
    );
    assert!(rendered_lines.iter().any(|line| line == "       }"));
}

#[test]
fn test_output_lines_expand_user_prompt_tab_indentation() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![(
            SessionMessageKind::UserPrompt,
            "\tif ready {\n\t\trun();\n\t}",
        )],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();

    // Assert
    assert!(
        rendered_lines
            .iter()
            .any(|line| line == " ›     if ready {")
    );
    assert!(
        rendered_lines
            .iter()
            .any(|line| line == "           run();")
    );
    assert!(rendered_lines.iter().any(|line| line == "       }"));
}

#[test]
fn test_output_lines_render_indented_user_prompt_table_and_horizontal_rule() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![(
            SessionMessageKind::UserPrompt,
            concat!(
                "  | Input | Meaning |\n",
                "  | --- | --- |\n",
                "  | Prompt | Indented |\n",
                "\n",
                "  ---",
            ),
        )],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let rendered_lines = lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>();
    let text = rendered_lines.join("\n");

    // Assert
    assert!(text.contains('┌'));
    assert!(text.contains("Prompt"));
    assert!(text.contains("Indented"));
    assert!(!text.contains("| --- | --- |"));
    assert!(rendered_lines.iter().any(|line| {
        line.strip_prefix(prompt_block::user_prompt_continuation_prefix().as_str())
            .is_some_and(|content| {
                content.len() > 10 && content.chars().all(|character| character == '-')
            })
    }));
}

#[test]
fn test_output_lines_render_user_prompt_markdown_with_minimum_content_width() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![(SessionMessageKind::UserPrompt, "alpha beta")],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 3, 8), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(text.contains("..."));
    assert!(lines.iter().all(|line| line.width() <= 3));
}

#[test]
fn test_output_lines_render_user_prompt_mermaid_with_uniform_background() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![(
            SessionMessageKind::UserPrompt,
            concat!(
                "```mermaid {theme=default}\n",
                "flowchart TD\n",
                "    A[Start] --> B[Finish]\n",
                "```",
            ),
        )],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let start_line = lines
        .iter()
        .find(|line| line.to_string().contains("Start"))
        .expect("Mermaid diagram label should render");
    let border_line = lines
        .iter()
        .find(|line| line.to_string().contains('┌'))
        .expect("Mermaid diagram border should render");

    // Assert
    assert!(text.contains("Start"));
    assert!(text.contains("Finish"));
    assert!(text.contains("▼"));
    assert!(!text.contains("flowchart TD"));
    assert!(!text.contains("```"));
    assert_eq!(start_line.width(), 80);
    assert_eq!(
        start_line.spans[0].style.bg,
        Some(style::palette::surface_prompt())
    );
    assert!(start_line.spans.iter().any(|span| {
        span.content.as_ref().trim().is_empty()
            && span.style.bg == Some(style::palette::surface_prompt())
    }));
    assert!(
        start_line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(style::palette::surface_prompt()))
    );
    assert!(border_line.spans.iter().any(|span| {
        span.content.as_ref().contains('┌')
            && span.style.fg == Some(style::palette::text())
            && span.style.bg == Some(style::palette::surface_prompt())
    }));
    assert!(
        border_line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(style::palette::surface_prompt()))
    );
}

#[test]
fn test_output_lines_keep_prompt_shading_for_mermaid_prefix_language() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![(
            SessionMessageKind::UserPrompt,
            concat!(
                "```mermaids\n",
                "flowchart TD\n",
                "    A[Start] --> B[Finish]\n",
                "```",
            ),
        )],
    );
    session.status = Status::Review;
    session.reconcile_transient_messages();
    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let source_line = lines
        .iter()
        .find(|line| line.to_string().contains("flowchart TD"))
        .expect("non-Mermaid source should remain visible");

    // Assert
    assert!(text.contains("flowchart TD"));
    assert!(text.contains("A[Start] --> B[Finish]"));
    assert!(!text.contains("▼"));
    assert_eq!(source_line.width(), 80);
    assert!(
        source_line
            .spans
            .iter()
            .any(|span| span.style.bg == Some(style::palette::surface_prompt()))
    );
}
