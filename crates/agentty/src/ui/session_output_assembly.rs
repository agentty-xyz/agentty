//! Pure display-block assembly for the session-output panel.
//!
//! This module owns transcript classification, display ordering, and line
//! spacing. The `session_output` component owns layout caching and Ratatui
//! painting, so callers can exercise transcript projection without a frame.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use ag_tui_text::text_util;
use ratatui::text::Line;

use crate::domain::session::{Session, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageSlot,
};
use crate::ui::markdown::{self, render_markdown};
use crate::ui::prompt_block::{self, USER_PROMPT_PREFIX, USER_PROMPT_RIGHT_GUTTER_WIDTH};
use crate::ui::{session_format, style};

const DRAFT_PREVIEW_HEADER: &str = "## Draft Session";
const DRAFT_PREVIEW_EMPTY_NOTE: &str = "No draft messages staged yet. Use `Enter` to stage the \
                                        first draft locally, then press `s` in session view to \
                                        start the bundle.";
const DRAFT_PREVIEW_STACKED_EMPTY_NOTE: &str = "No draft messages staged yet. Use `Enter` to \
                                                stage the first draft locally. The `s` start \
                                                action appears after the parent is review-ready.";
const DRAFT_PREVIEW_STAGED_NOTE: &str =
    "Draft messages stay local until you press `s` in session view to start the staged bundle.";
const DRAFT_PREVIEW_STACKED_STAGED_NOTE: &str =
    "Draft messages stay local until the parent is review-ready and you press `s` in session view \
     to start the stacked bundle from its parent branch.";
const USER_PROMPT_TAB_WIDTH: usize = 4;

/// Fully assembled session-output lines plus metadata derived during assembly.
pub(crate) struct SessionOutputLines {
    pub(crate) active_loader_line_index: Option<usize>,
    pub(crate) branch_operation_loader_line_index: Option<usize>,
    pub(crate) lines: Vec<Line<'static>>,
}

