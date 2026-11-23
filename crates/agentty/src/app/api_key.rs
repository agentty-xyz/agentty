//! Harness provider API key entry started from the prompt composer's
//! `/model` picker: the key is saved through the availability probe, and the
//! session switches to the chosen model.

use crate::app::App;
use crate::app::prompt_intent::PromptModelChange;
use crate::domain::agent::{AgentSelection, HarnessModelStatus};
use crate::presentation::api_key::{ApiKeyDialog, ApiKeyPhase};
use crate::presentation::app_mode::{AppMode, PromptModeSnapshot};

impl App {
    /// Opens the API key popup for `selection` above the composer captured
    /// in `restore`, or restores the composer when the listed model no
    /// longer needs a key.
    pub(crate) fn open_api_key_input(
        &mut self,
        selection: AgentSelection,
        restore: PromptModeSnapshot,
    ) {
        let provider = self
            .services
            .harness_model_options()
            .into_iter()
            .find_map(|option| match option.status {
                HarnessModelStatus::ApiKeyRequired(provider)
                    if option.model == selection.model() =>
                {
                    Some((provider, option.provider))
                }
                _ => None,
            });
        self.mode = match provider {
            Some((provider, provider_name)) => AppMode::ApiKeyInput {
                dialog: Box::new(ApiKeyDialog::new(
                    selection,
                    provider,
                    provider_name,
                    restore,
                )),
            },
            None => restore.into_prompt_mode(),
        };
    }

    /// Saves the pasted key, then switches the session to the chosen model,
    /// marks the provider's models ready, and lists the harness models
    /// again. A failure keeps the popup open with the reason.
    pub(crate) async fn save_api_key(&mut self) {
        let AppMode::ApiKeyInput { dialog } = &self.mode else {
            return;
        };
        if !dialog.is_editing() {
            return;
        }
        let provider = dialog.provider;
        let selection = dialog.selection;
        let session_id = dialog.session_id().clone();
        let provider_name = dialog.provider_name.clone();
        let saved = self
            .services
            .harness_model_probe()
            .save_harness_api_key(provider, dialog.input.text())
            .await;
        let phase = match saved {
            Ok(()) => {
                self.publish_harness_models_ready(|status| {
                    *status == HarnessModelStatus::ApiKeyRequired(provider)
                });
                let model_change = self
                    .update_prompt_session_model(&session_id, selection)
                    .await;
                self.refresh_harness_models();
                let model = selection.model().as_str();
                let message = match model_change {
                    PromptModelChange::Switched => {
                        format!(
                            "Saved your {provider_name} API key. This session now uses `{model}`."
                        )
                    }
                    PromptModelChange::Deferred => format!(
                        "Saved your {provider_name} API key. This session switches to `{model}` \
                         after its current turn and queued messages finish."
                    ),
                    PromptModelChange::Failed => format!(
                        "Saved your {provider_name} API key, but switching this session to \
                         `{model}` failed. Pick it again with /model."
                    ),
                };

                ApiKeyPhase::Saved { message }
            }
            Err(error) => ApiKeyPhase::Editing { error: Some(error) },
        };
        self.finish_api_key_input(phase);
    }

    /// Shows `phase` as the save result while the popup is still open;
    /// switching the session model can close it.
    fn finish_api_key_input(&mut self, phase: ApiKeyPhase) {
        if let AppMode::ApiKeyInput { dialog } = &mut self.mode {
            dialog.finish(phase);
        }
    }

    /// Closes the popup and restores the composer that opened it.
    pub(crate) fn close_api_key_input(&mut self) {
        match std::mem::replace(&mut self.mode, AppMode::List) {
            AppMode::ApiKeyInput { dialog } => self.mode = dialog.restore.into_prompt_mode(),
            other_mode => self.mode = other_mode,
        }
    }
}

#[cfg(test)]
#[path = "api_key_test.rs"]
mod tests;
