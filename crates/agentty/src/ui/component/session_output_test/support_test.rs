use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::Line;

use super::super::{SessionOutputLayout, SessionOutputLayoutLines, SessionOutputLineContext};
use crate::domain::session::{QueuedMessage, Session, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPrompt;
use crate::ui::input_layout::panel_inner_width;
use crate::ui::{markdown, session_format, session_output_assembly};

/// Builds one output-line context with defaults suitable for tests.
pub(super) fn line_context() -> SessionOutputLineContext<'static> {
    SessionOutputLineContext {
        active_prompt_output: None,
        active_progress: None,
        session_update_version: 0,
    }
}

pub(super) fn queued_message(order: u64, text: &str) -> QueuedMessage {
    QueuedMessage::new(order, TurnPrompt::from_text(text.to_string()))
}

/// Posts one focused-review slot for renderer tests.
pub(super) fn set_review_transient(session: &mut Session, body: TransientMessageBody) {
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::AfterCompletedTurn,
        body,
        lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
        slot: TransientMessageSlot::Review,
        turn_position: session.latest_user_prompt_position(),
    });
}

pub(super) fn session_fixture() -> Session {
    crate::test_support::SessionFixtureBuilder::new()
        .status(Status::Draft)
        .build()
}

/// Builds rendered lines without exposing row metadata to tests that only
/// assert text content.
pub(super) fn output_lines(
    session: &Session,
    output_area: Rect,
    context: SessionOutputLineContext<'_>,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> Vec<Line<'static>> {
    let inner_width =
        panel_inner_width(output_area, session_format::session_output_panel_borders());
    session_output_assembly::tests::output_lines(
        session,
        inner_width,
        context.active_progress,
        markdown_render_cache,
    )
    .lines
}

pub(super) fn table_header_background(layout: &SessionOutputLayout) -> Option<Color> {
    layout
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref().contains("Input"))
        .and_then(|span| span.style.bg)
}

pub(super) fn set_assistant_transcript(session: &mut Session, output: &str) {
    let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        output,
    )]);
    session.transcript = Some(transcript);
}

pub(super) fn set_conversation_transcript(
    session: &mut Session,
    messages: Vec<(SessionMessageKind, &str)>,
) {
    let transcript = SessionTranscript::new(
        messages
            .into_iter()
            .enumerate()
            .map(|(position, (kind, content))| {
                let position = i64::try_from(position).unwrap_or(i64::MAX);
                if kind.is_conversation_message() {
                    SessionMessage::conversation(position, kind, content)
                } else {
                    SessionMessage::new(position, kind, content)
                }
            })
            .collect(),
    );
    session.transcript = Some(transcript);
}

impl SessionOutputLayoutLines {
    pub(super) fn iter(&self) -> impl Iterator<Item = &Line<'static>> {
        self.body[..self.body_line_count]
            .iter()
            .chain(self.tail.iter())
    }
}
