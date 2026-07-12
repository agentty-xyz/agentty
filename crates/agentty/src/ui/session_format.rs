//! Session header, footer, and transcript display formatting.

use ag_protocol::AgentResponseSummary;
use ag_tui_text::text_util;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Borders;

use crate::app;
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::review;
use crate::domain::session::{
    COMMITTING_PROGRESS_LABEL, PublishedBranchSyncStatus, Session, Status,
};
use crate::presentation::help_action::{self, ViewHelpState};
use crate::ui::icon::Icon;
use crate::ui::{markdown, style};

const REVIEW_SUGGESTIONS_HEADER: &str = "### Suggestions";
const REVIEW_SUGGESTIONS_HEADER_WITH_HINT: &str =
    "### Suggestions (type \"/apply\" to verify and apply)";
const SESSION_OUTPUT_DEFAULT_SUMMARY_TEXT: &str = "No changes";

/// Formats the session title and metadata lines rendered above the output
/// panel.
///
/// When a linked review-request URL is available, the URL shares the metadata
/// row when the full row fits and otherwise wraps to the row directly above
/// the transcript border.
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
    let metadata_lines = session_header_metadata_lines(
        session,
        header_width,
        default_reasoning_level,
        wall_clock_unix_seconds,
    );

    let mut lines = Vec::with_capacity(1 + metadata_lines.len());
    lines.push(Line::from(title_spans));

    for metadata_text in metadata_lines {
        lines.push(Line::from(Span::styled(
            metadata_text,
            Style::default().fg(style::palette::text_muted()),
        )));
    }

    lines
}

