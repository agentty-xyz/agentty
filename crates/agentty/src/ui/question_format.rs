//! Question-mode text and footer formatting.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::state::app_mode::QuestionFocus;
use crate::ui::state::help_action;
use crate::ui::{style, text_util};

/// Returns wrapped question-panel lines with the correct focus styling.
pub(crate) fn question_panel_lines(
    question_title: &str,
    question: &str,
    is_chat_focused: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let title_color = if is_chat_focused {
        style::palette::text_muted()
    } else {
        style::palette::question()
    };
    let text_color = if is_chat_focused {
        style::palette::text_muted()
    } else {
        style::palette::warning()
    };
    let mut lines = vec![Line::from(Span::styled(
        question_title.to_string(),
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    ))];
    lines.extend(
        text_util::wrap_lines(question, usize::from(width.max(1)))
            .into_iter()
            .map(|line| Line::from(line.to_string()).style(Style::default().fg(text_color))),
    );

    lines
}

/// Returns wrapped and styled option rows for the question panel.
pub(crate) fn question_option_lines(
    options: &[String],
    selected_option_index: Option<usize>,
    dimmed: bool,
) -> Vec<Line<'static>> {
    let header_color = if dimmed {
        style::palette::text_muted()
    } else {
        style::palette::warning()
    };
    let mut lines = Vec::with_capacity(options.len() + 1);
    lines.push(Line::from(Span::styled(
        "Options:",
        Style::default().fg(header_color),
    )));

    for (option_index, option_text) in options.iter().enumerate() {
        let is_selected = selected_option_index == Some(option_index);
        let prefix = if is_selected { "▸ " } else { "  " };
        let label = format!("{prefix}{}. {option_text}", option_index + 1);
        let style = if dimmed {
            Style::default().fg(style::palette::text_muted())
        } else if is_selected {
            Style::default()
                .fg(style::palette::surface_overlay())
                .bg(style::palette::warning())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(style::palette::text())
        };

        lines.push(Line::from(Span::styled(label, style)));
    }

    lines
}

/// Builds the question-mode help footer line for the current focus target.
///
/// `is_navigating_options` mirrors the runtime predicate that treats plain `q`
/// as a navigation key while the user is moving through predefined options. The
/// `q: Sessions` hint is surfaced whenever that predicate is satisfied so the
/// shortcut stays discoverable in answer focus too, not only in chat focus.
/// `is_at_mention_open` mirrors the runtime predicate that routes `Esc` to the
/// at-mention dropdown dismissal, so the end-turn hint drops the `Esc` prefix
/// and a `Esc: cancel @` hint is surfaced while the dropdown is visible.
pub fn question_help_footer_line(
    focus: QuestionFocus,
    is_navigating_options: bool,
    is_at_mention_open: bool,
) -> Line<'static> {
    let is_chat_focused = focus == QuestionFocus::Chat;
    let mut help_actions = Vec::new();

    if is_chat_focused {
        help_actions.push(help_action::HelpAction::new("scroll", "j/k", "Scroll chat"));
        help_actions.push(help_action::HelpAction::new("diff", "d", "Diff"));
        help_actions.push(help_action::HelpAction::new(
            "answer",
            "Esc/Enter",
            "Answer",
        ));
    } else {
        help_actions.push(help_action::HelpAction::new("send", "Enter", "Submit"));
    }

    let focus_label = if is_chat_focused { "Answer" } else { "Chat" };
    help_actions.push(help_action::HelpAction::new("focus", "Tab", focus_label));

    if is_chat_focused || is_navigating_options {
        help_actions.push(help_action::HelpAction::new("sessions", "q", "Sessions"));
    }

    if !is_chat_focused {
        if is_at_mention_open {
            help_actions.push(help_action::HelpAction::new("cancel @", "Esc", "Cancel @"));
            help_actions.push(help_action::HelpAction::new(
                "end turn", "Ctrl+C", "End turn",
            ));
        } else {
            help_actions.push(help_action::HelpAction::new(
                "end turn",
                "Esc/Ctrl+C",
                "End turn",
            ));
        }
    }

    help_action::footer_line(&help_actions)
}
