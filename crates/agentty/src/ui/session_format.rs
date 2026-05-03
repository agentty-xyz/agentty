//! Session header, footer, and transcript display formatting.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Borders;

use crate::app;
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::review;
use crate::domain::session::{
    COMMITTING_PROGRESS_LABEL, PublishedBranchSyncStatus, Session, Status,
};
use crate::icon::Icon;
use crate::infra::agent::protocol::AgentResponseSummary;
use crate::ui::state::help_action::{self, ViewHelpState};
use crate::ui::{markdown, style, text_util};

const CLARIFICATION_HEADER_LINE: &str = " › Clarifications:";
const REVIEW_SUGGESTIONS_HEADER: &str = "### Suggestions";
const REVIEW_SUGGESTIONS_HEADER_WITH_HINT: &str =
    "### Suggestions (type \"/apply\" to verify and apply)";
const SESSION_OUTPUT_DEFAULT_SUMMARY_TEXT: &str = "No changes";
const USER_PROMPT_PREFIX: &str = " › ";
const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";

/// Formats the session title and metadata lines rendered above the output
/// panel.
pub fn session_header_lines(
    session: &Session,
    header_width: u16,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> Vec<Line<'static>> {
    let title_width = usize::from(header_width);
    let title_text = text_util::inline_text(session.display_title());
    let base_style = Style::default()
        .fg(style::status_color(session.status))
        .add_modifier(Modifier::BOLD);
    let title_spans = markdown::parse_inline_spans(&title_text, base_style);
    let title_spans = text_util::truncate_spans_with_ellipsis(title_spans, title_width);
    let metadata_text = session_metadata_text(
        session,
        header_width,
        default_reasoning_level,
        wall_clock_unix_seconds,
    );

    vec![
        Line::from(title_spans),
        Line::from(Span::styled(
            metadata_text,
            Style::default().fg(style::palette::text_muted()),
        )),
    ]
}

/// Formats the size, timer, token usage, model, and reasoning row shown in
/// the session header.
pub fn session_metadata_text(
    session: &Session,
    header_width: u16,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> String {
    let added_lines = session.stats.added_lines;
    let deleted_lines = session.stats.deleted_lines;
    let timer = text_util::format_duration_compact(
        session.in_progress_duration_seconds(wall_clock_unix_seconds),
    );
    let reasoning_level = session.effective_reasoning_level(default_reasoning_level);
    let input_tokens = text_util::format_token_count(session.stats.input_tokens);
    let output_tokens = text_util::format_token_count(session.stats.output_tokens);
    let metadata = format!(
        "Size: {}  Lines: +{added_lines} / -{deleted_lines}  Timer: {timer}  Model: {}  \
         Reasoning: {}  Tokens: {input_tokens}/{output_tokens}",
        session.size,
        session.model.as_str(),
        reasoning_level.as_str(),
    );
    let metadata_width = usize::from(header_width);

    text_util::truncate_with_ellipsis(&metadata, metadata_width)
}

/// Builds the footer help line shown in session view mode.
pub fn session_view_footer_line(session: &Session, can_open_worktree: bool) -> Line<'static> {
    help_action::footer_line(&session_view_footer_actions(session, can_open_worktree))
}

/// Renders persisted summary payloads into display markdown.
///
/// Structured JSON summaries are expanded into `Current Turn` and `Session
/// Changes` sections with a blank line after the section headers. Plain text
/// falls back unchanged, and empty content uses the shared `No changes`
/// placeholder for both sections.
pub(crate) fn session_output_summary_markdown(summary_text: &str) -> String {
    let trimmed_summary = summary_text.trim();
    if let Ok(summary_payload) = serde_json::from_str::<AgentResponseSummary>(trimmed_summary) {
        return format!(
            "## Change Summary\n\n### Current Turn\n{}\n\n### Session Changes\n{}",
            summary_section_text(&summary_payload.turn),
            summary_section_text(&summary_payload.session)
        );
    }

    if !trimmed_summary.is_empty() {
        return trimmed_summary.to_string();
    }

    format!(
        "## Change Summary\n\n### Current Turn\n{SESSION_OUTPUT_DEFAULT_SUMMARY_TEXT}\n\n### \
         Session Changes\n{SESSION_OUTPUT_DEFAULT_SUMMARY_TEXT}"
    )
}