/// Cached transcript body that excludes the dynamic session-status tail.
#[derive(Clone)]
pub(crate) struct SessionOutputBody {
    pub(crate) branch_operation_loader_line_index: Option<usize>,
    pub(crate) lines: Arc<[Line<'static>]>,
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

/// Assembles the stable transcript body without the dynamic status tail.
pub(crate) fn output_body(
    session: &Session,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> SessionOutputBody {
    output_assembly(session, inner_width, None, markdown_render_cache).into_output_body()
}

/// Appends the dynamic status tail to a cached stable transcript body.
pub(crate) fn layout_from_body(
    session: &Session,
    active_progress: Option<&str>,
    body: &SessionOutputBody,
) -> SessionOutputLines {
    let mut lines = body.lines.iter().cloned().collect::<Vec<_>>();
    let active_loader_line_index = append_session_tail_lines(
        &mut lines,
        session.status,
        active_progress,
        review_loading_message(session),
    );

    SessionOutputLines {
        active_loader_line_index,
        branch_operation_loader_line_index: body.branch_operation_loader_line_index,
        lines,
    }
}

/// Returns whether the status owns a live or queued turn whose newest prompt
/// remains separate from completed transcript content.
pub(crate) fn status_has_active_turn(status: Status) -> bool {
    matches!(status, Status::InProgress | Status::Queued)
}

/// Returns display text for typed transcript sections in canonical order.
#[cfg(test)]
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

#[cfg(test)]
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
#[cfg(test)]
pub(crate) fn append_queued_message_lines(
    lines: &mut Vec<Line<'static>>,
    queued_messages: &[String],
) {
    append_queued_messages(lines, queued_messages);
}

/// Appends one user prompt block while retaining its prompt marker and shading.
#[cfg(test)]
pub(crate) fn append_user_prompt_markdown_lines(
    lines: &mut Vec<Line<'static>>,
    prompt_text: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    append_user_prompt(lines, prompt_text, inner_width, markdown_render_cache);
}

#[derive(Clone, Copy)]
enum SessionOutputBlock {
    ActiveTurn,
    CompletedTranscript,
    QueuedMessage,
    SessionTail,
    Transient(TransientMessageAnchor),
    TrailingTranscriptNotice(TrailingTranscriptNoticePlacement),
}

#[derive(Clone, Copy)]
enum TrailingTranscriptNoticePlacement {
    AfterReview,
    BeforeActiveTurn,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SessionOutputSeparator {
    Always,
    AfterPreviousContent,
}

const SESSION_OUTPUT_BLOCK_ORDER: [SessionOutputBlock; 9] = [
    SessionOutputBlock::CompletedTranscript,
    SessionOutputBlock::TrailingTranscriptNotice(
        TrailingTranscriptNoticePlacement::BeforeActiveTurn,
    ),
    SessionOutputBlock::Transient(TransientMessageAnchor::AfterCompletedTurn),
    SessionOutputBlock::ActiveTurn,
    SessionOutputBlock::Transient(TransientMessageAnchor::AfterActiveTurn),
    SessionOutputBlock::TrailingTranscriptNotice(TrailingTranscriptNoticePlacement::AfterReview),
    SessionOutputBlock::QueuedMessage,
    SessionOutputBlock::Transient(TransientMessageAnchor::Tail),
    SessionOutputBlock::SessionTail,
];

struct SessionOutputAssembly<'a> {
    active_loader_line_index: Option<usize>,
    active_progress: Option<&'a str>,
    active_turn_has_visible_text: bool,
    active_turn_section: SessionOutputTranscriptSection<'a>,
    branch_operation_loader_line_index: Option<usize>,
    completed_turn_section: SessionOutputTranscriptSection<'a>,
    inner_width: usize,
    lines: Vec<Line<'static>>,
    markdown_render_cache: Option<&'a markdown::MarkdownRenderCache>,
    session: &'a Session,
    status: Status,
    trailing_notice_section: SessionOutputTranscriptSection<'a>,
}

struct SessionOutputTextSections<'a> {
    active_turn: SessionOutputTranscriptSection<'a>,
    completed_turn: SessionOutputTranscriptSection<'a>,
    trailing_notice: SessionOutputTranscriptSection<'a>,
}

enum SessionOutputTranscriptSection<'a> {
    Empty,
    Markdown(String),
    Messages(&'a [SessionMessage]),
}

impl SessionOutputTranscriptSection<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Markdown(text) => text.trim().is_empty(),
            Self::Messages(messages) => messages
                .iter()
                .all(|message| message.content.trim().is_empty()),
        }
    }
}

