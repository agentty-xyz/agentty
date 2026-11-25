//! Session pages of listed projects that session-API callers view while the
//! terminal shows another project.
//!
//! A request that names a listed project loads that project's page in the
//! turn that serves it, beside the active page, and renews its lease. Each
//! page refreshes by its own fingerprint and is dropped once its lease goes
//! idle, when more pages are held than the bound allows, when its project
//! leaves the list, or when the last paired device disconnects. Automation
//! stays with the terminal's project: workflows that apply the active
//! project's settings check the active page, and sessions on a viewed page
//! take the paths that serve projects the terminal does not show.

use ag_session::{ProjectScope, SessionError as ApiSessionError};

use crate::app::command_gateway::CommandInitiator;
use crate::app::project::ProjectContext;
use crate::app::{App, review};

impl App {
    /// Resolves the project `scope` names for a request from `initiator`,
    /// whose page the request then reads.
    ///
    /// [`ProjectScope::Active`] means the terminal's active project when
    /// this turn serves the request. A session-API caller's request makes
    /// the page available, as [`Self::ensure_project_page`] does; other
    /// callers, the terminal and paired devices, reach only a loaded page
    /// and renew no lease.
    ///
    /// # Errors
    /// Returns [`ApiSessionError::NotFound`] for a project the host does not
    /// list or, for other callers, whose page is not loaded, or an operation
    /// error when its page cannot be loaded.
    pub(crate) async fn resolve_project_scope(
        &mut self,
        initiator: CommandInitiator,
        scope: ProjectScope,
    ) -> Result<i64, ApiSessionError> {
        let project_id = match scope {
            ProjectScope::Active => self.active_project_id(),
            ProjectScope::Project(project_id) => project_id,
        };
        if initiator == CommandInitiator::SessionApi {
            self.ensure_project_page(project_id).await?;
        } else if self.sessions.state().page_for_project(project_id).is_none() {
            return Err(ApiSessionError::NotFound);
        }

        Ok(project_id)
    }

    /// Makes the session page of the listed project `project_id` available
    /// to a session-API request and renews its lease.
    ///
    /// The active project's page is always loaded. Another listed project's
    /// page is loaded in this turn when it is not held yet: its branch is
    /// detected as a project switch detects it, its stored focused reviews
    /// join the review cache without replacing live ones, and a sync of the
    /// project that finished while no page held it is reconciled.
    ///
    /// # Errors
    /// Returns [`ApiSessionError::NotFound`] for a project the host does not
    /// list, or an operation error when its page cannot be loaded.
    pub(crate) async fn ensure_project_page(
        &mut self,
        project_id: i64,
    ) -> Result<(), ApiSessionError> {
        if !self.projects.is_listed(project_id) {
            return Err(ApiSessionError::NotFound);
        }
        let needs_load = project_id != self.active_project_id()
            && self.sessions.state().viewed_page(project_id).is_none();
        if needs_load {
            self.load_viewed_project_page(project_id).await?;
            self.hydrate_focused_review_cache(project_id).await;
            self.apply_pending_project_sync_completion(project_id).await;
        }
        let now = self.services.clock().now_instant();
        self.sessions
            .state_mut()
            .renew_project_page(project_id, now);
        self.sessions.state_mut().evict_excess_viewed_pages();
        self.forget_dropped_viewed_contexts();

        Ok(())
    }

    /// Renews the lease of the page holding `session_id` for a request of a
    /// session-API caller. The terminal's own actions and paired devices,
    /// which reach only the active page, renew no lease.
    pub(crate) fn renew_session_page_lease(
        &mut self,
        initiator: CommandInitiator,
        session_id: &str,
    ) {
        if initiator != CommandInitiator::SessionApi {
            return;
        }

        let now = self.services.clock().now_instant();
        self.sessions
            .state_mut()
            .renew_session_page(session_id, now);
    }

    /// Reloads each viewed page whose stored fingerprint changed since it
    /// last loaded, detecting its project's branch again, and returns
    /// whether any page reloaded.
    pub(crate) async fn refresh_viewed_pages(&mut self) -> bool {
        let mut is_refreshed = false;
        for project_id in self.sessions.state().viewed_project_ids() {
            let sessions_metadata = self
                .services
                .db()
                .sessions()
                .load_sessions_metadata(project_id)
                .await
                .ok();
            // A failed read leaves the page as it is until the next check.
            let is_current = sessions_metadata.is_none_or(|sessions_metadata| {
                self.sessions
                    .state()
                    .viewed_page(project_id)
                    .is_some_and(|page| page.sessions_metadata() == sessions_metadata)
            });
            if is_current {
                continue;
            }

            is_refreshed |= self.load_viewed_project_page(project_id).await.is_ok();
        }

        is_refreshed
    }

    /// Drops the viewed pages whose lease is idle at `now`.
    pub(crate) fn evict_idle_viewed_pages(&mut self, now: std::time::Instant) {
        self.sessions.state_mut().evict_idle_viewed_pages(now);
        self.forget_dropped_viewed_contexts();
    }

    /// Drops the viewed pages of projects the host no longer lists.
    pub(crate) fn drop_unlisted_viewed_pages(&mut self) {
        let projects = &self.projects;
        self.sessions
            .state_mut()
            .retain_viewed_pages(|project_id| projects.is_listed(project_id));
        self.forget_dropped_viewed_contexts();
    }

    /// Drops every viewed page, as when the last paired device disconnects.
    pub(crate) fn clear_viewed_pages(&mut self) {
        self.sessions.state_mut().clear_viewed_pages();
        self.forget_dropped_viewed_contexts();
    }

    /// Adds the stored focused reviews of `project_id`'s sessions to the
    /// review cache, keeping the entries it already holds, and shows them
    /// on the loaded pages.
    pub(in crate::app) async fn hydrate_focused_review_cache(&mut self, project_id: i64) {
        for (session_id, persisted_review) in
            Self::load_focused_review_cache(self.services.db(), project_id).await
        {
            self.review_cache
                .entry(session_id)
                .or_insert(persisted_review);
        }
        review::hydrate_review_transients(&self.review_cache, self.sessions.state_mut());
    }

    /// Loads or reloads the viewed page of the switcher's project
    /// `project_id`, and records its context with the branch detected now.
    ///
    /// # Errors
    /// Returns [`ApiSessionError::NotFound`] when the switcher does not
    /// offer the project, or an operation error when its sessions cannot be
    /// loaded.
    async fn load_viewed_project_page(&mut self, project_id: i64) -> Result<(), ApiSessionError> {
        let project = self
            .projects
            .listed_project(project_id)
            .map(|project_item| project_item.project.clone())
            .ok_or(ApiSessionError::NotFound)?;
        let git_branch = self
            .services
            .git_client()
            .detect_git_info(project.path.clone())
            .await;
        if !self
            .sessions
            .load_viewed_page(&self.services, project_id, &project.path)
            .await
        {
            return Err(ApiSessionError::Operation(format!(
                "Could not load the sessions of project `{}`",
                project.display_label()
            )));
        }
        self.projects.set_viewed_context(ProjectContext::new(
            project_id,
            project.display_label(),
            git_branch,
            None,
            project.path,
        ));

        Ok(())
    }

    /// Keeps the viewed project contexts in step with the viewed pages.
    fn forget_dropped_viewed_contexts(&mut self) {
        let viewed_project_ids = self.sessions.state().viewed_project_ids();
        self.projects.retain_viewed_contexts(&viewed_project_ids);
    }
}

#[cfg(test)]
#[path = "project_page_test.rs"]
pub(super) mod tests;