/// Formats the size, timer, token usage, model, and reasoning row shown in
/// single-line metadata contexts without any chat-header-only URL suffix.
pub fn session_metadata_text(
    session: &Session,
    header_width: u16,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> String {
    let metadata =
        session_metadata_base_text(session, default_reasoning_level, wall_clock_unix_seconds);

    text_util::truncate_with_ellipsis(&metadata, usize::from(header_width))
}

/// Formats the chat header metadata rows, including the linked review-request
/// URL when one is available.
///
/// When a PR/MR URL is available, it is placed on the same row as the
/// left-side metadata when space allows; otherwise it is moved to the next
/// metadata row.
fn session_header_metadata_lines(
    session: &Session,
    header_width: u16,
    default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> Vec<String> {
    let metadata =
        session_metadata_base_text(session, default_reasoning_level, wall_clock_unix_seconds);
    let available_width = usize::from(header_width);

    let review_request_url = session
        .review_request
        .as_ref()
        .map(|request| request.summary.web_url.as_str())
        .filter(|url| !url.is_empty())
        .map(str::trim)
        .filter(|url| !url.is_empty());

    let Some(review_request_url) = review_request_url else {
        return vec![text_util::truncate_with_ellipsis(
            &metadata,
            available_width,
        )];
    };

    let metadata_width = metadata.chars().count();
    let url_width = review_request_url.chars().count();
    let separator_width = 2;
    let total_required_width = metadata_width
        .saturating_add(separator_width)
        .saturating_add(url_width);
    let mut metadata_lines = Vec::with_capacity(2);

    if total_required_width <= available_width {
        let separator = " ".repeat(
            available_width
                .saturating_sub(metadata_width)
                .saturating_sub(url_width),
        );
        metadata_lines.push(format!("{metadata}{separator}{review_request_url}"));

        return metadata_lines;
    }

    metadata_lines.push(text_util::truncate_with_ellipsis(
        &metadata,
        available_width,
    ));
    metadata_lines.push(text_util::truncate_with_ellipsis(
        review_request_url,
        available_width,
    ));

    metadata_lines
}

/// Builds the untruncated left-side metadata text shared by session header and
/// single-line metadata renderers.
fn session_metadata_base_text(
    session: &Session,
    _default_reasoning_level: ReasoningLevel,
    wall_clock_unix_seconds: i64,
) -> String {
    let added_lines = session.stats.added_lines;
    let deleted_lines = session.stats.deleted_lines;
    let timer = text_util::format_duration_compact(
        session.in_progress_duration_seconds(wall_clock_unix_seconds),
    );
    let reasoning_level = session.effective_reasoning_level();
    let input_tokens = text_util::format_token_count(session.stats.input_tokens);
    let output_tokens = text_util::format_token_count(session.stats.output_tokens);
    format!(
        "Size: {}  Lines: +{added_lines} / -{deleted_lines}  Timer: {timer}  Agent: {}  Model: \
         {}  Reasoning: {}  Tokens: {input_tokens}/{output_tokens}",
        session.size,
        session.agent.kind(),
        session.agent.model().as_str(),
        reasoning_level.as_str(),
    )
}

/// Builds the footer help line shown in session view mode.
pub(crate) fn session_view_footer_line(view_help_state: ViewHelpState) -> Line<'static> {
    crate::ui::help_format::footer_line(&help_action::view_footer_actions(view_help_state))
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
/// content while an auto-push is in progress.
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

/// Returns one rendered summary section or the shared empty placeholder.
fn summary_section_text(summary_text: &str) -> &str {
    let trimmed_summary = summary_text.trim();
    if trimmed_summary.is_empty() {
        return SESSION_OUTPUT_DEFAULT_SUMMARY_TEXT;
    }

    trimmed_summary
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{
        ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    };
    use crate::test_support::SessionFixtureBuilder;

    fn session_with_review_request(url: &str) -> Session {
        let mut session = SessionFixtureBuilder::new().build();
        session.review_request = Some(ReviewRequest {
            last_refreshed_at: 1,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "main".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Update workflow".to_string(),
                web_url: url.to_string(),
            },
        });

        session
    }

    #[test]
    fn test_session_header_lines_keeps_review_request_url_on_same_line_if_it_fits() {
        // Arrange
        let session = session_with_review_request("https://github.com/agentty-xyz/agentty/pull/42");
        let header_width = 180;

        // Act
        let header_lines =
            session_header_lines(&session, header_width, ReasoningLevel::default(), 0);
        let metadata_line = header_lines[1].to_string();

        // Assert
        assert_eq!(header_lines.len(), 2);
        assert_eq!(metadata_line.chars().count(), usize::from(header_width));
        assert!(metadata_line.contains("Tokens: 0/0"));
        assert!(metadata_line.ends_with("https://github.com/agentty-xyz/agentty/pull/42"));
    }

    #[test]
    fn test_session_header_lines_wraps_review_request_url_to_second_line_when_too_narrow() {
        // Arrange
        let session = session_with_review_request("https://example.test/pull/42");

        // Act
        let header_lines = session_header_lines(&session, 60, ReasoningLevel::default(), 0);
        let metadata_line = header_lines[1].to_string();
        let review_url_line = header_lines[2].to_string();

        // Assert
        assert_eq!(header_lines.len(), 3);
        assert!(metadata_line.contains("Size: XS"));
        assert!(review_url_line.starts_with("https://"));
        assert!(review_url_line.ends_with("https://example.test/pull/42"));
    }

    #[test]
    fn test_session_metadata_text_omits_review_request_url() {
        // Arrange
        let session = session_with_review_request("https://example.test/pull/42");

        // Act
        let metadata_text = session_metadata_text(&session, 160, ReasoningLevel::default(), 0);

        // Assert
        assert!(metadata_text.contains("Tokens: 0/0"));
        assert!(!metadata_text.contains("https://example.test/pull/42"));
    }

    #[test]
    fn test_session_metadata_text_prints_agent_before_model() {
        // Arrange
        let mut session = SessionFixtureBuilder::new().build();
        session.agent = crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            AgentModel::Gpt55,
        );

        // Act
        let metadata_text = session_metadata_text(&session, 160, ReasoningLevel::default(), 0);

        // Assert
        assert!(metadata_text.contains("Agent: codex  Model: gpt-5.5"));
    }
}