impl SessionOutputAssembly<'_> {
    fn into_output_lines(mut self) -> SessionOutputLines {
        for block in SESSION_OUTPUT_BLOCK_ORDER {
            self.append_block(block);
        }

        SessionOutputLines {
            active_loader_line_index: self.active_loader_line_index,
            branch_operation_loader_line_index: self.branch_operation_loader_line_index,
            lines: self.lines,
        }
    }

    fn into_output_body(mut self) -> SessionOutputBody {
        for block in SESSION_OUTPUT_BLOCK_ORDER {
            if !matches!(block, SessionOutputBlock::SessionTail) {
                self.append_block(block);
            }
        }

        SessionOutputBody {
            branch_operation_loader_line_index: self.branch_operation_loader_line_index,
            lines: Arc::from(self.lines),
        }
    }

    fn append_block(&mut self, block: SessionOutputBlock) {
        match block {
            SessionOutputBlock::CompletedTranscript => self.append_completed_transcript(),
            SessionOutputBlock::TrailingTranscriptNotice(placement) => {
                self.append_trailing_transcript_notice(placement);
            }
            SessionOutputBlock::Transient(anchor) => self.append_transient_messages(anchor),
            SessionOutputBlock::ActiveTurn => self.append_active_turn(),
            SessionOutputBlock::QueuedMessage => self.append_queued_messages(),
            SessionOutputBlock::SessionTail => self.append_session_tail(),
        }
    }

    fn append_completed_transcript(&mut self) {
        append_transcript_section(
            &mut self.lines,
            &self.completed_turn_section,
            self.inner_width,
            self.markdown_render_cache,
        );
    }

    fn append_trailing_transcript_notice(&mut self, placement: TrailingTranscriptNoticePlacement) {
        let should_append = match placement {
            TrailingTranscriptNoticePlacement::BeforeActiveTurn => {
                self.active_turn_has_visible_text
            }
            TrailingTranscriptNoticePlacement::AfterReview => !self.active_turn_has_visible_text,
        };
        if should_append {
            append_transcript_section(
                &mut self.lines,
                &self.trailing_notice_section,
                self.inner_width,
                self.markdown_render_cache,
            );
        }
    }

    fn append_transient_messages(&mut self, anchor: TransientMessageAnchor) {
        for message in self
            .session
            .transient_messages
            .messages()
            .iter()
            .filter(|message| message.anchor == anchor)
        {
            if append_transient_message(
                &mut self.lines,
                message,
                self.inner_width,
                self.markdown_render_cache,
            ) {
                self.branch_operation_loader_line_index = Some(self.lines.len().saturating_sub(1));
            }
        }
    }

    fn append_active_turn(&mut self) {
        append_transcript_section(
            &mut self.lines,
            &self.active_turn_section,
            self.inner_width,
            self.markdown_render_cache,
        );
    }

    fn append_queued_messages(&mut self) {
        append_queued_messages(&mut self.lines, &self.session.queued_messages);
    }

    fn append_session_tail(&mut self) {
        self.active_loader_line_index = append_session_tail_lines(
            &mut self.lines,
            self.status,
            self.active_progress,
            review_loading_message(self.session),
        );
    }
}

fn output_assembly<'assembly>(
    session: &'assembly Session,
    inner_width: usize,
    active_progress: Option<&'assembly str>,
    markdown_render_cache: Option<&'assembly markdown::MarkdownRenderCache>,
) -> SessionOutputAssembly<'assembly> {
    let status = session.status;
    let transcript_sections = output_text_sections(session, status);
    let active_turn_has_visible_text = !transcript_sections.active_turn.is_empty();

    SessionOutputAssembly {
        active_loader_line_index: None,
        active_progress,
        active_turn_has_visible_text,
        active_turn_section: transcript_sections.active_turn,
        branch_operation_loader_line_index: None,
        completed_turn_section: transcript_sections.completed_turn,
        inner_width,
        lines: Vec::new(),
        markdown_render_cache,
        session,
        status,
        trailing_notice_section: transcript_sections.trailing_notice,
    }
}

fn append_block_separator(lines: &mut Vec<Line<'static>>, separator: SessionOutputSeparator) {
    trim_trailing_blank_lines(lines);

    if separator == SessionOutputSeparator::Always || !lines.is_empty() {
        lines.push(Line::from(""));
    }
}

fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
    }
}

fn append_session_tail_lines(
    lines: &mut Vec<Line<'static>>,
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
) -> Option<usize> {
    if let Some(status_line) =
        session_format::session_output_status_line(status, active_progress, review_status_message)
    {
        append_block_separator(lines, SessionOutputSeparator::Always);
        let active_loader_line_index =
            session_format::session_output_uses_tachyon_loader(status).then_some(lines.len());
        lines.push(status_line);

        return active_loader_line_index;
    }

    if status == Status::Done {
        lines.push(Line::from(""));
        lines.push(session_format::session_output_done_line());
        lines.push(Line::from(""));

        return None;
    }

    lines.push(Line::from(""));

    None
}

