//! Background workspace preparation for persisted conversation identities.

use std::path::{Path, PathBuf};

use super::{SessionTaskService, isolation, session_branch, session_folder};
use crate::app::cargo_cache::Announcement;
use crate::app::session::SessionError;
use crate::app::{AppEvent, AppServices, SessionManager, spawn};
use crate::domain::agent::{AgentKind, parse_persisted_session_agent_model};
use crate::domain::session::Status;
use crate::infra::db::{SessionPreparationRow, SessionPreparationState, SessionRow};

impl SessionManager {
    /// Prepares one reserved workspace and durably records success or failure.
    /// A canceled reservation returns [`SessionError::StoppedByUser`].
    pub(crate) async fn prepare_reserved_session(
        services: &AppServices,
        session_id: &str,
    ) -> Result<(), SessionError> {
        let preparation = services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
            .ok_or(SessionError::NotFound)?;
        if preparation.state == SessionPreparationState::Canceled {
            Self::cleanup_canceled_preparation(services, session_id);

            return Err(SessionError::StoppedByUser(
                "Workspace setup was canceled".to_string(),
            ));
        }
        if preparation.state != SessionPreparationState::Preparing {
            return Err(SessionError::Workflow(
                "Workspace setup is not active".to_string(),
            ));
        }
        let row = services
            .db()
            .sessions()
            .load_session(session_id)
            .await?
            .ok_or(SessionError::NotFound)?;
        let selection = parse_persisted_session_agent_model(Some(&row.agent), &row.model);
        let result = Self::prepare_workspace(services, &preparation, &row, |folder| {
            services.setup_session_backend(selection.kind(), folder)
        })
        .await;
        let error = result.as_ref().err().map(ToString::to_string);
        let state = if result.is_ok() {
            SessionPreparationState::Ready
        } else {
            SessionPreparationState::Failed
        };
        let applied = services
            .db()
            .sessions()
            .update_session_preparation(session_id, state, error.as_deref())
            .await?;
        if !applied {
            Self::cleanup_canceled_preparation(services, session_id);

            return Err(SessionError::StoppedByUser(
                "Workspace setup was canceled".to_string(),
            ));
        }
        if result.is_ok() {
            Self::warm_cargo_cache(services, session_id, selection.kind());
        }

        result
    }

    /// Starts refreshing the Cargo cache of a new Harness session's project
    /// silently in the background, as a head start for its first turn,
    /// whose own refresh waits for this one and tells the user what is
    /// still missing; other agents and disabled caches skip the work.
    fn warm_cargo_cache(services: &AppServices, session_id: &str, agent_kind: AgentKind) {
        let cargo_cache = services.cargo_cache();
        if agent_kind != AgentKind::Harness || !cargo_cache.is_enabled() {
            return;
        }
        let app_event_tx = services.event_sender();
        let fs_client = services.fs_client();
        let git_client = services.git_client();
        let folder = session_folder(services.base_path(), session_id);
        let session_id = session_id.to_string();
        spawn::spawn_traced(async move {
            let env = isolation::validate_session_worktree(
                fs_client.as_ref(),
                git_client.as_ref(),
                &folder,
                &session_id,
            )
            .await
            .ok();
            // A worktree that fails validation gets no refresh; its turns
            // report the failure themselves.
            let _refresh = env.and_then(|env| {
                cargo_cache.warm(
                    &app_event_tx,
                    &session_id,
                    agent_kind,
                    &env,
                    Announcement::Silent,
                )
            });
        });
    }

    /// Reclaims a canceled attempt, including a retry canceled before its
    /// worker starts and finds an existing checkout.
    fn cleanup_canceled_preparation(services: &AppServices, session_id: &str) {
        let folder = session_folder(services.base_path(), session_id);
        let has_worktree = services.fs_client().is_dir(folder.clone());
        Self::spawn_canceled_session_cleanup(
            services,
            folder,
            session_branch(session_id),
            has_worktree,
            session_id.to_string(),
        );
    }

    /// Materializes an owned checkout or validates one left by interrupted
    /// setup.
    async fn prepare_workspace<SetupError: std::fmt::Display>(
        services: &AppServices,
        preparation: &SessionPreparationRow,
        row: &SessionRow,
        setup: impl FnOnce(&Path) -> Result<(), SetupError>,
    ) -> Result<(), SessionError> {
        let session_id = &preparation.session_id;
        let project = services
            .db()
            .projects()
            .get_project(row.project_id.ok_or(SessionError::NotFound)?)
            .await?
            .ok_or(SessionError::NotFound)?;
        let folder = session_folder(services.base_path(), session_id);
        let fs_client = services.fs_client();
        let git_client = services.git_client();
        if fs_client.exists(folder.clone()) {
            isolation::validate_session_worktree(
                fs_client.as_ref(),
                git_client.as_ref(),
                &folder,
                session_id,
            )
            .await?;
        } else {
            let repo_root = git_client
                .find_git_repo_root(PathBuf::from(project.path))
                .await
                .ok_or_else(|| {
                    SessionError::Workflow("Failed to find git repository root".to_string())
                })?;
            Self::create_session_worktree(
                services,
                &folder,
                &repo_root,
                &session_branch(session_id),
                &preparation.start_ref,
            )
            .await?;
        }
        setup(&folder).map_err(|error| {
            SessionError::Workflow(format!("Failed to setup session backend: {error}"))
        })?;
        // Forks start review-ready with inherited commits; ordinary drafts
        // refresh their diff during execution instead.
        if row.status == Status::Review.to_string()
            && let Some(diff_stats) = SessionTaskService::refresh_persisted_session_diff_stats(
                services.db(),
                fs_client.as_ref(),
                git_client.as_ref(),
                session_id,
                &folder,
            )
            .await
        {
            services.emit_app_event(AppEvent::SessionDiffStatsUpdated {
                diff_stats,
                session_id: session_id.as_str().into(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "preparation_test.rs"]
mod tests;
