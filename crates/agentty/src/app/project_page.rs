//! Session pages of listed projects that session-API callers and paired
//! devices view while the terminal shows another project.
//!
//! A request that names a listed project, or a session of one, loads that
//! project's page in the turn that serves it, beside the active page, and
//! renews its lease. Each page refreshes by its own fingerprint, its
//! project's git status is polled while it is loaded, and it is dropped
//! once its lease goes idle, when more pages are held than the bound
//! allows, when its project leaves the list, or when the last paired device
//! disconnects; the next request that names it loads it again. Automation
//! stays with the terminal's project: workflows that apply the active
//! project's settings check the active page, and sessions on a viewed page
//! take the paths that serve projects the terminal does not show. A caller
//! may also create sessions in, and sync, a viewed project, each from that
//! project's own checkout and settings.

use ag_session::{ProjectScope, SessionError as ApiSessionError};

use crate::app::command_gateway::CommandInitiator;
use crate::app::project::ProjectContext;
use crate::app::{App, SessionManager, review, sync};
use crate::domain::agent::AgentKind;

impl App {
    /// Resolves the project `scope` names for a request from `initiator`,
    /// whose page the request then reads.
    ///
    /// [`ProjectScope::Active`] means the terminal's active project when
    /// this turn serves the request. A request of a session-API caller or a
    /// paired device makes the page available, as
    /// [`Self::ensure_project_page`] does; the terminal reaches only a
    /// loaded page and renews no lease.
    ///
    /// # Errors
    /// Returns [`ApiSessionError::NotFound`] for a project the host does not
    /// list or, for the terminal, whose page is not loaded, or an operation
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
        if Self::leases_pages(initiator) {
            self.ensure_project_page(project_id).await?;
        } else if self.sessions.state().page_for_project(project_id).is_none() {
            return Err(ApiSessionError::NotFound);
        }