fn review_loading_message(session: &Session) -> Option<&str> {
    session
        .transient_messages
        .get(TransientMessageSlot::Review)
        .and_then(|message| match &message.body {
            TransientMessageBody::Loading(message) => Some(message.as_str()),
            TransientMessageBody::Markdown(_) | TransientMessageBody::Plain(_) => None,
        })
}

fn append_transient_message(
    lines: &mut Vec<Line<'static>>,
    message: &TransientMessage,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> bool {
    match &message.body {
        TransientMessageBody::Markdown(markdown) => {
            let markdown = match message.slot {
                TransientMessageSlot::Summary => {
                    session_format::session_output_summary_markdown(markdown)
                }
                TransientMessageSlot::Review => session_format::format_review_markdown(markdown),
                TransientMessageSlot::WorkflowNotice
                | TransientMessageSlot::BranchPublish
                | TransientMessageSlot::PublishedBranchSync => markdown.clone(),
            };
            append_markdown_lines(lines, &markdown, inner_width, markdown_render_cache);
        }
        TransientMessageBody::Plain(status_message) => {
            append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
            append_plain_status_lines(lines, status_message, inner_width);
        }
        TransientMessageBody::Loading(status_message) => {
            if message.slot == TransientMessageSlot::Review {
                return false;
            }

            append_block_separator(lines, SessionOutputSeparator::Always);
            lines.push(session_format::session_output_transient_loading_line(
                status_message,
            ));
        }
    }

    matches!(
        message.slot,
        TransientMessageSlot::BranchPublish | TransientMessageSlot::PublishedBranchSync
    ) && matches!(&message.body, TransientMessageBody::Loading(_))
}

fn output_text_sections(session: &Session, status: Status) -> SessionOutputTextSections<'_> {
    if session.status == Status::Draft && session.is_draft_session() {
        return SessionOutputTextSections {
            active_turn: SessionOutputTranscriptSection::Empty,
            completed_turn: SessionOutputTranscriptSection::Markdown(render_draft_session_preview(
                session,
            )),
            trailing_notice: SessionOutputTranscriptSection::Empty,
        };
    }

    if let Some(transcript) = session
        .transcript
        .as_ref()
        .filter(|transcript| !transcript.is_empty())
    {
        return typed_transcript_sections(status, transcript);
    }

    SessionOutputTextSections {
        active_turn: SessionOutputTranscriptSection::Empty,
        completed_turn: SessionOutputTranscriptSection::Empty,
        trailing_notice: SessionOutputTranscriptSection::Empty,
    }
}

fn typed_transcript_sections(
    status: Status,
    transcript: &SessionTranscript,
) -> SessionOutputTextSections<'_> {
    let messages = transcript.messages();
    let active_prompt_index =
        active_prompt_message_index(status, messages).unwrap_or(messages.len());
    let (completed_messages, active_messages) = messages.split_at(active_prompt_index);
    let trailing_notice_start =
        trailing_workflow_notice_start(completed_messages).unwrap_or(completed_messages.len());
    let (completed_messages, trailing_notice_messages) =
        completed_messages.split_at(trailing_notice_start);

    SessionOutputTextSections {
        active_turn: messages_section(active_messages),
        completed_turn: messages_section(completed_messages),
        trailing_notice: messages_section(trailing_notice_messages),
    }
}

fn messages_section(messages: &[SessionMessage]) -> SessionOutputTranscriptSection<'_> {
    if messages.is_empty() {
        return SessionOutputTranscriptSection::Empty;
    }

    SessionOutputTranscriptSection::Messages(messages)
}

fn active_prompt_message_index(status: Status, messages: &[SessionMessage]) -> Option<usize> {
    if !status_has_active_turn(status) {
        return None;
    }

    messages
        .iter()
        .rposition(|message| message.kind == SessionMessageKind::UserPrompt)
}

