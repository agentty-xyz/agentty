//! `ChatGPT` plan sign-in started from the prompt composer: a background
//! browser sign-in whose URL and result return through [`AppEvent`]s.

use std::sync::Arc;

use ag_session::ChatGptSignInOutcome;
use tokio::task::JoinHandle;

use crate::app::prompt_intent::PromptModelChange;
use crate::app::{App, AppEvent};
use crate::domain::agent::{AgentKind, AgentSelection, HarnessModelStatus};
use crate::infra::browser::BrowserClient;
use crate::presentation::app_mode::{AppMode, PromptModeSnapshot};
use crate::presentation::chatgpt_sign_in::{ChatGptSignInDialog, ChatGptSignInPhase};

/// Background sign-in progress delivered to the foreground reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChatGptSignInUpdate {
    /// The sign-in URL is ready for the user.
    Url {
        /// Request generation that produced the URL.
        request_id: u64,
        /// Browser URL that approves the sign-in.
        url: String,
    },
    /// The sign-in finished.
    Completed {
        /// Request generation that finished.
        request_id: u64,
        /// Signed-in account or a user-facing failure.
        result: Result<ChatGptSignInOutcome, String>,
    },
}

/// Injected browser boundary and ownership of the running sign-in.
pub(crate) struct ChatGptSignIn {
    browser: Arc<dyn BrowserClient>,
    next_request_id: u64,
    task: Option<JoinHandle<()>>,
}

impl ChatGptSignIn {
    /// Creates idle sign-in state that opens URLs with `browser`.
    pub(crate) fn new(browser: Arc<dyn BrowserClient>) -> Self {
        Self {
            browser,
            next_request_id: 1,
            task: None,
        }
    }

