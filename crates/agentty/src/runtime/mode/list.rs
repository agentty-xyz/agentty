use std::io;

use ag_tui_text::text_util::inline_text;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Tab};
use crate::domain::input::InputCommand;
use crate::domain::session::{Session, Status};
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, HelpContext};
use crate::presentation::help_action::{
    HelpAction, project_list_actions, session_list_actions, settings_actions,
};
use crate::presentation::setting::{SettingsAction, SettingsInput};
use crate::runtime::EventResult;
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;
use crate::runtime::mode::input_key;

/// Handles key input while the app is in list mode.
///
/// Pressing `q` opens a confirmation overlay instead of quitting immediately,
/// with `No` selected by default. Pressing `Enter` on the `Projects` tab
/// selects the active project and then moves focus to `Tab::Sessions`.
/// `c` opens a cancel confirmation overlay for running sessions, review
/// sessions, unstarted draft sessions, and draft orchestrators, and `Tab`
/// cycles tabs forward while `Shift+Tab` cycles backward.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> io::Result<EventResult> {
    if app.tabs.current() == Tab::Settings
        && (app
            .settings_presentation
            .is_launch_configuration_list_editor_open()
            || app.settings_presentation.is_selector_dropdown_open())
    {
        if let Some(input) = settings_input_for_key(key)
            && let Some(action) = app.settings_presentation.action_for_input(input)
        {
            apply_settings_action(app, action).await;
        }

        return Ok(EventResult::Continue);
    }

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::Confirmation {
                confirmation_intent: ConfirmationIntent::Quit,
                confirmation_message: "Quit agentty?".to_string(),
                confirmation_title: "Confirm Quit".to_string(),
                restore_view: None,
                session_id: None,
                selected_confirmation_index: DEFAULT_OPTION_INDEX,
            };

            return Ok(EventResult::Continue);
        }
        KeyCode::Tab => {
            app.next_tab();
            app.persist_current_tab().await;
        }
        KeyCode::BackTab => {
            app.previous_tab();
            app.persist_current_tab().await;
        }
        KeyCode::Char('a')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::NONE =>
        {
            open_session_creation_flow(app).await;
        }
        KeyCode::Char('p')
            if app.tabs.current() == Tab::Sessions && key.modifiers == KeyModifiers::NONE =>
        {
            app.mode = AppMode::ProjectSwitcher {
                selected_option_index: 0,
            };
        }
        KeyCode::Char('j') | KeyCode::Down => match app.tabs.current() {
            Tab::Projects => app.next_project(),
            Tab::Sessions => app.next(),
            Tab::Settings => apply_settings_action(app, SettingsAction::Next).await,
        },
        KeyCode::Char('k') | KeyCode::Up => match app.tabs.current() {
            Tab::Projects => app.previous_project(),
            Tab::Sessions => app.previous(),
            Tab::Settings => apply_settings_action(app, SettingsAction::Previous).await,
        },
        KeyCode::Enter => return handle_enter_key(app).await,
        KeyCode::Char('c') if app.tabs.current() == Tab::Sessions => {
            let selected_session = app.selected_session().and_then(|session| {
                session
                    .allows_cancel_action()
                    .then(|| (session.id.clone(), inline_text(session.display_title())))
            });
            if let Some((session_id, session_title)) = selected_session {
                let running_child_count = app.orchestration_running_child_count(&session_id).await;
                let confirmation_message =
                    cancel_confirmation_message(&session_title, running_child_count);
                app.mode = AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::CancelSession,
                    confirmation_message,
                    confirmation_title: "Confirm Cancel".to_string(),
                    restore_view: None,
                    session_id: Some(session_id),
                    selected_confirmation_index: DEFAULT_OPTION_INDEX,
                };
            }
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'s') => sync_list_context(app),
        KeyCode::Char('?') => {
            open_list_help_overlay(app);
        }
        _ => {}
    }

    Ok(EventResult::Continue)
}