fn trailing_workflow_notice_start(messages: &[SessionMessage]) -> Option<usize> {
    if messages.is_empty() {
        return None;
    }

    let Some(first_non_notice_from_end) = messages
        .iter()
        .rposition(|message| message.kind != SessionMessageKind::WorkflowNotice)
    else {
        return Some(0);
    };
    let notice_start = first_non_notice_from_end.saturating_add(1);

    (notice_start < messages.len()).then_some(notice_start)
}

fn render_draft_session_preview(session: &Session) -> String {
    let mut output = String::from(DRAFT_PREVIEW_HEADER);

    if session.has_staged_drafts() {
        let draft_note = if session.is_stacked_child() {
            DRAFT_PREVIEW_STACKED_STAGED_NOTE
        } else {
            DRAFT_PREVIEW_STAGED_NOTE
        };
        let _ = write!(output, "\n\n{draft_note}\n\n");
        output.push_str(&staged_draft_transcript_block(&session.prompt));
    } else {
        let draft_note = if session.is_stacked_child() {
            DRAFT_PREVIEW_STACKED_EMPTY_NOTE
        } else {
            DRAFT_PREVIEW_EMPTY_NOTE
        };
        let _ = write!(output, "\n\n{draft_note}\n");
    }

    if let Some(transcript_text) = session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
    {
        let _ = write!(output, "\n\n{transcript_text}");
    }

    output
}

fn staged_draft_transcript_block(prompt_text: &str) -> String {
    let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
    let mut formatted_lines = Vec::with_capacity(prompt_lines.len());
    let continuation_prefix = prompt_block::user_prompt_continuation_prefix();

    for (index, prompt_line) in prompt_lines.into_iter().enumerate() {
        let prefix = if index == 0 {
            USER_PROMPT_PREFIX
        } else {
            continuation_prefix.as_str()
        };
        formatted_lines.push(format!("{prefix}{prompt_line}"));
    }

    format!("{}\n\n", formatted_lines.join("\n"))
}

fn append_plain_status_lines(
    lines: &mut Vec<Line<'static>>,
    status_message: &str,
    inner_width: usize,
) {
    let rendered_lines = text_util::wrap_lines(status_message, inner_width)
        .into_iter()
        .map(|line| Line::from(line.to_string()));
    lines.extend(rendered_lines);
}

fn append_transcript_section(
    lines: &mut Vec<Line<'static>>,
    section: &SessionOutputTranscriptSection<'_>,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    match section {
        SessionOutputTranscriptSection::Empty => {}
        SessionOutputTranscriptSection::Markdown(markdown) => {
            append_markdown_lines(lines, markdown, inner_width, markdown_render_cache);
        }
        SessionOutputTranscriptSection::Messages(messages) => {
            append_transcript_messages(lines, messages, inner_width, markdown_render_cache);
        }
    }
}

fn append_transcript_messages(
    lines: &mut Vec<Line<'static>>,
    messages: &[SessionMessage],
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    for message in messages {
        match message.kind {
            SessionMessageKind::UserPrompt => {
                append_user_prompt(lines, &message.content, inner_width, markdown_render_cache);
            }
            SessionMessageKind::AssistantAnswer | SessionMessageKind::WorkflowNotice => {
                append_markdown_lines(lines, &message.content, inner_width, markdown_render_cache);
            }
        }
    }
}

