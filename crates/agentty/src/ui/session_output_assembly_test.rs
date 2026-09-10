use ratatui::text::Line;

use super::{
    SESSION_OUTPUT_BLOCK_ORDER, SessionOutputAssembly, SessionOutputTranscriptSection,
    append_markdown_lines, append_queued_entries, append_transient_message, append_user_prompt,
    output_assembly, protect_user_prompt_indentation, review_comment_resolution_loading_message,
    typed_transcript_sections,
};
use crate::domain::session::{QueuedMessage, Session, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPrompt;
use crate::ui::{markdown, style};

fn queued_message(order: u64, text: &str) -> QueuedMessage {
    QueuedMessage::new(order, TurnPrompt::from_text(text.to_string()))
}

#[test]
fn test_section_display_text_handles_empty_and_markdown_sections() {
    // Arrange
    let empty_section = SessionOutputTranscriptSection::Empty;
    let markdown_section = SessionOutputTranscriptSection::Markdown("draft preview".to_string());

    // Act
    let empty_text = section_display_text(&empty_section);
    let markdown_text = section_display_text(&markdown_section);

    // Assert
    assert!(empty_section.is_empty());
    assert!(!markdown_section.is_empty());
    assert_eq!(empty_text, "");
    assert_eq!(markdown_text, "draft preview");
}

#[test]
fn test_queued_messages_skip_blank_entries() {
    // Arrange
    let mut lines = Vec::new();
    let queued_messages = vec![
        queued_message(0, " \n\t"),
        queued_message(1, "queued reply"),
    ];

    // Act
    append_queued_entries(&mut lines, &[], &queued_messages);

    // Assert
    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["", "≡ queued › queued reply", ""]
    );
}

#[test]
fn test_empty_preparation_marker_preserves_draft_output_rows() {
    for messages in [
        vec![],
        vec![SessionMessage::conversation(
            0,
            SessionMessageKind::UserPrompt,
            "A saved prompt",
        )],
    ] {
        // Arrange
        let mut session = crate::test_support::session_fixture("preparing", Status::Draft);
        session.transcript = Some(SessionTranscript::new(messages));
        let expected = output_lines(&session, 80, None, None);
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Plain(String::new()),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::WorkspacePreparation,
            turn_position: None,
        });

        // Act
        let actual = output_lines(&session, 80, None, None);

        // Assert
        assert_eq!(actual.lines, expected.lines);
        assert_eq!(actual.transient_loader_line_index, None);
        assert!(session.allows_cancel_action());
    }
}

#[test]
fn test_transient_message_appender_skips_queued_actions() {
    // Arrange
    let mut lines = Vec::new();
    let message = TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(
            0,
            "sync after this turn".to_string(),
        )),
        lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::SyncQueue,
        turn_position: None,
    };

    // Act
    let loader_line_index = append_transient_message(&mut lines, &message, 80, None);

    // Assert
    assert_eq!(loader_line_index, None);
    assert_eq!(lines, []);
}

#[test]
fn test_generated_review_prompt_is_hidden_behind_resolution_loader() {
    // Arrange
    let mut session = crate::test_support::SessionFixtureBuilder::new()
        .status(Status::InProgress)
        .build();
    session.transcript = Some(SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "initial request"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "initial answer"),
        SessionMessage::conversation(
            2,
            SessionMessageKind::AgentPrompt,
            "Process the following selected forge review comments",
        ),
    ]));
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Resolving 3 review comments...".to_string()),
        lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::ReviewCommentResolution,
        turn_position: None,
    });

    // Act
    let output = output_lines(&session, 80, Some("Inspecting files"), None);
    let rendered_text = output
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(rendered_text.contains("initial request"));
    assert!(rendered_text.contains("initial answer"));
    assert!(rendered_text.contains("Resolving 3 review comments..."));
    assert!(!rendered_text.contains("Process the following"));
    assert!(!rendered_text.contains("Inspecting files"));
    assert!(output.active_loader_line_index.is_some());
    assert_eq!(output.transient_loader_line_index, None);
}

#[test]
fn test_review_comment_resolution_loader_ignores_non_loading_body() {
    // Arrange
    let mut session = crate::test_support::SessionFixtureBuilder::new().build();
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Plain("not loading".to_string()),
        lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::ReviewCommentResolution,
        turn_position: None,
    });

    // Act
    let loading_message = review_comment_resolution_loading_message(&session);

    // Assert
    assert_eq!(loading_message, None);
}

#[test]
fn test_queued_entries_follow_shared_submission_order() {
    // Arrange
    let mut lines = vec![Line::from("Active turn")];
    let transient_messages = vec![
        TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Queued(QueuedAction::new(
                0,
                "sync after this turn".to_string(),
            )),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::SyncQueue,
            turn_position: Some(1),
        },
        TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Queued(QueuedAction::new(
                2,
                "publish review request".to_string(),
            )),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::BranchPublish,
            turn_position: Some(1),
        },
    ];
    let queued_messages = vec![queued_message(1, "follow up")];

    // Act
    let queued_line_indices =
        append_queued_entries(&mut lines, &transient_messages, &queued_messages);

    // Assert
    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "Active turn",
            "",
            "≡ sync after this turn",
            "≡ queued › follow up",
            "≡ publish review request",
            "",
        ]
    );
    assert_eq!(queued_line_indices, [2, 3, 4]);
}

