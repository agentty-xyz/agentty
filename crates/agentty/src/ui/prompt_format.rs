//! Prompt footer and suggestion-list formatting.

use ratatui::text::Line;

use crate::domain::agent::AgentKind;
use crate::infra::file_index;
use crate::ui::component::chat_input::{SuggestionItem, SuggestionList};
use crate::ui::state::help_action;
use crate::ui::state::prompt::{
    PromptAtMentionState, PromptSlashState, PromptSuggestionList,
    build_prompt_slash_suggestion_list,
};

const AT_MENTION_DEFAULT_MAX_VISIBLE: usize = 10;
/// Footer shortcut label for prompt image paste.
///
/// Keeps the primary prompt image paste shortcut aligned with upstream Codex
/// CLI while the key handler also accepts shifted terminal paste chords.
const PROMPT_IMAGE_PASTE_SHORTCUT_LABEL: &str = "Ctrl+V/Alt+V";
const NEW_SESSION_PROMPT_FOOTER_ACTIONS: [help_action::HelpAction; 4] = [
    help_action::HelpAction::new("stage draft", "Enter", "Stage draft"),
    help_action::HelpAction::new("newline", "Alt+Enter", "Insert newline"),
    help_action::HelpAction::new(
        "paste image",
        PROMPT_IMAGE_PASTE_SHORTCUT_LABEL,
        "Paste image",
    ),
    help_action::HelpAction::new("cancel", "Esc", "Cancel prompt"),
];
const PROMPT_FOOTER_ACTIONS: [help_action::HelpAction; 4] = [
    help_action::HelpAction::new("submit", "Enter", "Submit prompt"),
    help_action::HelpAction::new("newline", "Alt+Enter", "Insert newline"),
    help_action::HelpAction::new(
        "paste image",
        PROMPT_IMAGE_PASTE_SHORTCUT_LABEL,
        "Paste image",
    ),
    help_action::HelpAction::new("cancel", "Esc", "Cancel prompt"),
];

/// Builds the prompt-mode footer help line shown below the composer.
pub fn prompt_footer_line(
    session: &crate::domain::session::Session,
    attachment_count: usize,
) -> Line<'static> {
    let mut footer_line = help_action::footer_line(prompt_footer_actions(session));

    if attachment_count > 0 {
        let suffix = if attachment_count == 1 { "" } else { "s" };
        footer_line.spans.push(help_action::footer_separator_span());
        footer_line
            .spans
            .push(help_action::footer_muted_span(format!(
                "{attachment_count} image{suffix} ready"
            )));
    }

    footer_line
}

/// Builds the prompt suggestion dropdown for the current input state.
///
/// Slash-command input renders the command/model suggestion set. Otherwise an
/// active `@` mention query renders the matching file lookup list. The
/// returned dropdown is already windowed so the highlighted row stays visible.
/// `allow_apply_command` must match runtime slash selection so the visible
/// rows and submitted command indexes stay synchronized.
pub(crate) fn prompt_suggestion_list(
    input: &crate::domain::input::InputState,
    slash_state: &PromptSlashState,
    at_mention_state: Option<&PromptAtMentionState>,
    agent_kind: AgentKind,
    allow_apply_command: bool,
) -> Option<SuggestionList> {
    let input_text = input.text();
    let cursor = input.cursor;

    if input_text.starts_with('/') {
        return build_prompt_slash_suggestion_list(
            input_text,
            slash_state,
            agent_kind,
            allow_apply_command,
        )
        .map(render_prompt_suggestion_list);
    }

    at_mention_state.and_then(|state| {
        file_lookup_suggestion_list(input_text, cursor, state, AT_MENTION_DEFAULT_MAX_VISIBLE)
    })
}

/// Converts prompt-suggestion state into chat-input dropdown rows.
pub(crate) fn render_prompt_suggestion_list(
    suggestion_list: PromptSuggestionList,
) -> SuggestionList {
    SuggestionList {
        items: suggestion_list
            .items
            .into_iter()
            .map(|item| SuggestionItem {
                badge: item.badge,
                detail: item.detail,
                label: item.label,
                metadata: item.metadata,
            })
            .collect(),
        selected_index: suggestion_list.selected_index,
        title: suggestion_list.title,
    }
}

/// Builds one file-lookup suggestion list for an active `@` mention query.
///
/// Directory entries keep a trailing slash and visible rows are clamped to
/// `max_visible` while preserving the selected item inside the visible window.
pub(crate) fn file_lookup_suggestion_list(
    input_text: &str,
    cursor: usize,
    at_mention_state: &PromptAtMentionState,
    max_visible: usize,
) -> Option<SuggestionList> {
    let (_, query) = crate::domain::input::extract_at_mention_query(input_text, cursor)?;
    let filtered = file_index::filter_entries(&at_mention_state.all_entries, &query);

    if filtered.is_empty() {
        return None;
    }

    let window_start = at_mention_state
        .selected_index
        .saturating_sub(max_visible / 2);
    let window_end = filtered.len().min(window_start + max_visible);
    let window_start = window_end.saturating_sub(max_visible);

    let items = filtered[window_start..window_end]
        .iter()
        .map(|entry| {
            let label = if entry.is_dir {
                format!("{}/", entry.path)
            } else {
                entry.path.clone()
            };

            SuggestionItem {
                badge: None,
                detail: entry.is_dir.then(|| "folder".to_string()),
                label,
                metadata: None,
            }
        })
        .collect();

    let selected_index = at_mention_state
        .selected_index
        .min(filtered.len().saturating_sub(1))
        .saturating_sub(window_start);

    Some(SuggestionList {
        items,
        selected_index,
        title: "Files (↑↓ move, Enter select, Esc dismiss)".to_string(),
    })
}

/// Returns the number of dropdown rows shown above the prompt input.
///
/// The visible row count includes the dropdown border chrome so callers can
/// reserve the same height used by the rendered widget.
pub(crate) fn prompt_suggestion_dropdown_rows(suggestion_list: Option<&SuggestionList>) -> usize {
    suggestion_list.map_or(0, |list| list.items.len().saturating_add(2))
}

/// Returns the fixed prompt-mode actions rendered in the composer footer.
fn prompt_footer_actions(
    session: &crate::domain::session::Session,
) -> &'static [help_action::HelpAction] {
    if session.status == crate::domain::session::Status::Draft && session.is_draft_session() {
        return &NEW_SESSION_PROMPT_FOOTER_ACTIONS;
    }

    &PROMPT_FOOTER_ACTIONS
}