fn append_queued_messages(lines: &mut Vec<Line<'static>>, queued_messages: &[String]) {
    if queued_messages.is_empty() {
        return;
    }

    let queued_style = ratatui::style::Style::default()
        .fg(style::palette::text_subtle())
        .add_modifier(ratatui::style::Modifier::ITALIC);
    let mut has_rendered_message = false;
    for queued_text in queued_messages {
        let message_lines = queued_text.split('\n').collect::<Vec<_>>();
        let Some(first_content_line_index) = message_lines
            .iter()
            .position(|message_line| !message_line.trim().is_empty())
        else {
            continue;
        };
        let last_content_line_index = message_lines
            .iter()
            .rposition(|message_line| !message_line.trim().is_empty())
            .unwrap_or(first_content_line_index);
        let separator = if has_rendered_message {
            SessionOutputSeparator::AfterPreviousContent
        } else {
            SessionOutputSeparator::Always
        };
        append_block_separator(lines, separator);

        for (line_index, message_line) in message_lines
            [first_content_line_index..=last_content_line_index]
            .iter()
            .enumerate()
        {
            let prefix = if line_index == 0 {
                "queued › "
            } else {
                "        "
            };
            lines.push(Line::styled(
                format!("{prefix}{message_line}"),
                queued_style,
            ));
        }
        has_rendered_message = true;
    }

    if has_rendered_message {
        lines.push(Line::from(""));
    }
}

fn append_user_prompt(
    lines: &mut Vec<Line<'static>>,
    prompt_text: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    if prompt_text.trim().is_empty() {
        return;
    }

    let prompt_prefix_width = USER_PROMPT_PREFIX.chars().count();
    let prompt_content_width = inner_width
        .saturating_sub(prompt_prefix_width)
        .saturating_sub(USER_PROMPT_RIGHT_GUTTER_WIDTH)
        .max(1);
    let (protected_prompt_text, indent_marker) = protect_user_prompt_indentation(prompt_text);
    let rendered_lines = rendered_markdown_lines(
        &protected_prompt_text,
        prompt_content_width,
        markdown_render_cache,
    );
    let Some(first_visible_line_index) = rendered_lines.iter().position(|line| line.width() > 0)
    else {
        return;
    };
    let last_visible_line_index = rendered_lines
        .iter()
        .rposition(|line| line.width() > 0)
        .unwrap_or(first_visible_line_index);

    append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
    lines.push(prompt_block::user_prompt_padding_line(inner_width));

    let mut has_rendered_content_line = false;
    let continuation_prefix = prompt_block::user_prompt_continuation_prefix();
    for rendered_line in &rendered_lines[first_visible_line_index..=last_visible_line_index] {
        if rendered_line.width() == 0 {
            lines.push(prompt_block::user_prompt_padding_line(inner_width));

            continue;
        }

        let prefix = if has_rendered_content_line {
            continuation_prefix.as_str()
        } else {
            USER_PROMPT_PREFIX
        };
        let prefix_style = if has_rendered_content_line {
            prompt_block::user_prompt_content_style()
        } else {
            prompt_block::user_prompt_prefix_style()
        };
        lines.push(prompt_block::user_prompt_markdown_line(
            restored_user_prompt_spans(rendered_line, indent_marker),
            prefix,
            prefix_style,
            inner_width,
        ));
        has_rendered_content_line = true;
    }

    lines.push(prompt_block::user_prompt_padding_line(inner_width));
}

fn protect_user_prompt_indentation(prompt_text: &str) -> (String, Option<char>) {
    let Some(indent_marker) = unused_private_use_character(prompt_text) else {
        return (prompt_text.to_string(), None);
    };

    let mut protected_text = String::with_capacity(prompt_text.len());
    let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
    let preservation_mask = markdown::markdown_block_preservation_mask(prompt_text);

    for (line_index, line) in prompt_lines.into_iter().enumerate() {
        if line_index > 0 {
            protected_text.push('\n');
        }

        if preservation_mask[line_index] {
            protected_text.push_str(line);
        } else {
            let (content_start, indentation_width) = leading_indentation(line);
            let content = &line[content_start..];
            protected_text.extend(std::iter::repeat_n(indent_marker, indentation_width));
            protected_text.push_str(content);
        }
    }

    (protected_text, Some(indent_marker))
}

