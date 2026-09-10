//! Prepared session creation and foreground completion routing.

use ag_session::{CreateSessionRequest, SessionError, SessionId};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::app::session::{SessionCreationKind, SessionCreationSettings};
use crate::app::{App, AppError, AppEvent, SessionManager};
use crate::domain::input::InputState;
use crate::domain::session::Status;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::SessionPreparationState;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState};

/// Creation inputs captured before external work releases the foreground.
pub(super) enum PreparedSessionCreation {
    Materialized {
        base_branch: String,
        creation_kind: SessionCreationKind,
        project_id: i64,
        settings: SessionCreationSettings,
    },
    Persisted(String),
}

/// Recipient and project captured when a creation request is accepted.
pub(crate) struct PendingSessionCreation {
    project_id: i64,
    response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    session_id: Option<String>,
}

impl App {
    /// Opens the composer after reserving metadata; checkout runs
    /// independently.
    pub(crate) async fn start_session_creation(
        &mut self,
        request: CreateSessionRequest,
        response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    ) {
        let request_id = Uuid::new_v4().to_string();
        let interactive = response_tx.is_none();
        self.pending_session_creations.insert(
            request_id.clone(),
            PendingSessionCreation {
                project_id: request.project_id,
                response_tx,
                session_id: None,
            },
        );
        let result = self.prepare_api_session_creation(request).await;
        let session_id = match result {
            Ok(PreparedSessionCreation::Materialized {
                base_branch,
                creation_kind,
                project_id,
                settings,
            }) => {
                match SessionManager::reserve_session(
                    &self.services,
                    project_id,
                    &base_branch,
                    settings,
                    creation_kind,
                )
                .await
                {
                    Ok(session_id) => session_id,
                    Err(error) => {
                        self.complete_session_creation(
                            &request_id,
                            Err(SessionError::Operation(error.to_string())),
                        )
                        .await;
                        return;
                    }
                }
            }
            Ok(PreparedSessionCreation::Persisted(session_id)) => {
                self.finish_api_session_creation(&session_id).await;
                if interactive {
                    self.open_created_session_composer(&session_id);
                }
                self.complete_session_creation(&request_id, Ok(session_id))
                    .await;
                return;
            }
            Err(error) => {
                self.complete_session_creation(&request_id, Err(error))
                    .await;
                return;
            }
        };
        self.finish_api_session_creation(&session_id).await;
        if interactive {
            self.open_created_session_composer(&session_id);
        }
        if let Some(pending) = self.pending_session_creations.get_mut(&request_id) {
            pending.session_id = Some(session_id.clone());
        }
        self.spawn_workspace_preparation(request_id, session_id);
    }

    /// Selects the newly reserved conversation once, before accepting input.
    fn open_created_session_composer(&mut self, session_id: &str) {
        let index = self
            .sessions
            .sessions()
            .iter()
            .position(|session| session.id == session_id);
        self.sessions.select_session_index(index);
        self.mode = AppMode::Prompt {
            at_mention_state: None,
            attachment_state: PromptAttachmentState::default(),
            focus: ChatFocus::Input,
            history_state: PromptHistoryState::new(Vec::new()),
            slash_state: self.prompt_slash_state(),
            session_id: session_id.into(),
            input: InputState::default(),
            scroll_offset: None,
        };
        self.mark_dirty();
    }

    /// Runs filesystem work without borrowing the foreground application.
    fn spawn_workspace_preparation(&self, request_id: String, session_id: String) {
        let services = self.services.clone();
        let completion_request_id = request_id.clone();
        let task = tokio::spawn(async move {
            let result = SessionManager::prepare_reserved_session(&services, &session_id)
                .await
                .map(|()| session_id)
                .map_err(|error| error.to_string());
            services.emit_app_event(AppEvent::SessionCreationCompleted {
                request_id: completion_request_id,
                result,
            });
        });
        self.services.track_session_creation_task(request_id, task);
    }

    /// Applies worker results without replacing a composer or navigation state.
    pub(crate) async fn complete_session_creations(
        &mut self,
        results: Vec<(String, Result<String, String>)>,
    ) {
        for (request_id, result) in results {
            self.complete_session_creation(&request_id, result.map_err(SessionError::Operation))
                .await;
        }
    }