/// Adds the verification-gated `/apply` hint to an actionable focused-review
/// suggestions header.
pub(crate) fn annotate_review_suggestions_header(review_markdown: &str) -> String {
    if !review::has_actionable_review_suggestions(Some(review_markdown)) {
        return review_markdown.to_string();
    }

    let mut annotated_lines = Vec::with_capacity(review_markdown.lines().count());
    for line in review_markdown.lines() {
        if line.trim_end() == REVIEW_SUGGESTIONS_HEADER {
            annotated_lines.push(REVIEW_SUGGESTIONS_HEADER_WITH_HINT.to_string());
        } else {
            annotated_lines.push(line.to_string());
        }
    }

    annotated_lines.join("\n")
}

/// Adds visual spacing around user prompt blocks inside session output text.
///
/// Clarification blocks receive extra blank rows between numbered follow-up
/// questions so grouped answers remain easy to scan.
pub(crate) fn session_output_text_with_spaced_user_input(output_text: &str) -> String {
    let raw_lines = output_text.split('\n').collect::<Vec<_>>();
    let mut formatted_lines = Vec::with_capacity(raw_lines.len());
    let mut line_index = 0;

    while line_index < raw_lines.len() {
        let line = raw_lines[line_index];
        if !line.starts_with(USER_PROMPT_PREFIX) {
            formatted_lines.push(line.to_string());
            line_index += 1;

            continue;
        }

        if formatted_lines
            .last()
            .is_some_and(|item: &String| !item.is_empty())
        {
            formatted_lines.push(String::new());
        }

        let block_end_index = user_prompt_block_end_index(&raw_lines, line_index);
        formatted_lines.extend(format_prompt_block_lines(
            &raw_lines[line_index..=block_end_index],
        ));
        line_index = block_end_index + 1;

        let next_line_is_empty = raw_lines
            .get(line_index)
            .is_none_or(|next_line| next_line.is_empty());
        if !next_line_is_empty {
            formatted_lines.push(String::new());
        }
    }

    formatted_lines.join("\n")
}

/// Returns borders used for the session output panel.
///
/// Vertical borders stay hidden so terminal copy/select flows do not pick up
/// extra gutter characters.
pub fn session_output_panel_borders() -> Borders {
    Borders::TOP | Borders::BOTTOM
}

/// Returns the border style used for the session output panel.
pub fn session_output_panel_border_style(status: Status) -> Style {
    Style::default().fg(style::status_color(status))
}

/// Builds the inline shortcut hint for continuing a completed session.
pub fn session_output_done_line() -> Line<'static> {
    Line::from(vec![Span::styled(
        "Press 'c' to continue in a new session.",
        Style::default().fg(style::palette::text_subtle()),
    )])
}

/// Builds the active-status line shown at the end of an in-flight session
/// transcript.
///
/// The leading glyph is stable text because the session output component
/// applies the Tachyonfx loader animation directly to those buffer cells after
/// the paragraph is rendered.
pub fn session_output_status_line(
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
    review_model: AgentModel,
) -> Option<Line<'static>> {
    if !matches!(
        status,
        Status::InProgress
            | Status::AgentReview
            | Status::Queued
            | Status::Rebasing
            | Status::Merging
    ) {
        return None;
    }

    let status_message =
        session_output_status_message(status, active_progress, review_status_message, review_model);

    Some(Line::from(vec![Span::styled(
        format!("{} {status_message}", session_output_status_icon(status)),
        Style::default().fg(style::status_color(status)),
    )]))
}

/// Builds the published-branch sync status line appended after transcript
/// content when auto-push state is available.
pub fn session_output_published_branch_sync_line(session: &Session) -> Option<Line<'static>> {
    let sync_message = session.published_branch_sync_message()?;

    Some(Line::from(vec![Span::styled(
        format!(
            "{} {sync_message}",
            session_output_published_branch_sync_icon(session.published_branch_sync_status)
        ),
        Style::default().fg(session_output_published_branch_sync_color(
            session.published_branch_sync_status,
        )),
    )]))
}

/// Returns the footer action list for one session view state.
fn session_view_footer_actions(
    session: &Session,
    can_open_worktree: bool,
) -> Vec<help_action::HelpAction> {
    let session_state = help_action::session_view_state(session);
    help_action::view_footer_actions(ViewHelpState {
        can_open_worktree,
        publish_pull_request_action: session.publish_pull_request_action(),
        session_state,
    })
}

/// Returns one rendered summary section or the shared empty placeholder.
fn summary_section_text(summary_text: &str) -> &str {
    let trimmed_summary = summary_text.trim();
    if trimmed_summary.is_empty() {
        return SESSION_OUTPUT_DEFAULT_SUMMARY_TEXT;
    }

    trimmed_summary
}