fn unused_private_use_character(prompt_text: &str) -> Option<char> {
    const DEFAULT_INDENT_MARKER: char = '\u{e000}';

    if !prompt_text.contains(DEFAULT_INDENT_MARKER) {
        return Some(DEFAULT_INDENT_MARKER);
    }

    let used_characters = prompt_text
        .chars()
        .filter(|character| {
            matches!(
                u32::from(*character),
                0xe000..=0xf8ff | 0x000f_0000..=0x000f_fffd | 0x0010_0000..=0x0010_fffd
            )
        })
        .collect::<HashSet<_>>();
    [
        0xe000..=0xf8ff,
        0x000f_0000..=0x000f_fffd,
        0x0010_0000..=0x0010_fffd,
    ]
    .into_iter()
    .flatten()
    .filter_map(char::from_u32)
    .find(|character| !used_characters.contains(character))
}

fn leading_indentation(line: &str) -> (usize, usize) {
    let mut content_start = 0;
    let mut indentation_width = 0;

    for (byte_index, character) in line.char_indices() {
        match character {
            ' ' => indentation_width += 1,
            '\t' => {
                indentation_width +=
                    USER_PROMPT_TAB_WIDTH - (indentation_width % USER_PROMPT_TAB_WIDTH);
            }
            _ => break,
        }
        content_start = byte_index + character.len_utf8();
    }

    (content_start, indentation_width)
}

fn restored_user_prompt_spans<'line>(
    rendered_line: &'line Line<'static>,
    indent_marker: Option<char>,
) -> impl Iterator<Item = ratatui::text::Span<'static>> + 'line {
    rendered_line.spans.iter().cloned().map(move |mut span| {
        if let Some(indent_marker) = indent_marker
            && span.content.contains(indent_marker)
        {
            span.content = span.content.replace(indent_marker, " ").into();
        }

        span
    })
}

fn append_markdown_lines(
    lines: &mut Vec<Line<'static>>,
    markdown: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) {
    let rendered_lines = rendered_markdown_lines(markdown, inner_width, markdown_render_cache);
    let Some(first_visible_line_index) = rendered_lines.iter().position(|line| line.width() > 0)
    else {
        return;
    };
    let last_visible_line_index = rendered_lines
        .iter()
        .rposition(|line| line.width() > 0)
        .unwrap_or(first_visible_line_index);

    append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
    lines.extend(
        rendered_lines[first_visible_line_index..=last_visible_line_index]
            .iter()
            .cloned(),
    );
}

fn rendered_markdown_lines(
    markdown: &str,
    inner_width: usize,
    markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
) -> Arc<[Line<'static>]> {
    match markdown_render_cache {
        Some(cache) => cache.render(markdown, inner_width),
        None => Arc::from(render_markdown(markdown, inner_width)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_section_display_text_handles_empty_and_markdown_sections() {
        // Arrange
        let empty_section = SessionOutputTranscriptSection::Empty;
        let markdown_section =
            SessionOutputTranscriptSection::Markdown("draft preview".to_string());

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
        let queued_messages = vec![" \n\t".to_string(), "queued reply".to_string()];

        // Act
        append_queued_messages(&mut lines, &queued_messages);

        // Assert
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["", "queued › queued reply", ""]
        );
    }

    #[test]
    fn test_blank_user_prompt_does_not_add_output_lines() {
        // Arrange
        let mut lines = Vec::new();

        // Act
        append_user_prompt(&mut lines, " \n\t", 80, None);

        // Assert
        assert!(lines.is_empty());
    }

    #[test]
    fn test_zero_width_user_prompt_does_not_add_output_lines() {
        // Arrange
        let mut lines = Vec::new();

        // Act
        append_user_prompt(&mut lines, "\u{200b}", 80, None);

        // Assert
        assert!(lines.is_empty());
    }

    #[test]
    fn test_zero_width_markdown_does_not_add_output_lines() {
        // Arrange
        let mut lines = Vec::new();

        // Act
        append_markdown_lines(&mut lines, "\u{200b}", 80, None);

        // Assert
        assert!(lines.is_empty());
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
        session.queued_messages = vec!["queued reply".to_string()];

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
}