    /// Returns the reserved identity to API callers even if setup failed,
    /// allowing them to link and retry that session, and dispatches ready
    /// turns.
    pub(crate) async fn complete_session_creation(
        &mut self,
        request_id: &str,
        result: Result<String, SessionError>,
    ) {
        self.services.finish_session_creation_task(request_id).await;
        let Some(pending) = self.pending_session_creations.remove(request_id) else {
            return;
        };
        if let Some(session_id) = &pending.session_id {
            self.refresh_workspace_preparation(session_id).await;
            if result.is_ok() && self.sessions.session_for_id(session_id).is_some() {
                self.dispatch_prepared_prompt(session_id).await;
            }
        }
        if let Some(response_tx) = pending.response_tx {
            let result = pending.session_id.map_or(result, Ok);
            let _ = response_tx.send(result.map(SessionId::from));
        } else if pending.session_id.is_none()
            && pending.project_id == self.active_project_id()
            && let Err(error) = result
        {
            self.mode = AppMode::SyncBlockedPopup {
                default_branch: None,
                is_loading: false,
                message: error.to_string(),
                project_name: None,
                title: "Session creation unavailable".to_string(),
            };
        }
        self.mark_dirty();
    }

    /// Reflects durable setup state without reloading unrelated sessions.
    pub(crate) async fn refresh_workspace_preparation(&mut self, session_id: &str) {
        if let Ok(preparation) = self
            .services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await
        {
            let ready = preparation
                .as_ref()
                .is_some_and(|row| row.state == SessionPreparationState::Ready);
            if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
                SessionManager::apply_workspace_preparation(session, preparation.as_ref());
            }
            self.sessions
                .set_session_worktree_available(session_id, ready);
        }
        self.mark_dirty();
    }

    /// Saves an early submission once; a second distinct prompt stays in its
    /// composer.
    pub(crate) async fn queue_preparation_prompt(
        &mut self,
        session_id: &str,
        prompt: &TurnPrompt,
    ) -> Result<bool, AppError> {
        let Some(preparation) = self
            .services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
        else {
            return Ok(false);
        };
        if preparation.state == SessionPreparationState::Canceled {
            return Err(AppError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }
        if preparation.state == SessionPreparationState::Ready && preparation.prompt.is_none() {
            return Ok(false);
        }
        let prompt_json =
            serde_json::to_string(prompt).map_err(|error| AppError::Workflow(error.to_string()))?;
        if let Some(saved) = &preparation.prompt {
            if saved != &prompt_json {
                return Err(AppError::Workflow(
                    "A first prompt is already saved. Wait for it to start before sending another."
                        .to_string(),
                ));
            }
        } else if !self
            .services
            .db()
            .sessions()
            .save_preparation_prompt(session_id, &prompt_json)
            .await?
        {
            return Err(AppError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }
        if preparation.state == SessionPreparationState::Failed {
            self.retry_workspace_preparation(session_id).await?;
        }
        self.refresh_workspace_preparation(session_id).await;

        Ok(true)
    }

    /// Retries failed setup or starts lazy draft preparation, retaining its
    /// prompt.
    pub(crate) async fn retry_workspace_preparation(
        &mut self,
        session_id: &str,
    ) -> Result<(), AppError> {
        if !self.sessions.can_retry_workspace_preparation(session_id) {
            return Err(AppError::Workflow(
                "The saved first prompt is already queued or running".to_string(),
            ));
        }
        if self
            .pending_session_creations
            .values()
            .any(|pending| pending.session_id.as_deref() == Some(session_id))
        {
            return Ok(());
        }
        let project_id = self
            .services
            .db()
            .sessions()
            .load_session_project_id(session_id)
            .await?
            .ok_or(AppError::Workflow("Session project is missing".to_string()))?;
        self.ensure_project_checkout_available(project_id)?;
        if !self
            .services
            .db()
            .sessions()
            .update_session_preparation(session_id, SessionPreparationState::Preparing, None)
            .await?
        {
            return Err(AppError::Workflow(
                "Workspace setup was canceled".to_string(),
            ));
        }
        let request_id = Uuid::new_v4().to_string();
        self.pending_session_creations.insert(
            request_id.clone(),
            PendingSessionCreation {
                project_id,
                response_tx: None,
                session_id: Some(session_id.to_string()),
            },
        );
        self.spawn_workspace_preparation(request_id, session_id.to_string());
        self.refresh_workspace_preparation(session_id).await;

        Ok(())
    }

    /// Resumes ready prompts when their owning project becomes active again.
    pub(crate) async fn resume_ready_workspace_prompts(&mut self) {
        let Ok(preparations) = self
            .services
            .db()
            .sessions()
            .load_session_preparations(self.active_project_id())
            .await
        else {
            return;
        };
        for preparation in preparations {
            if preparation.state == SessionPreparationState::Ready
                && preparation.prompt.is_some()
                && self
                    .sessions
                    .session_for_id(&preparation.session_id)
                    .is_some()
            {
                self.dispatch_prepared_prompt(&preparation.session_id).await;
            }
        }
    }

    /// Hands off a saved prompt only after readiness, then acknowledges
    /// persistence.
    async fn dispatch_prepared_prompt(&mut self, session_id: &str) {
        let result = self.run_prepared_prompt(session_id).await;
        if let Err(error) = result {
            let _ = self
                .services
                .db()
                .sessions()
                .update_session_preparation(
                    session_id,
                    SessionPreparationState::Failed,
                    Some(&error.to_string()),
                )
                .await;
        }
        self.refresh_workspace_preparation(session_id).await;
    }

    /// Reuses the ordinary first-turn and fork-reply paths after workspace
    /// setup.
    async fn run_prepared_prompt(&mut self, session_id: &str) -> Result<(), AppError> {
        let Some(preparation) = self
            .services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
        else {
            return Ok(());
        };
        if preparation.state != SessionPreparationState::Ready {
            return Ok(());
        }
        let Some(prompt_json) = preparation.prompt else {
            return Ok(());
        };
        let prompt = serde_json::from_str::<TurnPrompt>(&prompt_json)
            .map_err(|error| AppError::Workflow(error.to_string()))?;
        self.services
            .db()
            .sessions()
            .reclaim_preparation_prompt_operation(session_id)
            .await?;
        if self
            .recover_accepted_preparation_prompt(session_id, &prompt)
            .await?
        {
            return Ok(());
        }
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(AppError::Workflow("Session is not loaded".to_string()))?;
        let was_draft = session.is_draft_session();
        if was_draft && !self.sessions.can_start_staged_session(session_id) {
            return Err(AppError::Workflow(
                "The parent stack is no longer ready to start this session".to_string(),
            ));
        }
        if session.status == Status::Draft {
            self.sessions
                .start_session(&self.services, session_id, prompt)
                .await?;
        } else if session.status == Status::Review {
            if !self
                .sessions
                .reply_to_coordinator_message(
                    &self.services,
                    session_id,
                    format!("workspace:{session_id}"),
                    true,
                    prompt,
                )
                .await
            {
                return Err(AppError::Workflow(
                    "Could not start the saved prompt".to_string(),
                ));
            }
        } else {
            return Err(AppError::Workflow(
                "Check the existing turn before retrying the saved prompt".to_string(),
            ));
        }
        self.sessions
            .state_mut()
            .sync_session_from_handle(session_id);
        if was_draft {
            self.sessions
                .clear_started_draft_attachments(&self.services, session_id)
                .await;
        }
        Ok(())
    }

    /// Restores a saved prompt after an interrupted handoff without replaying
    /// it.
    async fn recover_accepted_preparation_prompt(
        &mut self,
        session_id: &str,
        prompt: &TurnPrompt,
    ) -> Result<bool, AppError> {
        if let Some(status) = self
            .services
            .db()
            .sessions()
            .preparation_prompt_operation_status(session_id)
            .await?
        {
            // A live queued command still owns its gate. Keep its saved
            // payload for restart recovery without queueing a duplicate.
            if status == "queued" {
                return Ok(true);
            }
            let text = prompt.transcript_text();
            let messages = self
                .services
                .db()
                .sessions()
                .load_session_messages(session_id)
                .await?;
            if !messages
                .iter()
                .any(|message| message.kind == "user_prompt" && message.content == text)
            {
                self.services
                    .db()
                    .sessions()
                    .append_session_message(
                        session_id,
                        ag_session::SessionMessageKind::UserPrompt,
                        &text,
                    )
                    .await?;
            }
            self.services
                .db()
                .sessions()
                .clear_preparation_prompt(session_id)
                .await?;
            self.sessions
                .load_session_detail_into_state(self.services.db(), session_id)
                .await;
            self.append_output_for_session(
                session_id,
                "Previous submission recovered. Check its turn outcome before sending again.",
            )
            .await;

            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "session_creation_test.rs"]
mod tests;
