//! Session refresh scheduling and post-reload view state restoration.

use std::time::Instant;

use super::SESSION_REFRESH_INTERVAL;
use crate::app::App;
use crate::model::AppMode;

impl App {
    /// Reloads session rows when the metadata cache indicates a change.
    ///
    /// This is a low-frequency fallback safety poll; primary refreshes should
    /// come from explicit [`AppEvent::RefreshSessions`] events.
    pub async fn refresh_sessions_if_needed(&mut self) {
        if !self.is_session_refresh_due() {
            return;
        }

        self.session_state.refresh_deadline = Instant::now() + SESSION_REFRESH_INTERVAL;

        let Ok(sessions_metadata) = self.db.load_sessions_metadata().await else {
            return;
        };
        let (sessions_row_count, sessions_updated_at_max) = sessions_metadata;
        if sessions_row_count == self.session_state.row_count
            && sessions_updated_at_max == self.session_state.updated_at_max
        {
            return;
        }

        self.reload_sessions(Some(sessions_metadata)).await;
    }

    /// Reloads sessions immediately, bypassing refresh deadline checks.
    pub(crate) async fn refresh_sessions_now(&mut self) {
        let sessions_metadata = self.db.load_sessions_metadata().await.ok();
        self.reload_sessions(sessions_metadata).await;
        self.session_state.refresh_deadline = Instant::now() + SESSION_REFRESH_INTERVAL;
    }

    async fn reload_sessions(&mut self, sessions_metadata: Option<(i64, i64)>) {
        let selected_index = self.session_state.table_state.selected();
        let selected_session_id = selected_index
            .and_then(|index| self.session_state.sessions.get(index))
            .map(|session| session.id.clone());

        self.session_state.sessions = Self::load_sessions(
            &self.base_path,
            &self.db,
            &self.projects,
            &mut self.session_state.handles,
        )
        .await;
        self.start_pr_polling_for_pull_request_sessions();
        self.restore_table_selection(selected_session_id.as_deref(), selected_index);
        self.ensure_mode_session_exists();
        let active_session_ids: std::collections::HashSet<String> = self
            .session_state
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        self.session_workers
            .retain(|session_id, _| active_session_ids.contains(session_id));

        if let Some((sessions_row_count, sessions_updated_at_max)) = sessions_metadata {
            self.session_state.row_count = sessions_row_count;
            self.session_state.updated_at_max = sessions_updated_at_max;
        } else {
            self.update_sessions_metadata_cache().await;
        }
    }

    /// Returns `true` when periodic session refresh should run.
    fn is_session_refresh_due(&self) -> bool {
        Instant::now() >= self.session_state.refresh_deadline
    }

    /// Restores table selection after session list reload.
    fn restore_table_selection(
        &mut self,
        selected_session_id: Option<&str>,
        selected_index: Option<usize>,
    ) {
        if self.session_state.sessions.is_empty() {
            self.session_state.table_state.select(None);

            return;
        }

        if let Some(session_id) = selected_session_id
            && let Some(index) = self
                .session_state
                .sessions
                .iter()
                .position(|session| session.id == session_id)
        {
            self.session_state.table_state.select(Some(index));

            return;
        }

        let restored_index =
            selected_index.map(|index| index.min(self.session_state.sessions.len() - 1));
        self.session_state.table_state.select(restored_index);
    }

    /// Switches back to list mode if the currently viewed session is missing.
    fn ensure_mode_session_exists(&mut self) {
        let mode_session_id = match &self.mode {
            AppMode::Prompt { session_id, .. }
            | AppMode::View { session_id, .. }
            | AppMode::Diff { session_id, .. } => Some(session_id),
            _ => None,
        };
        let Some(session_id) = mode_session_id else {
            return;
        };
        if self.session_index_for_id(session_id).is_none() {
            self.mode = AppMode::List;
        }
    }

    /// Refreshes cached session metadata used by incremental list reloads.
    pub(super) async fn update_sessions_metadata_cache(&mut self) {
        if let Ok((sessions_row_count, sessions_updated_at_max)) =
            self.db.load_sessions_metadata().await
        {
            self.session_state.row_count = sessions_row_count;
            self.session_state.updated_at_max = sessions_updated_at_max;
        }
    }
}
