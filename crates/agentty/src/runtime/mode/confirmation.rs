use crossterm::event::{KeyCode, KeyEvent};

/// Index of the `Yes` action in confirmation option navigation.
pub(crate) const YES_OPTION_INDEX: usize = 0;

/// Index of the `No` action in confirmation option navigation.
pub(crate) const NO_OPTION_INDEX: usize = 1;

/// Default selection for newly opened confirmation overlays.
pub(crate) const DEFAULT_OPTION_INDEX: usize = NO_OPTION_INDEX;

/// Describes how a confirmation selector should react to a pressed key.
pub(crate) enum ConfirmationDecision {
    Confirm,
    Reject,
    Cancel,
    Continue,
}

/// Handles shared confirmation keys (`y/n/q`, arrows, `h/l`, `Esc`, `Enter`)
/// for a yes/no confirmation selector.
pub(crate) fn handle(
    selected_confirmation_index: &mut usize,
    key: KeyEvent,
) -> ConfirmationDecision {
    match key.code {
        KeyCode::Char(character) if is_yes_shortcut(character) => ConfirmationDecision::Confirm,
        KeyCode::Char(character) if is_no_shortcut(character) => ConfirmationDecision::Reject,
        KeyCode::Esc | KeyCode::Char('q') => ConfirmationDecision::Cancel,
        KeyCode::Left => {
            *selected_confirmation_index = selected_confirmation_index.saturating_sub(1);

            ConfirmationDecision::Continue
        }
        KeyCode::Char(character) if is_left_shortcut(character) => {
            *selected_confirmation_index = selected_confirmation_index.saturating_sub(1);

            ConfirmationDecision::Continue
        }
        KeyCode::Right => {
            *selected_confirmation_index = (*selected_confirmation_index + 1).min(NO_OPTION_INDEX);

            ConfirmationDecision::Continue
        }
        KeyCode::Char(character) if is_right_shortcut(character) => {
            *selected_confirmation_index = (*selected_confirmation_index + 1).min(NO_OPTION_INDEX);

            ConfirmationDecision::Continue
        }
        KeyCode::Enter => {
            if *selected_confirmation_index == YES_OPTION_INDEX {
                ConfirmationDecision::Confirm
            } else {
                ConfirmationDecision::Reject
            }
        }
        _ => ConfirmationDecision::Continue,
    }
}

/// Returns whether the pressed key should confirm the action.
fn is_yes_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'y')
}

/// Returns whether the pressed key should cancel the action.
fn is_no_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'n')
}

/// Returns whether the pressed key should move selection to the left option.
fn is_left_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'h')
}

/// Returns whether the pressed key should move selection to the right option.
fn is_right_shortcut(character: char) -> bool {
    character.eq_ignore_ascii_case(&'l')
}

#[cfg(test)]
#[path = "confirmation_test.rs"]
mod tests;
