//! Session header, footer, and transcript display formatting.

use ag_tui_text::text_util;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Borders;

use crate::domain::resource::SessionResources;
use crate::domain::review;
use crate::domain::session::{COMMITTING_PROGRESS_LABEL, Session, Status};
use crate::presentation::frame_time::FrameTime;
use crate::presentation::help_action::{self, ViewHelpState};
use crate::ui::icon::Icon;
use crate::ui::{markdown, style};

const SECONDS_PER_MINUTE: i64 = 60;
const SECONDS_PER_HOUR: i64 = 3_600;
const SECONDS_PER_DAY: i64 = 86_400;
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

/// Formats active-work duration alongside the session age, omitting the age
/// when the combined label exceeds the available footer width.
pub(crate) fn session_timing_label(session: &Session, frame_time: FrameTime, width: u16) -> String {
    let timer = text_util::format_duration_compact(
        session.in_progress_duration_seconds(frame_time.unix_seconds()),
    );
    let started = session_started_label(session.created_at, frame_time);

    let label = format!("Timer: {timer} · {started}");
    if Line::raw(label.as_str()).width() <= usize::from(width) {
        label
    } else {
        format!("Timer: {timer}")
    }
}

/// Formats how long ago a session started, for example `Started 2d 4h ago`.
pub(crate) fn session_started_label(created_at: i64, frame_time: FrameTime) -> String {
    let age_seconds = frame_time.unix_seconds().saturating_sub(created_at);
    let age = if age_seconds < SECONDS_PER_MINUTE {
        "just now".to_string()
    } else {
        format!("{} ago", session_age_text(age_seconds))
    };

    format!("Started {age}")
}

/// Formats a session age with its two most significant units, such as
/// `12m`, `3h 5m`, or `2d 4h`.
fn session_age_text(age_seconds: i64) -> String {
    let minute_count = age_seconds / SECONDS_PER_MINUTE;
    let hour_count = age_seconds / SECONDS_PER_HOUR;
    let day_count = age_seconds / SECONDS_PER_DAY;

    if day_count > 0 {
        return format!("{day_count}d {}h", hour_count % 24);
    }

    if hour_count > 0 {
        return format!("{hour_count}h {}m", minute_count % 60);
    }

    format!("{minute_count}m")
}

/// Formats the size-prefixed title with right-aligned, color-coded change
/// totals, followed by any merge conflict and linked review-request URL.
pub fn session_header_lines(
    session: &Session,
    header_width: u16,
    has_merge_conflict: bool,
) -> Vec<Line<'static>> {
    let title_width = usize::from(header_width);
    let title_text = text_util::inline_text(session.display_title());
    let base_style = Style::default()
        .fg(style::status_color(session.status))
        .add_modifier(Modifier::BOLD);
    let mut title_spans = markdown::parse_inline_spans(&title_text, base_style);
    title_spans.insert(0, Span::styled(format!("[{}] ", session.size), base_style));
    let totals = vec![
        Span::styled("(", Style::default().fg(style::palette::warning())),
        Span::styled(
            format!("+{}", session.stats.added_lines),
            Style::default().fg(style::palette::success()),
        ),
        Span::raw(" "),
        Span::styled(
            format!("-{}", session.stats.deleted_lines),
            Style::default().fg(style::palette::danger()),
        ),
        Span::styled(")", Style::default().fg(style::palette::warning())),
    ];
    let totals_width: usize = totals.iter().map(Span::width).sum();
    let mut title_spans = text_util::truncate_spans_with_ellipsis(
        title_spans,
        title_width.saturating_sub(totals_width + 1),
    );
    let used_width: usize = title_spans.iter().map(Span::width).sum();
    title_spans.push(Span::raw(
        " ".repeat(title_width.saturating_sub(used_width + totals_width)),
    ));
    title_spans.extend(text_util::truncate_spans_with_ellipsis(totals, title_width));

    let mut lines = Vec::with_capacity(3);
    lines.push(Line::from(title_spans));

    if has_merge_conflict {
        lines.push(Line::from(Span::styled(
            format!("Merge conflict with {}", session.base_branch),
            Style::default()
                .fg(style::palette::danger())
                .add_modifier(Modifier::BOLD),
        )));
    }

    if let Some(url) = session
        .review_request
        .as_ref()
        .map(|request| request.summary.web_url.trim())
        .filter(|url| !url.is_empty())
    {
        lines.push(Line::styled(
            text_util::truncate_with_ellipsis(url, title_width),
            Style::default().fg(style::palette::text_muted()),
        ));
    }

    lines
}

/// Returns the display name of a session's response speed, or `None` when its
/// provider has no speed control to report.
///
/// Gemini and Antigravity expose no speed selection, so `/speed` is hidden for
/// them; surfacing a speed indicator in the composer would advertise a
/// setting those sessions cannot change.
pub(crate) fn session_speed_display(session: &Session) -> Option<&'static str> {
    if !session.agent.kind().supports_speed_mode() {
        return None;
    }

    Some(session.speed_mode.name())
}

/// Formats the response-style, response-speed, and permission indicators shown
/// in the prompt input title.
pub(crate) fn prompt_session_status(session: &Session) -> String {
    let speed_mode = session_speed_display(session);
    let permission_mode = session.permission_mode.display_label();
    let response_style = session.response_style.name();

    match speed_mode {
        None => format!("{response_style} · {permission_mode}"),
        Some(speed_mode) => {
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
/// extra gutter characters. The composer supplies the lower visual boundary.
pub fn session_output_panel_borders() -> Borders {
    Borders::TOP
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

/// Builds a single calm preview row for queued work, clipped to display width.
pub(crate) fn session_output_queued_lines(
    message: &str,
    first_line_prefix: &str,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let queued_style = Style::default()
        .fg(style::palette::text_subtle())
        .add_modifier(Modifier::ITALIC);
    let preview = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.is_empty() || inner_width == 0 {
        return Vec::new();
    }

    vec![Line::styled(
        text_util::truncate_with_ellipsis(
            &format!("{} {first_line_prefix}{preview}", Icon::QueuedAction),
            inner_width,
        ),
        queued_style,
    )]
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