/// Formats one prompt block and adds blank separators between clarification
/// question groups.
fn format_prompt_block_lines(raw_block_lines: &[&str]) -> Vec<String> {
    if !is_clarification_prompt_block(raw_block_lines) {
        return raw_block_lines.iter().map(ToString::to_string).collect();
    }

    let mut formatted_block_lines = Vec::with_capacity(raw_block_lines.len() + 2);
    for (block_line_index, raw_block_line) in raw_block_lines.iter().enumerate() {
        if block_line_index > 0 && is_clarification_question_line(raw_block_line) {
            formatted_block_lines.push(USER_PROMPT_CONTINUATION_PREFIX.to_string());
        }

        formatted_block_lines.push((*raw_block_line).to_string());
    }

    formatted_block_lines
}

/// Returns true when a prompt block is the generated clarifications payload.
fn is_clarification_prompt_block(raw_block_lines: &[&str]) -> bool {
    raw_block_lines
        .first()
        .is_some_and(|line| line.trim_end() == CLARIFICATION_HEADER_LINE)
}

/// Returns true for numbered clarification question rows like
/// `   1. Q: Need tests?`.
fn is_clarification_question_line(raw_block_line: &str) -> bool {
    let Some(line_without_prefix) = raw_block_line.strip_prefix(USER_PROMPT_CONTINUATION_PREFIX)
    else {
        return false;
    };
    let trimmed_line = line_without_prefix.trim_start();
    let digit_count = trimmed_line
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    if digit_count == 0 {
        return false;
    }

    let (_, suffix) = trimmed_line.split_at(digit_count);

    suffix.starts_with(". Q: ")
}

/// Returns the final non-empty line index for one prompt block.
fn user_prompt_block_end_index(raw_lines: &[&str], start_index: usize) -> usize {
    let mut candidate_index = start_index + 1;

    while candidate_index < raw_lines.len() {
        let candidate_line = raw_lines[candidate_index];
        if candidate_line.is_empty() || candidate_line.starts_with(USER_PROMPT_PREFIX) {
            break;
        }

        candidate_index += 1;
    }

    if raw_lines
        .get(candidate_index)
        .is_some_and(|candidate_line| candidate_line.is_empty())
    {
        return candidate_index.saturating_sub(1).max(start_index);
    }

    start_index
}

/// Returns the loader label for active session states.
///
/// Most in-progress details are agent thinking snippets appended to the
/// generic working label. Post-turn auto-commit sends a complete loader label
/// so commit-message generation and git commit work render as committing.
fn session_output_status_message(
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
    review_model: AgentModel,
) -> String {
    match status {
        Status::InProgress => active_progress
            .map(str::trim)
            .filter(|progress| !progress.is_empty())
            .map_or_else(
                || "Working...".to_string(),
                |progress| {
                    if progress == COMMITTING_PROGRESS_LABEL {
                        progress.to_string()
                    } else {
                        format!("Working... {progress}")
                    }
                },
            ),
        Status::AgentReview => review_status_message
            .map(str::trim)
            .filter(|status_message| !status_message.is_empty())
            .map_or_else(
                || app::review_loading_message(review_model),
                ToString::to_string,
            ),
        Status::Queued => "Waiting in merge queue...".to_string(),
        Status::Rebasing => "Rebasing...".to_string(),
        Status::Merging => "Merging...".to_string(),
        Status::Draft | Status::Review | Status::Question | Status::Done | Status::Canceled => {
            String::new()
        }
    }
}

/// Returns the status indicator icon used for inline session-output messages.
fn session_output_status_icon(status: Status) -> Icon {
    match status {
        Status::InProgress | Status::AgentReview | Status::Rebasing | Status::Merging => {
            Icon::TachyonLoader
        }
        Status::Queued
        | Status::Draft
        | Status::Review
        | Status::Question
        | Status::Done
        | Status::Canceled => Icon::Pending,
    }
}

/// Returns the icon used for published-branch sync status lines.
fn session_output_published_branch_sync_icon(sync_status: PublishedBranchSyncStatus) -> Icon {
    match sync_status {
        PublishedBranchSyncStatus::Idle => Icon::Pending,
        PublishedBranchSyncStatus::InProgress => Icon::Spinner,
        PublishedBranchSyncStatus::Succeeded => Icon::Check,
        PublishedBranchSyncStatus::Failed => Icon::Warn,
    }
}

/// Returns the color used for published-branch sync status lines.
fn session_output_published_branch_sync_color(
    sync_status: PublishedBranchSyncStatus,
) -> ratatui::style::Color {
    match sync_status {
        PublishedBranchSyncStatus::Idle => style::palette::text_muted(),
        PublishedBranchSyncStatus::InProgress | PublishedBranchSyncStatus::Failed => {
            style::palette::warning()
        }
        PublishedBranchSyncStatus::Succeeded => style::palette::success(),
    }
}