/// Translates terminal-specific keys into frontend-neutral settings input.
fn settings_input_for_key(key: KeyEvent) -> Option<SettingsInput> {
    let input_command = input_key::command_for_key(key, input_key::InputCapabilities::SINGLE_LINE);

    match (key.code, input_command) {
        (KeyCode::Esc, _) => Some(SettingsInput::Cancel),
        (KeyCode::Enter, _) => Some(SettingsInput::Confirm),
        (KeyCode::Down, _) => Some(SettingsInput::MoveDown),
        (KeyCode::Up, _) => Some(SettingsInput::MoveUp),
        (KeyCode::Char(character), Some(InputCommand::Insert(_))) => {
            Some(SettingsInput::Character(character))
        }
        (_, Some(command)) => Some(SettingsInput::Edit(command)),
        (_, None) => None,
    }
}

fn cancel_confirmation_message(session_title: &str, running_child_count: usize) -> String {
    if running_child_count > 0 {
        return format!("Cancel orchestration and its {running_child_count} running children?");
    }

    format!("Cancel session \"{session_title}\"?")
}

/// Opens the session selector, preceded by an advisory when configured
/// pre-commit validation has no executable Git hook.
async fn open_session_creation_flow(app: &mut App) {
    if let Some(warning) = app.pre_commit_hook_warning().await {
        app.mode = AppMode::PreCommitHookWarning {
            message: format!(
                "{warning}\n\nPress Enter to continue to session options, or Esc to cancel."
            ),
        };

        return;
    }

    app.mode = AppMode::SessionCreation {
        selected_option_index: 0,
    };
}

/// Handles `Enter` in list mode and triggers the selected tab primary action.
///
/// On the sessions tab, any selected session can be opened in view mode.
async fn handle_enter_key(app: &mut App) -> io::Result<EventResult> {
    match app.tabs.current() {
        Tab::Projects => {
            if app.switch_selected_project().await.is_ok() {
                app.tabs.set(Tab::Sessions);
                app.persist_current_tab().await;
            }
        }
        Tab::Sessions => {
            if let Some(session_index) = app.sessions.selected_session_index() {
                let Some(session_id) = app
                    .sessions
                    .session_at(session_index)
                    .map(|session| session.id.clone())
                else {
                    return Ok(EventResult::Continue);
                };

                app.sessions
                    .load_session_detail_into_state(app.services.db(), session_id.as_str())
                    .await;
                app.restore_review_output(&session_id);

                let questions = app
                    .sessions
                    .session_at(session_index)
                    .filter(|session| session.status == Status::Question)
                    .map(|session| session.questions.clone());
                if let Some(questions) = questions {
                    app.enter_question_mode(session_id.as_str(), questions);
                } else if !app.restore_prompt_progress(session_id.as_str()).await {
                    app.mode = AppMode::View {
                        session_id,
                        scroll_offset: None,
                    };
                }
            }
        }
        Tab::Settings => {
            apply_settings_action(app, SettingsAction::Activate).await;
        }
    }

    Ok(EventResult::Continue)
}

/// Applies a semantic settings-screen action and persists any requested value
/// change through the narrow settings application service.
async fn apply_settings_action(app: &mut App, action: SettingsAction) {
    let operation = {
        let view = app.settings.view();

        app.settings_presentation.apply(&view, action)
    };

    if let Some(operation) = operation {
        app.settings.apply_operation(operation).await;
    }
}

/// Starts the sync action for the active project.
fn sync_list_context(app: &mut App) {
    app.start_sync_main();
}

/// Opens the help overlay with list-mode action availability projection.
fn open_list_help_overlay(app: &mut App) {
    let keybindings = list_keybindings(app);

    app.mode = AppMode::Help {
        context: HelpContext::List { keybindings },
        scroll_offset: 0,
    };
}

/// Projects current list-mode action availability into keybinding entries.
fn list_keybindings(app: &App) -> Vec<HelpAction> {
    if app.tabs.current() == Tab::Projects {
        return project_list_actions();
    }

    if app.tabs.current() == Tab::Settings {
        return settings_actions();
    }

    let is_sessions_tab = app.tabs.current() == Tab::Sessions;
    let selected_session = app.selected_session();
    let can_cancel_selected_session =
        is_sessions_tab && selected_session.is_some_and(Session::allows_cancel_action);
    let can_open_selected_session = is_sessions_tab
        && app
            .sessions
            .selected_session_index()
            .and_then(|selected_index| app.sessions.session_at(selected_index))
            .is_some();
    session_list_actions(can_cancel_selected_session, can_open_selected_session)
}

#[cfg(test)]
#[path = "list_test.rs"]
mod tests;
