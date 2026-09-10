use std::sync::Arc;

use ratatui::layout::Rect;
use ratatui::text::Line;

use super::super::{SessionOutput, SessionOutputLayoutCache, SessionOutputLineContext};
use super::support::{
    line_context, output_lines, queued_message, session_fixture, set_assistant_transcript,
    set_conversation_transcript,
};
use crate::domain::session::Status;
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::{
    QueuedAction, TransientMessage, TransientMessageAnchor, TransientMessageBody,
    TransientMessageLifecycle, TransientMessageSlot,
};
use crate::ui::icon::Icon;
use crate::ui::render::Component;
use crate::ui::{markdown, session_output_assembly, style};

/// Verifies queued-message edge trimming preserves blank lines within a
/// multiline message.
#[test]
fn test_append_queued_message_lines_trims_only_outer_empty_lines() {
    // Arrange
    let mut lines = vec![Line::from("Previous message.")];
    let queued_messages = vec![
        queued_message(0, "\nFirst queued message.\n"),
        queued_message(1, " \nSecond queued message.\n\nMore context.\n\t"),
    ];

    // Act
    session_output_assembly::tests::append_queued_message_lines(&mut lines, &queued_messages);
    let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

    // Assert
    assert_eq!(
        rendered_lines,
        vec![
            "Previous message.",
            "",
            "≡ queued › First queued message.",
            "≡ queued › Second queued message.",
            "           ",
            "           More context.",
            "",
        ]
    );
}

#[test]
/// Verifies queued chat rows invalidate the output layout cache so
/// in-progress replies appear as soon as they are staged.
fn test_output_layout_cache_keys_queued_messages() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::InProgress;
    set_assistant_transcript(&mut session, " › running prompt");
    let markdown_render_cache = markdown::MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let context = line_context();

    // Act
    let empty_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );
    session.queued_messages = vec![queued_message(0, "queued reply")];
    let queued_layout = output_layout_cache.layout(
        &session,
        Rect::new(0, 0, 80, 8),
        context,
        Some(&markdown_render_cache),
    );
    let queued_text = queued_layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    // Assert
    assert!(!Arc::ptr_eq(&empty_layout.lines, &queued_layout.lines));
    assert!(queued_text.contains("queued › queued reply"));
}

#[test]
fn test_render_animates_transient_loader_and_queued_indicator() {
    // Arrange
    let mut session = session_fixture();
    session.status = Status::Review;
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Publishing review request...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::BranchPublish,
        turn_position: None,
    });
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Queued(QueuedAction::new(
            0,
            "sync after this turn".to_string(),
        )),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::SyncQueue,
        turn_position: None,
    });
    let backend = ratatui::backend::TestBackend::new(80, 10);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let tachyon_cell_symbol = Icon::TachyonLoader
        .as_str()
        .chars()
        .next()
        .expect("tachyon loader should contain a cell")
        .to_string();

    // Act
    terminal
        .draw(|frame| {
            SessionOutput::new(&session)
                .spinner_frame(5)
                .render(frame, frame.area());
        })
        .expect("failed to draw session output");
    let buffer = terminal.backend().buffer();
    let queued_indicator = buffer
        .content()
        .iter()
        .find(|cell| cell.symbol() == Icon::QueuedAction.as_str())
        .expect("queued indicator should be rendered");

    // Assert
    assert_ne!(queued_indicator.fg, style::palette::text_subtle());
    assert!(buffer.content().iter().any(|cell| {
        cell.symbol() == tachyon_cell_symbol && cell.fg == style::palette::warning()
    }));
}

/// Verifies queued follow-up messages render after the active turn.
#[test]
fn test_output_lines_in_progress_session_shows_queued_messages_after_active_turn() {
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
            (SessionMessageKind::AssistantAnswer, "working"),
        ],
    );
    session.queued_messages = vec![queued_message(0, "follow up\nwith context")];
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
    let queued_index = text
        .find("queued › follow up")
        .expect("queued message should be rendered");

    // Assert
    assert!(!text.contains("Change Summary"));
    assert!(commit_index < prompt_index);
    assert!(prompt_index < queued_index);
    assert!(text.contains("           with context"));
}

/// Verifies a queued follow-up remains below workflow notices that were
/// already visible when a session sync accepted the message.
#[test]
fn test_output_lines_rebasing_session_places_queued_message_after_workflow_notices() {
    // Arrange
    let mut session = session_fixture();
    set_conversation_transcript(
        &mut session,
        vec![
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            ),
            (
                SessionMessageKind::WorkflowNotice,
                "\n[Sync Assist] Attempt 1/3. Resolving conflicts.\n",
            ),
        ],
    );
    session.status = Status::Rebasing;
    session.queued_messages = vec![queued_message(0, "address review comments")];

    // Act
    let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let commit_index = text
        .find("[Commit] No changes to commit.")
        .expect("commit notice should be rendered");
    let sync_assist_index = text
        .find("[Sync Assist] Attempt 1/3.")
        .expect("sync-assist notice should be rendered");
    let queued_message_index = text
        .find("queued › address review comments")
        .expect("queued follow-up should be rendered");

    // Assert
    assert!(commit_index < sync_assist_index);
    assert!(sync_assist_index < queued_message_index);
}
