//! Presentation state for the `ChatGPT` plan sign-in popup opened from the
//! prompt composer.

use super::app_mode::PromptModeSnapshot;
use crate::domain::agent::AgentSelection;
use crate::domain::session::SessionId;

/// Popup title shown in every sign-in phase.
pub const CHATGPT_SIGN_IN_TITLE: &str = "Sign in with ChatGPT";
/// Spinner label shown while the browser sign-in runs.
pub const CHATGPT_SIGN_IN_LOADING_LABEL: &str = "Waiting for browser sign-in…";

/// Step of the sign-in popup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatGptSignInPhase {
    /// Explains the sign-in and waits for `Enter` or `Esc`.
    Confirm,
    /// Browser sign-in running in the background.
    Waiting {
        /// Request generation used to ignore results after cancellation.
        request_id: u64,
        /// Sign-in URL once the callback listener is ready.
        url: Option<String>,
    },
    /// Sign-in finished with a success or failure message.
    Finished {
        /// Outcome text shown above the `OK` action.
        message: String,
    },
}

/// Sign-in popup state, including the composer restored when it closes.
#[derive(Clone)]
pub struct ChatGptSignInDialog {
    /// Current popup step.
    pub phase: ChatGptSignInPhase,
    /// Prompt composer restored after the popup closes.
    pub restore: PromptModeSnapshot,
    /// Harness model the session switches to after a successful sign-in.
    pub selection: AgentSelection,
}

impl ChatGptSignInDialog {
    /// Creates the confirmation step for `selection`, restoring `restore`
    /// afterwards.
    pub fn new(selection: AgentSelection, restore: PromptModeSnapshot) -> Self {
        Self {
            phase: ChatGptSignInPhase::Confirm,
            restore,
            selection,
        }
    }

    /// Returns the session whose composer opened the popup.
    pub fn session_id(&self) -> &SessionId {
        &self.restore.session_id
    }

    /// Returns whether the browser sign-in is still running.
    pub fn is_loading(&self) -> bool {
        matches!(self.phase, ChatGptSignInPhase::Waiting { .. })
    }

    /// Returns the request generation of a running sign-in.
    pub fn waiting_request_id(&self) -> Option<u64> {
        match self.phase {
            ChatGptSignInPhase::Waiting { request_id, .. } => Some(request_id),
            ChatGptSignInPhase::Confirm | ChatGptSignInPhase::Finished { .. } => None,
        }
    }

    /// Returns the popup body for the current step.
    pub fn message(&self) -> String {
        match &self.phase {
            ChatGptSignInPhase::Confirm => format!(
                "Use your ChatGPT plan for `{}`. Agentty opens your browser to sign in.",
                self.selection.model().as_str()
            ),
            ChatGptSignInPhase::Waiting { url: None, .. } => {
                "Starting the sign-in…\n\nPress `Esc` to cancel.".to_string()
            }
            ChatGptSignInPhase::Waiting { url: Some(url), .. } => {
                format!("If your browser did not open, visit:\n\n`{url}`\n\nPress `Esc` to cancel.")
            }
            ChatGptSignInPhase::Finished { message } => message.clone(),
        }
    }

    /// Returns the action row label shown when the popup is not loading.
    pub fn action_label(&self) -> &'static str {
        match self.phase {
            ChatGptSignInPhase::Confirm => "Enter: Sign in · Esc: Cancel",
            ChatGptSignInPhase::Waiting { .. } | ChatGptSignInPhase::Finished { .. } => "OK",
        }
    }
}

#[cfg(test)]
#[path = "chatgpt_sign_in_test.rs"]
mod tests;
