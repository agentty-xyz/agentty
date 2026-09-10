//! Session header, footer, and transcript display formatting.

use ag_tui_text::text_util;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Borders;

use crate::domain::agent::ReasoningLevel;
use crate::domain::resource::SessionResources;
use crate::domain::review;
use crate::domain::session::{COMMITTING_PROGRESS_LABEL, Session, SessionId, Status};
use crate::presentation::help_action::{self, ViewHelpState};
use crate::ui::icon::Icon;
use crate::ui::{markdown, style};

const REVIEW_PROJECT_IMPACT_HEADER: &str = "### Project Impact";
const REVIEW_SUGGESTIONS_HEADER: &str = "### Suggestions";
const REVIEW_SUGGESTIONS_HEADER_WITH_HINT: &str =
    "### Suggestions (type \"/apply\" to verify and apply)";

/// Formats the bounded chat row for the tracked agent process tree.
pub(crate) fn session_resources_line(
    resources: Option<SessionResources>,
    host_cpu_temperature_celsius: Option<f32>,
    width: u16,
) -> Line<'static> {
    let mut text = resources.map_or_else(
        || "Processes: --  CPU: --  Memory: --".to_string(),
        |resources| {
            let memory = resources.resident_memory_kib;

            format!(
                "Processes: {}  CPU: {:.1}%  Memory: {}.{} MiB",
                resources.process_count,
                resources.cpu_percent,
                memory / 1024,
                memory % 1024 * 10 / 1024,
            )
        },
    );

    let temperature = host_cpu_temperature_celsius.map_or_else(
        || "--".to_string(),
        |temperature| format!("{temperature:.1}°C"),
    );
    text.push_str("  Host CPU temp: ");
    text.push_str(&temperature);

    Line::styled(
        text_util::truncate_with_ellipsis(&text, usize::from(width)),
        Style::default().fg(style::palette::text_muted()),
    )
}

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
    has_merge_conflict: bool,
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

    if has_merge_conflict {
        lines.push(Line::from(Span::styled(
            format!("Merge conflict with {}", session.base_branch),
            Style::default()
                .fg(style::palette::danger())
                .add_modifier(Modifier::BOLD),
        )));
    }

    if session.is_managed() {
        let controller = session
            .controller_session_id
            .as_ref()
            .map_or("orchestrator", SessionId::as_str);
        lines.push(Line::from(Span::styled(
            format!("Managed by {controller} — actions restricted"),
            Style::default().fg(style::palette::warning()),
        )));
    }

    for metadata_text in metadata_lines {
        lines.push(Line::from(Span::styled(
            metadata_text,
            Style::default().fg(style::palette::text_muted()),
        )));
    }

    lines
}

/// Formats the size, timer, model, reasoning, speed, and token-usage row shown
/// in single-line metadata contexts without any chat-header-only URL suffix.
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
    let speed = session_speed_display(session)
        .map(|speed_mode| format!("  Speed: {speed_mode}"))
        .unwrap_or_default();
    let response_style = session_response_style_display(session)
        .map(|response_style| format!("  Style: {response_style}"))
        .unwrap_or_default();
    format!(
        "Size: {}  Lines: +{added_lines} / -{deleted_lines}  Timer: {timer}  Agent: {}  Model: \
         {}  Reasoning: {}{speed}{response_style}  Tokens: {input_tokens}/{output_tokens}",
        session.size,
        session.agent.kind(),
        session.agent.model().as_str(),
        reasoning_level.as_str(),
    )
}

/// Returns the display name of a session's response speed, or `None` when its
/// provider has no speed control to report.
///
/// Gemini and Antigravity expose no speed selection, so `/speed` is hidden for
/// them; surfacing a `Speed:` field anyway would advertise a setting those
/// sessions cannot change.
pub(crate) fn session_speed_display(session: &Session) -> Option<&'static str> {
    if !session.agent.kind().supports_speed_mode() {
        return None;
    }

    Some(session.speed_mode.name())
}

/// Returns a non-default response-style label for compact status surfaces.
fn session_response_style_display(session: &Session) -> Option<&'static str> {
    (session.response_style != crate::domain::agent::ResponseStyle::Balanced)
        .then(|| session.response_style.name())
}

/// Formats the response-style, response-speed, and permission indicators shown
/// in the prompt input title.
pub(crate) fn prompt_session_status(session: &Session) -> String {
    let speed_mode = session_speed_display(session);
    let permission_mode = session.permission_mode.display_label();
    let response_style = session_response_style_display(session);

    match (response_style, speed_mode) {
        (None, None) => permission_mode.to_string(),
        (None, Some(speed_mode)) => format!("{speed_mode} · {permission_mode}"),
        (Some(response_style), None) => format!("{response_style} · {permission_mode}"),
        (Some(response_style), Some(speed_mode)) => {
            format!("{response_style} · {speed_mode} · {permission_mode}")
        }
    }
}

/// Builds the compact session-view footer.
pub(crate) fn session_view_footer_line(view_help_state: ViewHelpState) -> Line<'static> {
    crate::ui::help_format::footer_line(&help_action::view_footer_actions(view_help_state))
}