        Ok(project_id)
    }

    /// Returns the listed project whose loaded page lists `session_id`, or
    /// `None` for a session the host does not list.
    ///
    /// A session of a listed project whose page is not loaded has that page
    /// loaded first, in this turn; it is listed only when the loaded page
    /// holds it, among the project's active sessions and its archive
    /// window. A session of a project the host does not list, or of the
    /// active project but off its page, is not listed.
    ///
    /// # Errors
    /// Returns an operation error when the session's project cannot be read
    /// or its page cannot be loaded.
    pub(crate) async fn listed_session_project(
        &mut self,
        session_id: &str,
    ) -> Result<Option<i64>, ApiSessionError> {
        if self
            .sessions
            .state()
            .session_project_id(session_id)
            .is_none()
        {
            let project_id = self
                .services
                .db()
                .sessions()
                .load_session_project_id(session_id)
                .await
                .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
            let Some(project_id) = project_id.filter(|project_id| {
                *project_id != self.active_project_id() && self.projects.is_listed(*project_id)
            }) else {
                return Ok(None);
            };
            self.ensure_project_page(project_id).await?;
        }
        let projects = &self.projects;

        Ok(self
            .sessions
            .state()
            .session_project_id(session_id)
            .filter(|project_id| projects.is_listed(*project_id)))
    }

    /// Makes the session page of the listed project `project_id` available
    /// to a request of a session-API caller or a paired device and renews
    /// its lease.
    ///
    /// The active project's page is always loaded. Another listed project's
    /// page is loaded in this turn when it is not held yet: its branch is
    /// detected as a project switch detects it, its stored focused reviews
    /// join the review cache without replacing live ones, a sync of the
    /// project that finished while no page held it is reconciled, and the
    /// first prompts its sessions saved while no page held them start.
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
            self.resume_ready_workspace_prompts(project_id).await;
        }
        let now = self.services.clock().now_instant();
        self.sessions
            .state_mut()
            .renew_project_page(project_id, now);
        self.sessions.state_mut().evict_excess_viewed_pages();
        self.follow_viewed_pages();

        Ok(())
    }

    /// Renews the lease of the page holding `session_id` for a request of a
    /// session-API caller or a paired device, so a page stays loaded while
    /// a device works in it. The terminal's own actions, which reach only
    /// the active page, renew no lease.
    pub(crate) fn renew_session_page_lease(
        &mut self,
        initiator: CommandInitiator,
        session_id: &str,
    ) {
        if !Self::leases_pages(initiator) {
            return;
        }

        let now = self.services.clock().now_instant();
        self.sessions
            .state_mut()
            .renew_session_page(session_id, now);
    }

    /// Reloads each viewed page whose stored fingerprint changed since it
    /// last loaded, detecting its project's branch again, and returns
    /// whether any page reloaded. A reload hands the page's new polling
    /// targets to the background poll.
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
        if is_refreshed {
            self.publish_viewed_sync_contexts();
        }

        is_refreshed
    }

    /// Drops the viewed pages whose lease is idle at `now`.
    pub(crate) fn evict_idle_viewed_pages(&mut self, now: std::time::Instant) {
        self.sessions.state_mut().evict_idle_viewed_pages(now);
        self.follow_viewed_pages();
    }

    /// Drops the viewed pages of projects the host no longer lists.
    pub(crate) fn drop_unlisted_viewed_pages(&mut self) {
        let projects = &self.projects;
        self.sessions
            .state_mut()
            .retain_viewed_pages(|project_id| projects.is_listed(project_id));
        self.follow_viewed_pages();
    }

    /// Drops every viewed page, as when the last paired device disconnects.
    pub(crate) fn clear_viewed_pages(&mut self) {
        self.sessions.state_mut().clear_viewed_pages();
        self.follow_viewed_pages();
    }

    /// Starts a sync of the viewed project `project_id`, whose page the
    /// request loaded, in that project's own checkout.
    ///
    /// The sync runs on the branch checked out there now and refreshes the
    /// review requests of that page's sessions, and its conflicts are
    /// resolved with that project's default model, as
    /// [`Self::start_sync_main`] does for the active project.
    ///
    /// # Errors
    /// Returns [`ApiSessionError::NotFound`] when no viewed page of the
    /// project is loaded.
    pub(crate) async fn start_sync_main_for(
        &mut self,
        project_id: i64,
    ) -> Result<(), ApiSessionError> {
        let sync_context = self
            .viewed_project_sync_context(project_id)
            .await
            .ok_or(ApiSessionError::NotFound)?;
        let session_model = SessionManager::load_default_session_model(
            &self.services,
            Some(project_id),
            AgentKind::Antigravity.default_model(),
        )
        .await;
        self.start_sync_main_with(sync_context, session_model);

        Ok(())
    }

    /// Detects the branch checked out now in the viewed project
    /// `project_id`'s checkout, as loading its page does, rather than the
    /// branch detected then. Returns `None` when no viewed page of the
    /// project is loaded or no branch is checked out.
    pub(crate) async fn detect_viewed_project_branch(&self, project_id: i64) -> Option<String> {
        self.sessions.state().viewed_page(project_id)?;
        let working_dir = self
            .projects
            .context(project_id)?
            .working_dir()
            .to_path_buf();

        self.services
            .git_client()
            .detect_git_info(working_dir)
            .await
    }

    /// Builds the git-status polling context of the viewed project
    /// `project_id` from its context and viewed page: its checkout, the
    /// branch detected when the page loaded, and the page's sessions, with
    /// no review-request targets.
    pub(in crate::app) fn viewed_polling_context(
        &self,
        project_id: i64,
    ) -> Option<sync::SyncContext> {
        let context = self.projects.context(project_id)?;
        let page = self.sessions.state().viewed_page(project_id)?;

        Some(sync::SyncContext {
            run_client: self.services.run_client(),
            generation: 0,
            fs_client: self.services.fs_client(),
            git_client: self.services.git_client(),
            project_branch_name: context.git_branch().map(str::to_string),
            project_id,
            project_name: context.name().to_string(),
            review_request_client: self.services.review_request_client(),
            review_request_sync_targets: Vec::new(),
            session_git_status_targets: Self::page_git_status_targets(
                &self.sessions,
                page.sessions(),
            ),
            working_dir: context.working_dir().to_path_buf(),
        })
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

    /// Returns whether a request from `initiator` loads and leases the
    /// pages it names: every caller's but the terminal's, which shows only
    /// the active page.
    fn leases_pages(initiator: CommandInitiator) -> bool {
        initiator != CommandInitiator::SessionView
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

    /// Builds the context of one sync of the viewed project `project_id`:
    /// its polling context with the branch checked out there now instead of
    /// the branch detected when its page loaded, and its sessions' review
    /// requests, which the background poll leaves to the active project.
    async fn viewed_project_sync_context(&self, project_id: i64) -> Option<sync::SyncContext> {
        let project_branch_name = self.detect_viewed_project_branch(project_id).await;
        let page = self.sessions.state().viewed_page(project_id)?;

        Some(sync::SyncContext {
            project_branch_name,
            review_request_sync_targets: Self::page_review_request_sync_targets(page),
            ..self.viewed_polling_context(project_id)?
        })
    }

    /// Keeps the viewed project contexts, and the background poll of viewed
    /// projects' git status, in step with the viewed pages.
    fn follow_viewed_pages(&mut self) {
        let viewed_project_ids = self.sessions.state().viewed_project_ids();
        self.projects.retain_viewed_contexts(&viewed_project_ids);
        self.publish_viewed_sync_contexts();
    }
}

#[cfg(test)]
#[path = "project_page_test.rs"]
pub(super) mod tests;