#[test]
fn test_blank_user_prompt_does_not_add_output_lines() {
    // Arrange
    let mut lines = Vec::new();

    // Act
    append_user_prompt(&mut lines, " \n\t", 80, None);

    // Assert
    assert_eq!(lines, [] as [ratatui::prelude::Line<'_>; 0]);
}

#[test]
fn test_user_prompt_highlights_at_lookup_file() {
    // Arrange
    let mut lines = Vec::new();
    let lookup = "@crates/agentty/src/ui/markdown.rs";

    // Act
    append_user_prompt(
        &mut lines,
        &format!("Review {lookup} before replying"),
        80,
        None,
    );

    // Assert
    let lookup_span = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.as_ref() == lookup)
        .expect("file lookup should render as one highlighted span");
    assert_eq!(lookup_span.style.fg, Some(style::palette::info()));
    assert_eq!(lookup_span.style.bg, Some(style::palette::surface_prompt()));
}

#[test]
fn test_zero_width_user_prompt_does_not_add_output_lines() {
    // Arrange
    let mut lines = Vec::new();

    // Act
    append_user_prompt(&mut lines, "\u{200b}", 80, None);

    // Assert
    assert_eq!(lines, [] as [ratatui::prelude::Line<'_>; 0]);
}

#[test]
fn test_zero_width_markdown_does_not_add_output_lines() {
    // Arrange
    let mut lines = Vec::new();

    // Act
    append_markdown_lines(&mut lines, "\u{200b}", 80, None);

    // Assert
    assert_eq!(lines, [] as [ratatui::prelude::Line<'_>; 0]);
}

#[test]
fn test_protected_prompt_falls_back_when_private_use_is_exhausted() {
    // Arrange
    let prompt_text = [
        0xe000..=0xf8ff,
        0x000f_0000..=0x000f_fffd,
        0x0010_0000..=0x0010_fffd,
    ]
    .into_iter()
    .flatten()
    .filter_map(char::from_u32)
    .collect::<String>();

    // Act
    let (protected_text, indent_marker) = protect_user_prompt_indentation(&prompt_text);

    // Assert
    assert_eq!(indent_marker, None);
    assert_eq!(protected_text, prompt_text);
}

#[test]
fn test_output_lines_places_queued_messages_after_active_turn() {
    // Arrange
    let mut session = crate::test_support::SessionFixtureBuilder::new()
        .status(Status::InProgress)
        .build();
    session.transcript = Some(SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "first prompt"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "first answer"),
        SessionMessage::conversation(2, SessionMessageKind::UserPrompt, "active prompt"),
    ]));
    session.queued_messages = vec![queued_message(0, "queued reply")];

    // Act
    let output = output_lines(&session, 80, None, None)
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    let active_prompt_index = output
        .find("active prompt")
        .expect("active prompt should be rendered");
    let queued_reply_index = output
        .find("queued › queued reply")
        .expect("queued reply should be rendered");

    assert!(active_prompt_index < queued_reply_index);
}

/// Fully assembled session-output lines plus metadata derived during assembly.
pub(crate) struct SessionOutputLines {
    pub(crate) active_loader_line_index: Option<usize>,
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) queued_line_indices: Vec<usize>,
    pub(crate) transient_loader_line_index: Option<usize>,
}
/// Assembles a complete session-output panel in canonical display order.
pub(crate) fn output_lines(
    session: &Session,
    inner_width: usize,
    active_progress: Option<&str>,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> SessionOutputLines {
    output_assembly(session, inner_width, active_progress, markdown_render_cache)
        .into_output_lines()
}
/// Returns display text for typed transcript sections in canonical order.
pub(crate) fn transcript_section_texts(
    status: Status,
    transcript: &SessionTranscript,
) -> (String, String, String) {
    let sections = typed_transcript_sections(status, transcript);

    (
        section_display_text(&sections.completed_turn),
        section_display_text(&sections.active_turn),
        section_display_text(&sections.trailing_notice),
    )
}
fn section_display_text(section: &SessionOutputTranscriptSection<'_>) -> String {
    match section {
        SessionOutputTranscriptSection::Empty => String::new(),
        SessionOutputTranscriptSection::Markdown(markdown) => markdown.clone(),
        SessionOutputTranscriptSection::Messages(messages) => {
            SessionTranscript::display_text_for_messages(messages)
        }
    }
}
/// Appends queued chat rows in submission order beneath the active turn.
pub(crate) fn append_queued_message_lines(
    lines: &mut Vec<Line<'static>>,
    queued_messages: &[QueuedMessage],
) {
    append_queued_entries(lines, &[], queued_messages);
}
/// Appends one user prompt block while retaining its prompt marker and shading.
pub(crate) fn append_user_prompt_markdown_lines(
    lines: &mut Vec<Line<'static>>,
    prompt_text: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    append_user_prompt(lines, prompt_text, inner_width, markdown_render_cache);
}
impl SessionOutputAssembly<'_> {
    fn into_output_lines(mut self) -> SessionOutputLines {
        for block in SESSION_OUTPUT_BLOCK_ORDER {
            self.append_block(block);
        }

        SessionOutputLines {
            active_loader_line_index: self.active_loader_line_index,
            lines: self.lines,
            queued_line_indices: self.queued_line_indices,
            transient_loader_line_index: self.transient_loader_line_index,
        }
    }
}
