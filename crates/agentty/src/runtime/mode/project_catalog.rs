//! Keyboard routing for explicit project catalog dialogs.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::App;
use crate::presentation::app_mode::AppMode;
use crate::presentation::project_catalog::ProjectCatalogDialog;
use crate::runtime::EventResult;
use crate::runtime::mode::{confirmation, input_key};

/// Applies user intent; only affirmative submission may access the filesystem.
pub(crate) async fn handle(app: &mut App, key: KeyEvent) -> EventResult {
    let AppMode::ProjectCatalog { dialog } = &mut app.mode else {
        return EventResult::Continue;
    };
    match dialog {
        ProjectCatalogDialog::SuggestHome { selected, .. } => {
            match confirmation::handle(selected, key) {
                confirmation::ConfirmationDecision::Confirm => {
                    app.dismiss_project_discovery_suggestion(true).await;
                }
                confirmation::ConfirmationDecision::Reject
                | confirmation::ConfirmationDecision::Cancel => {
                    app.dismiss_project_discovery_suggestion(false).await;
                }
                confirmation::ConfirmationDecision::Continue => {}
            }
        }
        ProjectCatalogDialog::PathInput { input, error, .. } => match key.code {
            KeyCode::Esc => app.close_project_catalog(),
            KeyCode::Enter => app.submit_project_path().await,
            _ => {
                if let Some(command) =
                    input_key::command_for_key(key, input_key::InputCapabilities::SINGLE_LINE)
                {
                    input.apply(command);
                    *error = None;
                }
            }
        },
        ProjectCatalogDialog::Scanning { .. } => {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                app.close_project_catalog();
            }
        }
        ProjectCatalogDialog::Results {
            paths, selected, ..
        } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.close_project_catalog(),
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(paths.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some((_, checked)) = paths.get_mut(*selected) {
                    *checked = !*checked;
                }
            }
            KeyCode::Enter => app.save_discovered_projects().await,
            _ => {}
        },
        ProjectCatalogDialog::Message { .. } => {
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
                app.close_project_catalog();
            }
        }
    }

    EventResult::Continue
}

#[cfg(test)]
#[path = "project_catalog_test.rs"]
mod tests;