/// Formats focused-review section headings with compact spacing and adds the
/// verification-gated `/apply` hint when suggestions are actionable.
pub(crate) fn format_review_markdown(review_markdown: &str) -> String {
    let has_actionable_suggestions =
        review::has_actionable_review_suggestions(Some(review_markdown));
    let mut formatted_lines = Vec::with_capacity(review_markdown.lines().count());
    let mut skip_section_spacing = false;

    for line in review_markdown.lines() {
        if skip_section_spacing && line.trim().is_empty() {
            continue;
        }
        skip_section_spacing = false;

        let trimmed_line = line.trim_end();
        if trimmed_line == REVIEW_PROJECT_IMPACT_HEADER {
            formatted_lines.push(line.to_string());
            skip_section_spacing = true;
        } else if matches!(
            trimmed_line,
            REVIEW_SUGGESTIONS_HEADER | REVIEW_SUGGESTIONS_HEADER_WITH_HINT
        ) {
            if has_actionable_suggestions {
                formatted_lines.push(REVIEW_SUGGESTIONS_HEADER_WITH_HINT.to_string());
            } else {
                formatted_lines.push(line.to_string());
            }
            skip_section_spacing = true;
        } else {
            formatted_lines.push(line.to_string());
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

/// Returns whether the session-output status row receives a Tachyon loader
/// effect.
pub(crate) fn session_output_uses_tachyon_loader(status: Status) -> bool {
    matches!(
        status,
        Status::InProgress
            | Status::AgentReview
            | Status::Rebasing
            | Status::Merging
            | Status::Merged
    )
}

/// Builds the inline shortcut hint for continuing a completed session.
pub fn session_output_done_line() -> Line<'static> {
    Line::from(vec![Span::styled(
        "Press 'c' to continue in a new session.",
        Style::default().fg(style::palette::text_subtle()),
    )])
}

/// Builds the active-status lines shown at the end of an in-flight session
/// transcript.
///
/// The leading glyph is stable text because the session output component
/// applies the Tachyonfx loader animation directly to those buffer cells after
/// the paragraph is rendered.
pub fn session_output_status_lines(
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
    review_comment_resolution_message: Option<&str>,
) -> Vec<Line<'static>> {
    if !matches!(
        status,
        Status::InProgress
            | Status::AgentReview
            | Status::Queued
            | Status::Rebasing
            | Status::Merging
            | Status::Merged
    ) {
        return Vec::new();
    }

    let status_message = session_output_status_message(
        status,
        active_progress,
        review_status_message,
        review_comment_resolution_message,
    );

    let mut source_lines = status_message.trim().lines();
    let heading = source_lines.next().unwrap_or_default().trim();
    let mut lines = vec![Line::from(Span::styled(
        format!("{} {heading}", session_output_status_icon(status)),
        Style::default().fg(style::status_color(status)),
    ))];
    lines.extend(
        source_lines
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
            .map(|detail| {
                Line::from(Span::styled(
                    format!("    {detail}"),
                    Style::default().fg(style::palette::text_muted()),
                ))
            }),
    );

    lines
}

/// Builds an animated loading header followed by any indented detail rows.
pub(crate) fn session_output_transient_loading_lines(message: &str) -> Vec<Line<'static>> {
    let warning_style = Style::default().fg(style::palette::warning());
    let mut source_lines = message.trim().lines();
    let header = source_lines.next().unwrap_or_default().trim();
    let mut lines = vec![Line::from(vec![Span::styled(
        format!("{} {header}", Icon::Spinner),
        warning_style,
    )])];
    lines.extend(
        source_lines
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
            .map(|detail| Line::from(Span::styled(format!("    {detail}"), warning_style))),
    );

    lines
}

/// Builds calm queued-work rows with one distinct indicator on the first row.
pub(crate) fn session_output_queued_lines(
    message: &str,
    first_line_prefix: &str,
) -> Vec<Line<'static>> {
    let queued_style = Style::default()
        .fg(style::palette::text_subtle())
        .add_modifier(Modifier::ITALIC);
    let message_lines = message.trim().lines().collect::<Vec<_>>();
    let Some(first_content_line_index) = message_lines
        .iter()
        .position(|message_line| !message_line.trim().is_empty())
    else {
        return Vec::new();
    };
    let last_content_line_index = message_lines
        .iter()
        .rposition(|message_line| !message_line.trim().is_empty())
        .unwrap_or(first_content_line_index);
    let continuation_indent = " ".repeat(2 + first_line_prefix.chars().count());

    message_lines[first_content_line_index..=last_content_line_index]
        .iter()
        .enumerate()
        .map(|(line_index, message_line)| {
            let rendered_text = if line_index == 0 {
                format!("{} {first_line_prefix}{message_line}", Icon::QueuedAction)
            } else {
                format!("{continuation_indent}{message_line}")
            };

            Line::styled(rendered_text, queued_style)
        })
        .collect()
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
    review_comment_resolution_message: Option<&str>,
) -> String {
    match status {
        Status::InProgress => review_comment_resolution_message
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map_or_else(
                || {
                    active_progress
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
                        )
                },
                ToString::to_string,
            ),
        Status::AgentReview => review_status_message
            .map(str::trim)
            .filter(|status_message| !status_message.is_empty())
            .unwrap_or("Reviewing changes...")
            .to_string(),
        Status::Queued => "Waiting in merge queue...".to_string(),
        Status::Rebasing => "Rebasing...".to_string(),
        Status::Merging => "Merging...".to_string(),
        Status::Merged => "Waiting for manual local target sync...".to_string(),
        Status::Draft | Status::Review | Status::Question | Status::Done | Status::Canceled => {
            String::new()
        }
    }
}

/// Returns the status indicator icon used for inline session-output messages.
fn session_output_status_icon(status: Status) -> Icon {
    match status {
        Status::InProgress
        | Status::AgentReview
        | Status::Rebasing
        | Status::Merging
        | Status::Merged => Icon::TachyonLoader,
        Status::Queued
        | Status::Draft
        | Status::Review
        | Status::Question
        | Status::Done
        | Status::Canceled => Icon::Pending,
    }
}

#[cfg(test)]
#[path = "session_format_test.rs"]
mod tests;