    /// Stops the running sign-in, releasing its callback listener.
    fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for ChatGptSignIn {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl App {
    /// Returns the session's harness model when it needs a `ChatGPT` sign-in
    /// before a turn can run.
    pub(crate) fn chatgpt_sign_in_required(&self, session_index: usize) -> Option<AgentSelection> {
        let selection = self.session_at(session_index)?.agent;
        if selection.kind() != AgentKind::Harness {
            return None;
        }

        self.services
            .harness_model_options()
            .iter()
            .any(|option| {
                option.model == selection.model()
                    && matches!(option.status, HarnessModelStatus::SignInRequired(_))
            })
            .then_some(selection)
    }

    /// Opens the sign-in popup for `selection` above the composer captured
    /// in `restore`.
    pub(crate) fn open_chatgpt_sign_in(
        &mut self,
        selection: AgentSelection,
        restore: PromptModeSnapshot,
    ) {
        self.mode = AppMode::ChatGptSignIn {
            dialog: Box::new(ChatGptSignInDialog::new(selection, restore)),
        };
    }

    /// Starts the browser sign-in in the background after the user confirms
    /// the popup. The URL opens in the browser and appears in the popup.
    pub(crate) fn start_chatgpt_sign_in(&mut self) {
        let AppMode::ChatGptSignIn { dialog } = &mut self.mode else {
            return;
        };
        if dialog.phase != ChatGptSignInPhase::Confirm {
            return;
        }
        self.chatgpt_sign_in.cancel();
        let request_id = self.chatgpt_sign_in.next_request_id;
        self.chatgpt_sign_in.next_request_id += 1;
        dialog.phase = ChatGptSignInPhase::Waiting {
            request_id,
            url: None,
        };
        let browser = Arc::clone(&self.chatgpt_sign_in.browser);
        let probe = self.services.harness_model_probe();
        let event_tx = self.services.event_sender();
        self.chatgpt_sign_in.task = Some(tokio::spawn(async move {
            let url_tx = event_tx.clone();
            let on_url = Box::new(move |url: String| {
                browser.open(&url);
                let _ = url_tx.send(AppEvent::ChatGptSignInUpdated {
                    update: ChatGptSignInUpdate::Url { request_id, url },
                });
            });
            let result = probe.sign_in_chatgpt(on_url).await;
            let _ = event_tx.send(AppEvent::ChatGptSignInUpdated {
                update: ChatGptSignInUpdate::Completed { request_id, result },
            });
        }));
    }

    /// Closes the popup, cancelling a running sign-in, and restores the
    /// composer that opened it.
    pub(crate) fn close_chatgpt_sign_in(&mut self) {
        match std::mem::replace(&mut self.mode, AppMode::List) {
            AppMode::ChatGptSignIn { dialog } => {
                self.chatgpt_sign_in.cancel();
                self.mode = dialog.restore.into_prompt_mode();
            }
            other_mode => self.mode = other_mode,
        }
    }

    /// Applies one background sign-in update while its popup is still
    /// waiting for it; updates from cancelled sign-ins are dropped.
    ///
    /// A successful sign-in switches the session to the chosen model, marks
    /// signed-in models ready, and lists the harness models again.
    pub(crate) async fn apply_chatgpt_sign_in_update(&mut self, update: ChatGptSignInUpdate) {
        let AppMode::ChatGptSignIn { dialog } = &mut self.mode else {
            return;
        };
        match update {
            ChatGptSignInUpdate::Url { request_id, url } => {
                if dialog.waiting_request_id() == Some(request_id) {
                    dialog.phase = ChatGptSignInPhase::Waiting {
                        request_id,
                        url: Some(url),
                    };
                }
            }
            ChatGptSignInUpdate::Completed { request_id, result } => {
                if dialog.waiting_request_id() != Some(request_id) {
                    return;
                }
                self.chatgpt_sign_in.task = None;
                let selection = dialog.selection;
                let session_id = dialog.session_id().clone();
                let message = match result {
                    Ok(outcome) => {
                        let account = outcome
                            .email
                            .unwrap_or_else(|| "your ChatGPT account".to_string());
                        let model = selection.model().as_str();
                        self.mark_chatgpt_models_signed_in();
                        let model_change = self
                            .update_prompt_session_model(&session_id, selection)
                            .await;
                        self.refresh_harness_models();
                        match model_change {
                            PromptModelChange::Switched => format!(
                                "Signed in as {account}. `{model}` now uses your ChatGPT plan."
                            ),
                            PromptModelChange::Deferred => format!(
                                "Signed in as {account}. This session switches to `{model}` on \
                                 your ChatGPT plan after its current turn and queued messages \
                                 finish."
                            ),
                            PromptModelChange::Failed => format!(
                                "Signed in as {account}, but switching this session to `{model}` \
                                 failed. Pick it again with /model."
                            ),
                        }
                    }
                    Err(error) => format!("Sign-in failed: {error}"),
                };
                self.finish_chatgpt_sign_in(message);
            }
        }
    }

    /// Shows `message` as the sign-in result while the popup is still open;
    /// the popup may have closed while the session model was switching.
    fn finish_chatgpt_sign_in(&mut self, message: String) {
        if let AppMode::ChatGptSignIn { dialog } = &mut self.mode {
            dialog.phase = ChatGptSignInPhase::Finished { message };
        }
    }

    /// Publishes the cached harness models with sign-in rows marked ready,
    /// so the composer accepts the signed-in model before the next listing
    /// completes.
    fn mark_chatgpt_models_signed_in(&self) {
        let options = self
            .services
            .harness_model_options()
            .into_iter()
            .map(|mut option| {
                if matches!(option.status, HarnessModelStatus::SignInRequired(_)) {
                    option.status = HarnessModelStatus::Ready;
                }

                option
            })
            .collect();
        let _ = self
            .services
            .event_sender()
            .send(AppEvent::HarnessModelsUpdated { options });
    }
}

#[cfg(test)]
#[path = "chatgpt_sign_in_test.rs"]
mod tests;
