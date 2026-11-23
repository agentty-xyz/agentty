//! Presentation state for the popup that collects a harness provider's API
//! key, opened from the prompt composer's `/model` picker.

use super::app_mode::PromptModeSnapshot;
use crate::domain::agent::AgentSelection;
use crate::domain::input::InputState;
use crate::domain::session::SessionId;

/// Popup title shown in every step.
pub const API_KEY_TITLE: &str = "Paste API Key";
/// Character drawn in place of each pasted key character.
const MASK_CHARACTER: char = '•';

/// Step of the API key popup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiKeyPhase {
    /// Collects the key, showing the last save failure, if any.
    Editing {
        /// Failure of the last save, shown above the field.
        error: Option<String>,
    },
    /// The key was saved; shows the outcome above the `OK` action.
    Saved {
        /// Outcome text.
        message: String,
    },
}

/// API key popup state, including the composer restored when it closes.
#[derive(Clone)]
pub struct ApiKeyDialog {
    /// Pasted key, never shown unmasked.
    pub input: InputState,
    /// Current popup step.
    pub phase: ApiKeyPhase,
    /// Provider id the key is stored under, such as `kimi`.
    pub provider: &'static str,
    /// Provider display name, such as `Kimi`.
    pub provider_name: String,
    /// Prompt composer restored after the popup closes.
    pub restore: PromptModeSnapshot,
    /// Harness model the session switches to after the key is saved.
    pub selection: AgentSelection,
}

impl ApiKeyDialog {
    /// Creates the editing step for `selection`, whose provider stores its
    /// key under `provider`, restoring `restore` afterwards.
    pub fn new(
        selection: AgentSelection,
        provider: &'static str,
        provider_name: String,
        restore: PromptModeSnapshot,
    ) -> Self {
        Self {
            input: InputState::default(),
            phase: ApiKeyPhase::Editing { error: None },
            provider,
            provider_name,
            restore,
            selection,
        }
    }

    /// Returns the session whose composer opened the popup.
    pub fn session_id(&self) -> &SessionId {
        &self.restore.session_id
    }

    /// Returns the popup body for the current step.
    pub fn message(&self) -> String {
        match &self.phase {
            ApiKeyPhase::Editing { error } => {
                let prompt = format!(
                    "`{}` needs a {} API key. Agentty saves it to `auth/api_keys.json` in its \
                     data directory (`~/.agentty` by default), readable only by you.",
                    self.selection.model().as_str(),
                    self.provider_name
                );

                match error {
                    Some(error) => format!("{error}\n\n{prompt}"),
                    None => prompt,
                }
            }
            ApiKeyPhase::Saved { message } => message.clone(),
        }
    }

    /// Returns the pasted key with every character masked, so the cursor
    /// keeps its position.
    pub fn masked_input(&self) -> String {
        self.input.text().chars().map(|_| MASK_CHARACTER).collect()
    }

    /// Returns whether the popup still collects the key.
    pub fn is_editing(&self) -> bool {
        matches!(self.phase, ApiKeyPhase::Editing { .. })
    }

    /// Moves to `phase`, the result of saving the key, forgetting the
    /// pasted key once it is saved.
    pub fn finish(&mut self, phase: ApiKeyPhase) {
        if matches!(phase, ApiKeyPhase::Saved { .. }) {
            self.input = InputState::default();
        }
        self.phase = phase;
    }
}

#[cfg(test)]
#[path = "api_key_test.rs"]
mod tests;
